//! The macOS / Metal GPU sharing path.
//!
//! The simplest of the three. The offscreen surface surfman creates on macOS is
//! an IOSurface, and an IOSurface is shareable across processes and devices to
//! begin with. Godot's `MTLDevice` can therefore be asked for
//! `newTextureWithDescriptor:iosurface:plane:` directly, with no copy and no
//! handle to pass around.
//!
//! Because nothing is transferred, there is also nowhere to correct the
//! orientation. GL's origin is bottom-left, so the result arrives upside down,
//! which is why `needs_v_flip()` returns `true`.

use dpi::PhysicalSize;
use godot::classes::rendering_device::{DataFormat, TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{RenderingServer, Texture2D, Texture2Drd};
use godot::prelude::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_metal::{MTLPixelFormat, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
use surfman::cgl::surface::NativeSurface;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct MetalBridge {
    /// Holds the IOSurface reference. Dropping it would pull the storage out
    /// from under the texture below.
    _native_surface: NativeSurface,
    /// The `id<MTLTexture>` handed to Godot. Godot does not necessarily retain
    /// it, so keep a reference here too.
    _metal_texture: Retained<AnyObject>,
    rd_texture: Rid,
    texture: Gd<Texture2Drd>,
}

impl MetalBridge {
    pub fn new(context: &GodotRenderingContext, size: PhysicalSize<u32>) -> Result<Self, String> {
        let metal_device = super::godot_logical_device()?;

        // Take the IOSurface out of the bound surface. That needs a `&Surface`,
        // so unbind and rebind once. The same surface is used from here on, so
        // the texture created below stays valid for the whole lifetime.
        let native_surface = context
            .with_unbound_surface(|device, surface| device.native_surface(surface))
            .map_err(|error| format!("failed to take the surfman surface: {error:?}"))?;

        let metal_texture = unsafe { create_metal_texture(metal_device, &native_surface, size)? };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        let rd_texture = rendering_device.texture_create_from_extension(
            TextureType::TYPE_2D,
            // The IOSurface is created as kCVPixelFormatType_32BGRA.
            DataFormat::B8G8R8A8_UNORM,
            TextureSamples::SAMPLES_1,
            TextureUsageBits::SAMPLING_BIT | TextureUsageBits::COLOR_ATTACHMENT_BIT,
            Retained::as_ptr(&metal_texture) as u64,
            size.width as u64,
            size.height as u64,
            1,
            1,
        );
        if !rd_texture.is_valid() {
            return Err("texture_create_from_extension returned an invalid RID".into());
        }

        let mut texture = Texture2Drd::new_gd();
        texture.set_texture_rd_rid(rd_texture);

        Ok(Self {
            _native_surface: native_surface,
            _metal_texture: metal_texture,
            rd_texture,
            texture,
        })
    }
}

impl TextureBridge for MetalBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    /// Both sides look at the same memory, so there is nothing to transfer.
    /// Just flush the outstanding GL work.
    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        use glow::HasContext;
        unsafe { context.glow().flush() };
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "metal-iosurface"
    }

    fn needs_v_flip(&self) -> bool {
        true
    }

    fn release(&mut self, _context: &GodotRenderingContext) {
        if self.rd_texture.is_valid() {
            self.texture.set_texture_rd_rid(Rid::Invalid);
            if let Some(mut rendering_device) = RenderingServer::singleton().get_rendering_device()
            {
                rendering_device.free_rid(self.rd_texture);
            }
            self.rd_texture = Rid::Invalid;
        }
    }
}

/// Ask Godot's `MTLDevice` for a texture backed by the IOSurface Servo draws into.
///
/// # Safety
///
/// `metal_device` must be a live `id<MTLDevice>`.
unsafe fn create_metal_texture(
    metal_device: u64,
    native_surface: &NativeSurface,
    size: PhysicalSize<u32>,
) -> Result<Retained<AnyObject>, String> {
    let descriptor = MTLTextureDescriptor::new();
    descriptor.setTextureType(MTLTextureType::Type2D);
    descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    descriptor.setWidth(size.width as usize);
    descriptor.setHeight(size.height as usize);
    descriptor.setDepth(1);
    descriptor.setMipmapLevelCount(1);
    descriptor.setSampleCount(1);
    // Godot declares it as a color attachment too, so allow both usages.
    descriptor.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);

    let device: &AnyObject = &*(metal_device as *const AnyObject);
    let io_surface = &*native_surface.0;

    let texture: Option<Retained<AnyObject>> = objc2::msg_send![
        device,
        newTextureWithDescriptor: &*descriptor,
        iosurface: io_surface,
        plane: 0usize,
    ];

    texture.ok_or_else(|| "newTextureWithDescriptor:iosurface:plane: returned nil".into())
}
