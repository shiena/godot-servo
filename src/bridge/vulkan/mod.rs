//! The Vulkan GPU sharing path.
//!
//! Vulkan is Godot's default on Windows and Linux, and what Forward+ and Mobile
//! run on everywhere else, so it is the renderer most projects actually meet. It
//! used to fall straight through to CPU readback: a Vulkan device's extensions
//! are fixed when the device is created, and until
//! [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940)
//! nothing let a project ask for one. [`caps`] covers what that changed.
//!
//! Each platform hands Vulkan the same kind of thing — memory that Servo's GL
//! context can also see — and only the door differs:
//!
//! | Platform | The shared allocation | Passed through |
//! | --- | --- | --- |
//! | Windows | an ANGLE D3D11 texture, imported | `VK_KHR_external_memory_win32` |
//! | Linux, Android | a `VkImage`, exported to GL | `VK_KHR_external_memory_fd` |
//! | macOS | Servo's IOSurface, imported | `VK_EXT_metal_objects` (MoltenVK) |
//!
//! Only Windows and macOS need [`caps`]'s project setting for that. Godot
//! enables `VK_KHR_external_memory_fd` on its own, so Linux and Android run on a
//! stock Godot — see `opaque_fd`.
//!
//! From there the route is the D3D12 one: the imported image is a copy source,
//! and what Godot displays is a texture it allocated itself. Handing the
//! imported texture to `Texture2DRD` directly renders black on Godot 4.7, which
//! `crate::bridge::create_owned_rd_texture` goes into.

pub mod caps;
mod device;

#[cfg(windows)]
mod win32;
#[cfg(windows)]
use win32::SharedImage;

#[cfg(any(target_os = "linux", target_os = "android"))]
mod opaque_fd;
#[cfg(any(target_os = "linux", target_os = "android"))]
use opaque_fd::SharedImage;

#[cfg(target_os = "macos")]
mod metal;
#[cfg(target_os = "macos")]
use metal::SharedImage;

use ash::vk::Handle;
use dpi::PhysicalSize;
use godot::classes::rendering_device::{TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{RenderingServer, Texture2D, Texture2Drd};
use godot::prelude::*;

use self::device::VulkanDevice;
use super::{copy_rd_texture, create_owned_rd_texture, SharedPath, TextureBridge};
use crate::rendering_context::GodotRenderingContext;

/// Look for the Vulkan sharing path, and build it if it is there.
///
/// Everything before the import is a question about this Godot rather than about
/// this machine, so a missing API or a missing extension reports as
/// [`SharedPath::Absent`] with something the user can act on, not as a failure.
pub(crate) fn try_create(context: &GodotRenderingContext, size: PhysicalSize<u32>) -> SharedPath {
    // Only the paths that need something Godot does not enable of its own accord
    // have to ask, and asking is what needs the API that may not be there.
    if !SharedImage::REQUIRED_EXTENSIONS.is_empty() {
        let enabled = match caps::enabled_extensions() {
            Ok(enabled) => enabled,
            Err(reason) => return SharedPath::Absent(reason),
        };
        if let Err(reason) = caps::require(&enabled, SharedImage::REQUIRED_EXTENSIONS) {
            return SharedPath::Absent(reason);
        }
    }

    let vulkan = match VulkanDevice::from_godot() {
        Ok(vulkan) => vulkan,
        Err(error) => return SharedPath::Failed(error),
    };
    // A name on the enabled list is a claim; a resolved entry point is the fact.
    if let Err(reason) = vulkan.require_functions(SharedImage::REQUIRED_FUNCTIONS) {
        return SharedPath::Absent(reason);
    }

    SharedPath::built(VulkanBridge::new(vulkan, context, size))
}

pub struct VulkanBridge {
    vulkan: VulkanDevice,
    /// The platform's shared memory, as a `VkImage` and whatever Servo draws
    /// into it through.
    image: SharedImage,
    /// The imported image wrapped as a Godot RD texture. The copy source.
    imported: Rid,
    /// A texture Godot allocated itself. The copy destination, and what is displayed.
    owned: Rid,
    texture: Gd<Texture2Drd>,
    size: PhysicalSize<u32>,
}

impl VulkanBridge {
    fn new(
        vulkan: VulkanDevice,
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let mut image = SharedImage::new(&vulkan, context, size)?;

        match build_textures(&image, size) {
            Ok((imported, owned)) => {
                let mut texture = Texture2Drd::new_gd();
                texture.set_texture_rd_rid(owned);
                Ok(Self {
                    vulkan,
                    image,
                    imported,
                    owned,
                    texture,
                    size,
                })
            }
            Err(error) => {
                vulkan.wait_until_idle();
                image.destroy(&vulkan, context);
                Err(error)
            }
        }
    }
}

/// Wrap the imported image for Godot, and allocate what it copies into.
fn build_textures(image: &SharedImage, size: PhysicalSize<u32>) -> Result<(Rid, Rid), String> {
    let mut rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("no RenderingDevice")?;

    let imported = rendering_device.texture_create_from_extension(
        TextureType::TYPE_2D,
        SharedImage::GODOT_FORMAT,
        TextureSamples::SAMPLES_1,
        TextureUsageBits::SAMPLING_BIT | TextureUsageBits::CAN_COPY_FROM_BIT,
        image.vk_image().as_raw(),
        size.width as u64,
        size.height as u64,
        1,
        1,
    );
    if !imported.is_valid() {
        return Err("texture_create_from_extension returned an invalid RID".into());
    }

    match create_owned_rd_texture(&mut rendering_device, SharedImage::GODOT_FORMAT, size) {
        Ok(owned) => Ok((imported, owned)),
        Err(error) => {
            rendering_device.free_rid(imported);
            Err(error)
        }
    }
}

impl TextureBridge for VulkanBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        self.image.update(context, self.size)?;
        copy_rd_texture(self.imported, self.owned, self.size)
    }

    fn backend_name(&self) -> &'static str {
        SharedImage::BACKEND_NAME
    }

    fn needs_v_flip(&self) -> bool {
        SharedImage::NEEDS_V_FLIP
    }

    fn release(&mut self, context: &GodotRenderingContext) {
        self.texture.set_texture_rd_rid(Rid::Invalid);
        if let Some(mut rendering_device) = RenderingServer::singleton().get_rendering_device() {
            for rid in [&mut self.owned, &mut self.imported] {
                if rid.is_valid() {
                    rendering_device.free_rid(*rid);
                    *rid = Rid::Invalid;
                }
            }
        }
        // Godot's own free is queued, and frames referencing the image may still
        // be in flight, so the allocation cannot go until the device is done.
        self.vulkan.wait_until_idle();
        self.image.destroy(&self.vulkan, context);
    }
}
