//! macOS: Servo's IOSurface, imported as a `VkImage` through MoltenVK.
//!
//! macOS defaults to Godot's Metal driver, which [`crate::bridge::metal`]
//! already covers. This is for the other configuration — Vulkan on top of
//! MoltenVK — where the same IOSurface has to reach a `VkImage` instead of an
//! `MTLTexture`.
//!
//! `VK_EXT_metal_objects` takes the IOSurface directly:
//! `VkImportMetalIOSurfaceInfoEXT` on the `VkImageCreateInfo` chain leaves the
//! image backed by that surface, so there is no allocation to make and nothing
//! to bind. Servo already draws into it, which makes this the one path with no
//! per-frame transfer at all — and, for the same reason, the one that arrives
//! upside down.

use std::ffi::CStr;

use ash::vk;
use dpi::PhysicalSize;
use glow::HasContext;
use godot::classes::rendering_device::DataFormat;
use surfman::cgl::surface::NativeSurface;

use super::device::VulkanDevice;
use crate::rendering_context::GodotRenderingContext;

pub struct SharedImage {
    /// Holds the IOSurface reference. Dropping it would pull the storage out
    /// from under the image below.
    _native_surface: NativeSurface,
    image: vk::Image,
}

impl SharedImage {
    pub const BACKEND_NAME: &'static str = "vulkan-iosurface";
    pub const REQUIRED_EXTENSIONS: &'static [&'static str] = &["VK_EXT_metal_objects"];
    /// The import is a struct on the `VkImageCreateInfo` chain rather than a
    /// call, so the enabled list is the only thing there is to check.
    pub const REQUIRED_FUNCTIONS: &'static [&'static CStr] = &[];
    /// The IOSurface surfman creates is `kCVPixelFormatType_32BGRA`.
    pub const GODOT_FORMAT: DataFormat = DataFormat::B8G8R8A8_UNORM;
    /// Nothing transfers, so there is nowhere to correct GL's bottom-left origin.
    pub const NEEDS_V_FLIP: bool = true;

    pub fn new(
        vulkan: &VulkanDevice,
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        // Take the IOSurface out of the bound surface. That needs a `&Surface`,
        // so unbind and rebind once. The same surface is used from here on, so
        // the image created below stays valid for the whole lifetime.
        let native_surface = context
            .with_unbound_surface(|device, surface| device.native_surface(surface))
            .map_err(|error| format!("failed to take the surfman surface: {error:?}"))?;

        // SAFETY: the surface is live and held for as long as the image is.
        let image = unsafe { import(vulkan, &native_surface, size)? };

        Ok(Self {
            _native_surface: native_surface,
            image,
        })
    }

    pub fn vk_image(&self) -> vk::Image {
        self.image
    }

    /// Both sides look at the same memory, so there is nothing to transfer.
    /// Just flush the outstanding GL work.
    pub fn update(
        &mut self,
        context: &GodotRenderingContext,
        _size: PhysicalSize<u32>,
    ) -> Result<(), String> {
        // SAFETY: Servo's context is current.
        unsafe { context.glow().flush() };
        Ok(())
    }

    pub fn destroy(&mut self, vulkan: &VulkanDevice, _context: &GodotRenderingContext) {
        // SAFETY: the image was created below, the caller has waited for the
        // device, and it is nulled out so a second call cannot double free. The
        // IOSurface behind it belongs to surfman and outlives this.
        unsafe {
            if self.image != vk::Image::null() {
                vulkan.device.destroy_image(self.image, None);
                self.image = vk::Image::null();
            }
        }
    }
}

/// Create a `VkImage` backed by Servo's IOSurface.
///
/// # Safety
///
/// `native_surface` must outlive the returned image.
unsafe fn import(
    vulkan: &VulkanDevice,
    native_surface: &NativeSurface,
    size: PhysicalSize<u32>,
) -> Result<vk::Image, String> {
    // `NativeSurface` holds a `CFRetained<IOSurfaceRef>`; Vulkan wants the bare
    // pointer, which `vk::IOSurfaceRef` is an alias for.
    let io_surface =
        core::ptr::from_ref(&*native_surface.0) as *const core::ffi::c_void as vk::IOSurfaceRef;

    let mut import = vk::ImportMetalIOSurfaceInfoEXT::default().io_surface(io_surface);
    let create_info = vk::ImageCreateInfo::default()
        .push_next(&mut import)
        .image_type(vk::ImageType::TYPE_2D)
        .format(vk::Format::B8G8R8A8_UNORM)
        .extent(vk::Extent3D {
            width: size.width,
            height: size.height,
            depth: 1,
        })
        .mip_levels(1)
        .array_layers(1)
        .samples(vk::SampleCountFlags::TYPE_1)
        .tiling(vk::ImageTiling::OPTIMAL)
        // `TRANSFER_SRC` because Godot copies out of this image every frame; see
        // `crate::bridge::create_owned_rd_texture`.
        .usage(vk::ImageUsageFlags::SAMPLED | vk::ImageUsageFlags::TRANSFER_SRC)
        .sharing_mode(vk::SharingMode::EXCLUSIVE)
        .initial_layout(vk::ImageLayout::UNDEFINED);

    // No `vkAllocateMemory` and no `vkBindImageMemory`: an image created with
    // `VkImportMetalIOSurfaceInfoEXT` already has its storage.
    vulkan
        .device
        .create_image(&create_info, None)
        .map_err(|error| format!("vkCreateImage over the IOSurface: {error}"))
}
