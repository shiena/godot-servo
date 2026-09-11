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

use std::ffi::c_void;
use std::sync::OnceLock;

use dpi::PhysicalSize;
use godot::classes::rendering_device::{DataFormat, TextureSamples, TextureType, TextureUsageBits};
use godot::classes::{RenderingDevice, RenderingServer, Texture2D, Texture2Drd};
use godot::prelude::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_metal::{MTLPixelFormat, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
use surfman::cgl::surface::NativeSurface;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFGetRetainCount(cf: *const c_void) -> isize;
}

pub struct MetalBridge {
    /// Holds the IOSurface reference. Dropping it would pull the storage out
    /// from under the texture below.
    _native_surface: NativeSurface,
    /// The `id<MTLTexture>` handed to Godot. This reference is this bridge's
    /// own; Godot's is sorted out in `new()`.
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

        let metal_texture =
            unsafe { create_metal_texture(metal_device, Some(&native_surface), size)? };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        // `texture_free` releases the texture, but whether the import retained
        // it first depends on the driver. 4.6 does, and 4.8 does again
        // (godotengine/godot#121995). 4.7 only casts the pointer, so its release
        // takes the reference kept below. That one is then released a second
        // time when this bridge drops, and Godot crashes a few frames later. So
        // a driver like 4.7's is handed a reference of its own to release.
        let godot_takes_ours = godot_takes_imported_reference(metal_device, &mut rendering_device)?;
        let handed = if godot_takes_ours {
            Retained::into_raw(metal_texture.clone())
        } else {
            Retained::as_ptr(&metal_texture).cast_mut()
        };

        let rd_texture = import_texture(&mut rendering_device, handed, size);
        if !rd_texture.is_valid() {
            if godot_takes_ours {
                // SAFETY: from `into_raw` above, and Godot did not take it.
                drop(unsafe { Retained::from_raw(handed) });
            }
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

/// Whether Godot's Metal driver releases, in `texture_free`, a reference it
/// never retained on import. Measured once, on the first bridge.
///
/// A throwaway texture is imported, and its retain count shows whether the
/// import took a reference. A version check would leak on 4.8, and on any 4.7.x
/// that godotengine/godot#121995 is picked into. A retain count is no way to
/// manage references, but nothing else holds this texture, so a change across
/// the call is the import's doing.
fn godot_takes_imported_reference(
    metal_device: u64,
    rendering_device: &mut Gd<RenderingDevice>,
) -> Result<bool, String> {
    static TAKES: OnceLock<bool> = OnceLock::new();
    if let Some(&takes) = TAKES.get() {
        return Ok(takes);
    }

    let size = PhysicalSize::new(1, 1);
    let probe = unsafe { create_metal_texture(metal_device, None, size)? };
    let before = retain_count(&probe);
    let rd_texture = import_texture(rendering_device, Retained::as_ptr(&probe).cast_mut(), size);
    if !rd_texture.is_valid() {
        return Err("texture_create_from_extension returned an invalid RID".into());
    }
    let takes = retain_count(&probe) <= before;
    if takes {
        // Freeing the RID below releases the probe as well.
        std::mem::forget(probe.clone());
    }
    rendering_device.free_rid(rd_texture);

    Ok(*TAKES.get_or_init(|| takes))
}

/// Hand `metal_texture` to Godot as a RenderingDevice texture. The probe goes
/// through here too, so it takes the same path in the driver as the real one.
fn import_texture(
    rendering_device: &mut Gd<RenderingDevice>,
    metal_texture: *mut AnyObject,
    size: PhysicalSize<u32>,
) -> Rid {
    rendering_device.texture_create_from_extension(
        TextureType::TYPE_2D,
        // The IOSurface is created as kCVPixelFormatType_32BGRA. The same
        // format as the texture, so Godot makes no view of it: a view would be
        // what it keeps, and the reference handed over would never be released.
        DataFormat::B8G8R8A8_UNORM,
        TextureSamples::SAMPLES_1,
        TextureUsageBits::SAMPLING_BIT | TextureUsageBits::COLOR_ATTACHMENT_BIT,
        metal_texture as u64,
        size.width as u64,
        size.height as u64,
        1,
        1,
    )
}

fn retain_count(object: &AnyObject) -> isize {
    unsafe { CFGetRetainCount((object as *const AnyObject).cast()) }
}

/// Ask Godot's `MTLDevice` for a texture backed by the IOSurface Servo draws
/// into, or, without one, by storage of its own.
///
/// # Safety
///
/// `metal_device` must be a live `id<MTLDevice>`.
unsafe fn create_metal_texture(
    metal_device: u64,
    native_surface: Option<&NativeSurface>,
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

    let Some(native_surface) = native_surface else {
        let texture: Option<Retained<AnyObject>> =
            objc2::msg_send![device, newTextureWithDescriptor: &*descriptor];
        return texture.ok_or_else(|| "newTextureWithDescriptor: returned nil".into());
    };

    let io_surface = &*native_surface.0;
    let texture: Option<Retained<AnyObject>> = objc2::msg_send![
        device,
        newTextureWithDescriptor: &*descriptor,
        iosurface: io_surface,
        plane: 0usize,
    ];

    texture.ok_or_else(|| "newTextureWithDescriptor:iosurface:plane: returned nil".into())
}
