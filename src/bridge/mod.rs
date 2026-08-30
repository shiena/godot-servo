//! The route that carries what Servo drew into a Godot texture.
//!
//! Backends that can share the GPU resource directly do so; the rest fall back
//! to a CPU readback. `backend_name()` reports which one is in use.

use dpi::PhysicalSize;
use godot::classes::{RenderingServer, Texture2D};
use godot::prelude::*;

use crate::rendering_context::GodotRenderingContext;

pub mod cpu;

#[cfg(windows)]
pub mod d3d12;

#[cfg(target_os = "macos")]
pub mod metal;

#[cfg(target_os = "android")]
pub mod android;

pub trait TextureBridge {
    /// The texture handed to Godot. The same instance for the whole lifetime.
    fn texture(&self) -> Gd<Texture2D>;

    /// Called right after Servo's `paint()` and before `present()`.
    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String>;

    fn backend_name(&self) -> &'static str;

    /// Whether the texture can only be read through `samplerExternalOES`.
    ///
    /// Android's `GL_TEXTURE_EXTERNAL_OES` is such a texture. A `sampler2D`
    /// reads it as black, so the caller has to swap in a different shader.
    fn needs_external_sampler(&self) -> bool {
        false
    }

    /// Whether the texture arrives upside down.
    ///
    /// GL's origin is bottom-left, which a path with a transfer step can correct
    /// on the way through. macOS shares the IOSurface as it is and has nowhere to
    /// do that, so it returns `true`.
    fn needs_v_flip(&self) -> bool {
        false
    }

    /// Called as the node leaves the tree. Frees the Godot RIDs.
    fn release(&mut self) {}
}

/// Work out whether GPU sharing is available here and build the matching bridge.
///
/// A GPU path that fails to initialise warns and falls back to the CPU one. The
/// extension must never refuse to start just because the renderer is unsupported.
pub fn create(
    context: &GodotRenderingContext,
    size: PhysicalSize<u32>,
    host: &crate::gl_guard::HostContext,
) -> Box<dyn TextureBridge> {
    let driver = RenderingServer::singleton()
        .get_current_rendering_driver_name()
        .to_string();

    match try_create_shared(&driver, context, size, host) {
        Some(Ok(bridge)) => return bridge,
        Some(Err(error)) => {
            godot_warn!(
                "godot-servo: GPU texture sharing on the '{driver}' backend failed \
                 ({error}); falling back to CPU readback."
            );
        }
        None => {
            godot_print!(
                "godot-servo: no GPU sharing path for the '{driver}' backend; \
                 using CPU readback."
            );
        }
    }

    Box::new(cpu::CpuBridge::new(size))
}

/// `Some` when this combination has a sharing path at all, `None` when it does not.
#[allow(unused_variables)]
fn try_create_shared(
    driver: &str,
    context: &GodotRenderingContext,
    size: PhysicalSize<u32>,
    host: &crate::gl_guard::HostContext,
) -> Option<Result<Box<dyn TextureBridge>, String>> {
    let _ = host;
    #[cfg(windows)]
    if driver == "d3d12" {
        return Some(
            d3d12::D3d12Bridge::new(context, size)
                .map(|bridge| Box::new(bridge) as Box<dyn TextureBridge>),
        );
    }

    // GPU sharing on Android only works with the Compatibility (GLES3) renderer.
    // In the RenderingDevice backends behind Forward+ and Mobile,
    // texture_external_initialize() is a stub, so there is nothing to receive an
    // external texture with.
    #[cfg(target_os = "android")]
    if driver == "opengl3" {
        return Some(
            android::AndroidBridge::new(context, size, host)
                .map(|bridge| Box::new(bridge) as Box<dyn TextureBridge>),
        );
    }

    #[cfg(target_os = "macos")]
    if driver == "metal" {
        return Some(
            metal::MetalBridge::new(context, size)
                .map(|bridge| Box::new(bridge) as Box<dyn TextureBridge>),
        );
    }

    None
}

/// Godot's logical device handle, shared by every GPU bridge.
///
/// A `VkDevice` on Vulkan, an `ID3D12Device*` on D3D12, an `id<MTLDevice>` on Metal.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn godot_logical_device() -> Result<u64, String> {
    use godot::classes::rendering_device::DriverResource;

    let rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("no RenderingDevice (the Compatibility renderer has none)")?;

    let handle =
        rendering_device.get_driver_resource(DriverResource::LOGICAL_DEVICE, Rid::Invalid, 0);
    if handle == 0 {
        return Err("get_driver_resource returned a null logical device".into());
    }
    Ok(handle)
}
