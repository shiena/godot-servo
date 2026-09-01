//! Linux and Android: a `VkImage` exported as an opaque fd, imported into GL.
//!
//! Both platforms run the sharing backwards from Windows and macOS: Vulkan
//! allocates and GL imports. Surfman keeps the `EGLImage` behind its Linux
//! surfaces `pub(crate)`, so there is nothing on Servo's side to export, and the
//! obvious containers to allocate instead — a dma-buf, an `AHardwareBuffer` —
//! both need a device extension Godot does not enable.
//!
//! An opaque fd needs none. Godot registers `VK_KHR_external_memory_fd` on every
//! platform already (`_register_requested_device_extension`, for a reason
//! unrelated to this: without it some runtime components fill the validation
//! layers with noise), `VK_KHR_external_memory` is core in 1.1, and the GL side
//! is `GL_EXT_memory_object_fd`, which Mesa, the NVIDIA driver and Adreno all
//! advertise. So this path asks nothing of the project settings:
//!
//! 1. Create an exportable `VkImage` with a dedicated allocation
//! 2. `vkGetMemoryFdKHR` for an `OPAQUE_FD` onto that allocation
//! 3. `glImportMemoryFdEXT` into a GL memory object, also flagged dedicated
//! 4. `glTexStorageMem2DEXT` for a texture over the same memory, tiled to match
//! 5. Blit Servo's FBO into that texture every frame
//!
//! Both sides being the same driver is what makes step 4 work without any
//! agreement about layout: `GL_OPTIMAL_TILING_EXT` and
//! `VK_IMAGE_TILING_OPTIMAL` are then the same thing, which is also why this
//! needs no linear fallback.
//!
//! The shape is not invented here: `godot-xreal` arrived at it after an
//! `AHardwareBuffer` import failed on device, and runs it on an Adreno 710.

use std::ffi::{c_void, CStr};
use std::sync::Arc;

use ash::vk;
use dpi::PhysicalSize;
use glow::HasContext;
use godot::classes::rendering_device::DataFormat;

use super::device::VulkanDevice;
use crate::rendering_context::GodotRenderingContext;

/// The handle flavour the memory is exported and imported as.
const HANDLE_TYPE: vk::ExternalMemoryHandleTypeFlags = vk::ExternalMemoryHandleTypeFlags::OPAQUE_FD;

// GL_EXT_memory_object / GL_EXT_memory_object_fd tokens.
const GL_TEXTURE_TILING_EXT: u32 = 0x9580;
const GL_DEDICATED_MEMORY_OBJECT_EXT: u32 = 0x9581;
const GL_OPTIMAL_TILING_EXT: i32 = 0x9584;
const GL_HANDLE_TYPE_OPAQUE_FD_EXT: u32 = 0x9586;

/// The `GL_EXT_memory_object` entry points, resolved through surfman.
///
/// Not in `glow`, which covers core GL only.
struct MemoryObjectExt {
    create: unsafe extern "C" fn(i32, *mut u32),
    delete: unsafe extern "C" fn(i32, *const u32),
    parameteriv: unsafe extern "C" fn(u32, u32, *const i32),
    import_fd: unsafe extern "C" fn(u32, u64, u32, i32),
    tex_storage_2d: unsafe extern "C" fn(u32, i32, u32, i32, i32, u32, u64),
}

impl MemoryObjectExt {
    fn load(context: &GodotRenderingContext) -> Result<Self, String> {
        let device = context.device();
        let gl_context = context.context_mut();
        let require = |name: &str| -> Result<*const c_void, String> {
            let pointer = device.get_proc_address(&gl_context, name);
            if pointer.is_null() {
                return Err(format!(
                    "{name} is not available; this GL driver has no GL_EXT_memory_object_fd"
                ));
            }
            Ok(pointer)
        };

        // SAFETY: the extension's entry points, transmuted to the signatures the
        // specification gives them.
        unsafe {
            Ok(Self {
                create: std::mem::transmute(require("glCreateMemoryObjectsEXT")?),
                delete: std::mem::transmute(require("glDeleteMemoryObjectsEXT")?),
                parameteriv: std::mem::transmute(require("glMemoryObjectParameterivEXT")?),
                import_fd: std::mem::transmute(require("glImportMemoryFdEXT")?),
                tex_storage_2d: std::mem::transmute(require("glTexStorageMem2DEXT")?),
            })
        }
    }
}

pub struct SharedImage {
    image: vk::Image,
    memory: vk::DeviceMemory,
    ext: MemoryObjectExt,
    /// Held for teardown, alongside the memory object and texture below.
    gl: Arc<glow::Context>,
    memory_object: u32,
    /// The GL texture Servo blits into, over the same memory as `image`.
    gl_texture: glow::Texture,
}

impl SharedImage {
    pub const BACKEND_NAME: &'static str = "vulkan-opaque-fd";
    /// Nothing. Godot enables `VK_KHR_external_memory_fd` of its own accord, so
    /// this path needs no project setting and works on a stock Godot.
    pub const REQUIRED_EXTENSIONS: &'static [&'static str] = &[];
    /// Whether it is really there is decided by the device, not the list.
    pub const REQUIRED_FUNCTIONS: &'static [&'static CStr] = &[c"vkGetMemoryFdKHR"];
    pub const GODOT_FORMAT: DataFormat = DataFormat::R8G8B8A8_UNORM;
    pub const NEEDS_V_FLIP: bool = false;

    pub fn new(
        vulkan: &VulkanDevice,
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let ext = MemoryObjectExt::load(context)?;
        context
            .make_current_public()
            .map_err(|error| format!("make_current failed: {error:?}"))?;

        // SAFETY: a straight sequence of Vulkan and GL calls with Servo's context
        // current. Everything already allocated is released before returning on
        // failure, except the fd, which the GL import consumes either way.
        unsafe {
            let (image, memory, size_bytes) = allocate(vulkan, size)?;

            let fd = match export_fd(vulkan, memory) {
                Ok(fd) => fd,
                Err(error) => {
                    vulkan.device.free_memory(memory, None);
                    vulkan.device.destroy_image(image, None);
                    return Err(error);
                }
            };

            match import_into_gl(&ext, context.glow(), fd, size_bytes, size) {
                Ok((memory_object, gl_texture)) => Ok(Self {
                    image,
                    memory,
                    ext,
                    gl: context.glow().clone(),
                    memory_object,
                    gl_texture,
                }),
                Err(error) => {
                    vulkan.device.free_memory(memory, None);
                    vulkan.device.destroy_image(image, None);
                    Err(error)
                }
            }
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
        let source_fbo = context.framebuffer();
        // SAFETY: Servo's context is current and the texture is alive.
        unsafe { crate::bridge::blit_flipped(context.glow(), source_fbo, self.gl_texture, size) }
    }

    pub fn destroy(&mut self, vulkan: &VulkanDevice, context: &GodotRenderingContext) {
        // The GL objects can only be deleted through the context that made them.
        let _ = context.make_current_public();

        // SAFETY: everything here was created above, and is nulled out so a
        // second call cannot double free.
        unsafe {
            if self.memory_object != 0 {
                self.gl.delete_texture(self.gl_texture);
                (self.ext.delete)(1, &self.memory_object);
                self.memory_object = 0;
            }
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

/// Create an image whose allocation can leave the device, and its size in bytes.
///
/// # Safety
///
/// `vulkan` must be live.
unsafe fn allocate(
    vulkan: &VulkanDevice,
    size: PhysicalSize<u32>,
) -> Result<(vk::Image, vk::DeviceMemory, u64), String> {
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
        // Optimal on both sides, which `GL_OPTIMAL_TILING_EXT` matches below.
        .tiling(vk::ImageTiling::OPTIMAL)
        // `TRANSFER_SRC` because Godot copies out of this image every frame; see
        // `crate::bridge::create_owned_rd_texture`.
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
        .map_err(|error| format!("vkCreateImage for the shared image: {error}"))?;

    let requirements = vulkan.device.get_image_memory_requirements(image);
    let allocated = allocate_memory(vulkan, image, &requirements);
    let memory = match allocated {
        Ok(memory) => memory,
        Err(error) => {
            vulkan.device.destroy_image(image, None);
            return Err(error);
        }
    };

    if let Err(error) = vulkan.device.bind_image_memory(image, memory, 0) {
        vulkan.device.free_memory(memory, None);
        vulkan.device.destroy_image(image, None);
        return Err(format!("vkBindImageMemory for the shared image: {error}"));
    }
    Ok((image, memory, requirements.size))
}

/// The exportable, dedicated allocation behind `image`.
///
/// # Safety
///
/// `image` must be a live image on `vulkan`.
unsafe fn allocate_memory(
    vulkan: &VulkanDevice,
    image: vk::Image,
    requirements: &vk::MemoryRequirements,
) -> Result<vk::DeviceMemory, String> {
    if requirements.size == 0 {
        return Err("vkGetImageMemoryRequirements reported a zero-sized image".into());
    }
    let memory_type = vulkan.memory_type_index(requirements.memory_type_bits)?;

    let mut dedicated = vk::MemoryDedicatedAllocateInfo::default().image(image);
    let mut export = vk::ExportMemoryAllocateInfo::default().handle_types(HANDLE_TYPE);
    let allocate_info = vk::MemoryAllocateInfo::default()
        .allocation_size(requirements.size)
        .memory_type_index(memory_type)
        .push_next(&mut dedicated)
        .push_next(&mut export);

    vulkan
        .device
        .allocate_memory(&allocate_info, None)
        .map_err(|error| format!("vkAllocateMemory for the shared image: {error}"))
}

/// Export the allocation as an fd.
///
/// # Safety
///
/// `memory` must be an exportable allocation on `vulkan`.
unsafe fn export_fd(vulkan: &VulkanDevice, memory: vk::DeviceMemory) -> Result<i32, String> {
    let external_memory =
        ash::khr::external_memory_fd::Device::new(&vulkan.instance, &vulkan.device);
    let fd = external_memory
        .get_memory_fd(
            &vk::MemoryGetFdInfoKHR::default()
                .memory(memory)
                .handle_type(HANDLE_TYPE),
        )
        .map_err(|error| format!("vkGetMemoryFdKHR: {error}"))?;
    if fd < 0 {
        return Err("vkGetMemoryFdKHR succeeded but handed back no descriptor".into());
    }
    Ok(fd)
}

/// Build a GL texture over the exported allocation.
///
/// Takes the descriptor: `glImportMemoryFdEXT` consumes it whether or not the
/// import succeeds, so there is nothing for the caller to close afterwards.
///
/// # Safety
///
/// Servo's GL context must be current, and `fd` must be an `OPAQUE_FD` onto an
/// allocation of `size_bytes` holding an image matching `size`.
unsafe fn import_into_gl(
    ext: &MemoryObjectExt,
    gl: &glow::Context,
    fd: i32,
    size_bytes: u64,
    size: PhysicalSize<u32>,
) -> Result<(u32, glow::Texture), String> {
    // Start from a clean slate so the check at the end is about this import.
    while gl.get_error() != glow::NO_ERROR {}

    let mut memory_object: u32 = 0;
    (ext.create)(1, &mut memory_object);
    // The Vulkan allocation is dedicated, so this one has to say so too.
    let dedicated: i32 = 1;
    (ext.parameteriv)(
        memory_object,
        GL_DEDICATED_MEMORY_OBJECT_EXT,
        &dedicated as *const i32,
    );
    (ext.import_fd)(memory_object, size_bytes, GL_HANDLE_TYPE_OPAQUE_FD_EXT, fd);

    let texture = match gl.create_texture() {
        Ok(texture) => texture,
        Err(error) => {
            (ext.delete)(1, &memory_object);
            return Err(format!("glGenTextures failed: {error}"));
        }
    };
    gl.bind_texture(glow::TEXTURE_2D, Some(texture));
    // Has to be set before the storage call, and has to match the VkImage.
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        GL_TEXTURE_TILING_EXT,
        GL_OPTIMAL_TILING_EXT,
    );
    (ext.tex_storage_2d)(
        glow::TEXTURE_2D,
        1,
        glow::RGBA8,
        size.width as i32,
        size.height as i32,
        memory_object,
        0,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MIN_FILTER,
        glow::LINEAR as i32,
    );
    gl.tex_parameter_i32(
        glow::TEXTURE_2D,
        glow::TEXTURE_MAG_FILTER,
        glow::LINEAR as i32,
    );
    gl.bind_texture(glow::TEXTURE_2D, None);

    let error = gl.get_error();
    if error != glow::NO_ERROR || memory_object == 0 {
        gl.delete_texture(texture);
        if memory_object != 0 {
            (ext.delete)(1, &memory_object);
        }
        return Err(format!(
            "the GL_EXT_memory_object import left GL error 0x{error:x}"
        ));
    }
    Ok((memory_object, texture))
}
