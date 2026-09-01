//! Windows: an ANGLE D3D11 texture, imported as a `VkImage`.
//!
//! Servo draws through ANGLE, which is D3D11, and `VK_KHR_external_memory_win32`
//! takes a DXGI NT handle directly. The shared texture is therefore the same one
//! the D3D12 path allocates — see [`crate::bridge::angle`] — with only the far
//! side swapped: `vkAllocateMemory` with a dedicated import instead of
//! `ID3D12Device::OpenSharedHandle`.

use std::ffi::CStr;

use ash::vk;
use dpi::PhysicalSize;
use godot::classes::rendering_device::DataFormat;
use windows::Win32::Foundation::{CloseHandle, HANDLE};
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::device::VulkanDevice;
use crate::bridge::angle;
use crate::rendering_context::GodotRenderingContext;

/// The handle flavour DXGI's `SHARED_NTHANDLE` textures import as.
const HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags =
    vk::ExternalMemoryHandleTypeFlags::D3D11_TEXTURE;

pub struct SharedImage {
    /// The shared texture on ANGLE's D3D11 device. Servo blits into this one.
    d3d11_texture: ID3D11Texture2D,
    /// The same memory, as an image on Godot's Vulkan device.
    image: vk::Image,
    memory: vk::DeviceMemory,
}

impl SharedImage {
    pub const BACKEND_NAME: &'static str = "vulkan-d3d11-shared-nt-handle";
    pub const REQUIRED_EXTENSIONS: &'static [&'static str] = &["VK_KHR_external_memory_win32"];
    pub const REQUIRED_FUNCTIONS: &'static [&'static CStr] =
        &[c"vkGetMemoryWin32HandlePropertiesKHR"];
    pub const GODOT_FORMAT: DataFormat = DataFormat::R8G8B8A8_UNORM;
    pub const NEEDS_V_FLIP: bool = false;

    pub fn new(
        vulkan: &VulkanDevice,
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let d3d11_device = angle::d3d11_device(context)?;

        // SAFETY: the device is live, and the handle is closed once Vulkan has
        // imported it, whether or not that succeeded.
        unsafe {
            let (d3d11_texture, handle) = angle::create_shared_texture(&d3d11_device, size)?;
            let imported = import(vulkan, handle, size);
            // Importing does not transfer ownership of a Win32 handle, so this
            // duplicate is ours to close either way.
            let _ = CloseHandle(handle);

            let (image, memory) = imported?;
            Ok(Self {
                d3d11_texture,
                image,
                memory,
            })
        }
    }

    pub fn vk_image(&self) -> vk::Image {
        self.image
    }

    pub fn update(
        &mut self,
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<(), String> {
        angle::blit_into(context, &self.d3d11_texture, size)
    }

    pub fn destroy(&mut self, vulkan: &VulkanDevice, _context: &GodotRenderingContext) {
        // SAFETY: both handles were created below, the caller has waited for the
        // device, and they are nulled out so a second call cannot double free.
        unsafe {
            if self.image != vk::Image::null() {
                vulkan.device.destroy_image(self.image, None);
                self.image = vk::Image::null();
            }
            if self.memory != vk::DeviceMemory::null() {
                vulkan.device.free_memory(self.memory, None);
                self.memory = vk::DeviceMemory::null();
            }
        }
    }
}

/// Create a `VkImage` over the memory behind a DXGI NT handle.
///
/// # Safety
///
/// `handle` must be a DXGI NT handle onto an `R8G8B8A8_UNORM` texture of `size`.
unsafe fn import(
    vulkan: &VulkanDevice,
    handle: HANDLE,
    size: PhysicalSize<u32>,
) -> Result<(vk::Image, vk::DeviceMemory), String> {
    let mut external = vk::ExternalMemoryImageCreateInfo::default().handle_types(HANDLE_TYPE);
    let create_info = vk::ImageCreateInfo::default()
        .push_next(&mut external)
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::R8G8B8A8_UNORM)
        .extent(vk::Extent3D {
            width: size.width,
            height: size.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        .usage(
            vk::ImageUsageFlags::SAMPLED
                | vk::ImageUsageFlags::TRANSFER_SRC
                | vk::ImageUsageFlags::COLOR_ATTACHMENT,
        )
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    let image = vulkan
        .device
        .create_image(&create_info, None)
        .map_err(|error| format!("vkCreateImage for the shared texture: {error}"))?;

    let memory = match allocate(vulkan, image, handle) {
        Ok(memory) => memory,
        Err(error) => {
            vulkan.device.destroy_image(image, None);
            return Err(error);
        }
    };

    if let Err(error) = vulkan.device.bind_image_memory(image, memory, 0) {
        vulkan.device.free_memory(memory, None);
        vulkan.device.destroy_image(image, None);
        return Err(format!("vkBindImageMemory for the shared texture: {error}"));
    }
    Ok((image, memory))
}

/// Import the handle's memory, dedicated to `image`.
///
/// # Safety
///
/// `image` must be a live image on `vulkan`, and `handle` its DXGI NT handle.
unsafe fn allocate(
    vulkan: &VulkanDevice,
    image: vk::Image,
    handle: HANDLE,
) -> Result<vk::DeviceMemory, String> {
    let external_memory =
        ash::khr::external_memory_win32::Device::new(&vulkan.instance, &vulkan.device);

    // The driver decides which memory types a given handle can be imported into.
    let mut handle_properties = vk::MemoryWin32HandlePropertiesKHR::default();
    external_memory
        .get_memory_win32_handle_properties(
            HANDLE_TYPE,
            handle.0 as vk::HANDLE,
            &mut handle_properties,
        )
        .map_err(|error| format!("vkGetMemoryWin32HandlePropertiesKHR: {error}"))?;

    let requirements = vulkan.device.get_image_memory_requirements(image);
    let allowed = requirements.memory_type_bits & handle_properties.memory_type_bits;
    let memory_type = vulkan.memory_type_index(allowed)?;

    // A D3D11 texture is one allocation holding one image, which is what a
    // dedicated allocation describes. The specification requires it here.
    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let mut import = vk::ImportMemoryWin32HandleInfoKHR::default()
        .handle_type(HANDLE_TYPE)
        .handle(handle.0 as vk::HANDLE);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type)
        .push_next(&mut dedicated)
        .push_next(&mut import);

    vulkan
        .device
        .allocate_memory(&allocate_info, None)
        .map_err(|error| format!("vkAllocateMemory importing the shared texture: {error}"))
}
