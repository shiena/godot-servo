//! The route that carries what Servo drew into a Godot texture.
//!
//! Backends that can share the GPU resource directly do so; the rest fall back
//! to a CPU readback. `backend_name()` reports which one is in use.

use dpi::PhysicalSize;
use godot::classes::{RenderingServer, Texture2D};
use godot::prelude::*;

use crate::rendering_context::GodotRenderingContext;

pub mod cpu;
pub mod vulkan;

#[cfg(windows)]
pub mod angle;

#[cfg(windows)]
pub mod d3d12;

#[cfg(target_os = "macos")]
pub mod metal;

#[cfg(target_os = "android")]
pub mod egl;

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

    /// Called as the node leaves the tree, and again on every resize.
    ///
    /// Frees the Godot RIDs and whatever the path allocated. Servo's rendering
    /// context is still alive here and is passed in, because a GL object can
    /// only be deleted through the context that made it and nothing else
    /// guarantees one is current by this point.
    fn release(&mut self, context: &GodotRenderingContext) {
        let _ = context;
    }
}

/// What the look for a GPU sharing path turned up.
pub(crate) enum SharedPath {
    /// Built and ready to use.
    Ready(Box<dyn TextureBridge>),
    /// A path exists for this combination but could not be built.
    Failed(String),
    /// No path here, and why not. Not a fault: an unsupported renderer, or a
    /// Godot without the API the path is built on.
    Absent(String),
}

impl SharedPath {
    /// A bridge constructor's result, as a path.
    pub(crate) fn built<B: TextureBridge + 'static>(result: Result<B, String>) -> Self {
        match result {
            Ok(bridge) => Self::Ready(Box::new(bridge)),
            Err(error) => Self::Failed(error),
        }
    }
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
        SharedPath::Ready(bridge) => return bridge,
        SharedPath::Failed(error) => {
            godot_warn!(
                "godot-servo: GPU texture sharing on the '{driver}' backend failed \
                 ({error}); falling back to CPU readback."
            );
        }
        SharedPath::Absent(reason) => {
            godot_print!(
                "godot-servo: no GPU sharing path on the '{driver}' backend: \
                 {reason}. Using CPU readback."
            );
        }
    }

    Box::new(cpu::CpuBridge::new(size))
}

/// Pick the sharing path for this renderer, if there is one.
#[allow(unused_variables)]
fn try_create_shared(
    driver: &str,
    context: &GodotRenderingContext,
    size: PhysicalSize<u32>,
    host: &crate::gl_guard::HostContext,
) -> SharedPath {
    #[cfg(windows)]
    if driver == "d3d12" {
        return SharedPath::built(d3d12::D3d12Bridge::new(context, size));
    }

    // Android's Compatibility (GLES3) renderer receives the shared buffer as an
    // `ExternalTexture`. Forward+ and Mobile render through Vulkan and have no
    // GL context to hand one to, so they take the Vulkan path below instead.
    #[cfg(target_os = "android")]
    if driver == "opengl3" {
        return SharedPath::built(android::AndroidBridge::new(context, size, host));
    }

    #[cfg(target_os = "macos")]
    if driver == "metal" {
        return SharedPath::built(metal::MetalBridge::new(context, size));
    }

    if driver == "vulkan" {
        return vulkan::try_create(context, size);
    }

    SharedPath::Absent("this extension implements none for it".into())
}

/// Godot's logical device handle, shared by every GPU bridge.
///
/// A `VkDevice` on Vulkan, an `ID3D12Device*` on D3D12, an `id<MTLDevice>` on Metal.
pub(crate) fn godot_logical_device() -> Result<u64, String> {
    use godot::classes::rendering_device::DriverResource;

    driver_resource(DriverResource::LOGICAL_DEVICE, "logical device")
}

/// One of Godot's raw driver handles, or why it could not be had.
pub(crate) fn driver_resource(
    resource: godot::classes::rendering_device::DriverResource,
    what: &str,
) -> Result<u64, String> {
    let rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("no RenderingDevice (the Compatibility renderer has none)")?;

    let handle = rendering_device.get_driver_resource(resource, Rid::Invalid, 0);
    if handle == 0 {
        return Err(format!("get_driver_resource returned a null {what}"));
    }
    Ok(handle)
}

/// A texture Godot allocated itself, to copy an imported one into.
///
/// Both paths that go through `RenderingDevice` need this. `Texture2DRD` calls
/// `texture_create_shared()` internally, and neither driver renders the result
/// for a texture created from an extension: the D3D12 one rejects it outright,
/// through the `DEBUG_ENABLED` check in `_texture_create_shared_from_slice()`
/// that only accepts textures owning an allocation, and the Vulkan one accepts
/// it and then samples black. So what is displayed is always a texture Godot
/// owns, filled by [`copy_rd_texture`]. The copy stays on the GPU, so no CPU
/// round trip appears.
pub(crate) fn create_owned_rd_texture(
    rendering_device: &mut Gd<godot::classes::RenderingDevice>,
    format: godot::classes::rendering_device::DataFormat,
    size: PhysicalSize<u32>,
) -> Result<Rid, String> {
    use godot::classes::rendering_device::{TextureSamples, TextureType, TextureUsageBits};
    use godot::classes::{RdTextureFormat, RdTextureView};

    let mut texture_format = RdTextureFormat::new_gd();
    texture_format.set_format(format);
    texture_format.set_texture_type(TextureType::TYPE_2D);
    texture_format.set_width(size.width);
    texture_format.set_height(size.height);
    texture_format.set_depth(1);
    texture_format.set_array_layers(1);
    texture_format.set_mipmaps(1);
    texture_format.set_samples(TextureSamples::SAMPLES_1);
    texture_format.set_usage_bits(
        TextureUsageBits::SAMPLING_BIT
            | TextureUsageBits::CAN_COPY_TO_BIT
            | TextureUsageBits::CAN_COPY_FROM_BIT,
    );

    let view = RdTextureView::new_gd();
    let rid = rendering_device.texture_create(&texture_format, &view);
    if rid.is_valid() {
        Ok(rid)
    } else {
        Err("texture_create returned an invalid RID".into())
    }
}

/// Copy the whole of one RD texture into another, on the GPU.
pub(crate) fn copy_rd_texture(from: Rid, to: Rid, size: PhysicalSize<u32>) -> Result<(), String> {
    let mut rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("no RenderingDevice")?;
    let extent = Vector3::new(size.width as f32, size.height as f32, 1.0);
    let error =
        rendering_device.texture_copy(from, to, Vector3::ZERO, Vector3::ZERO, extent, 0, 0, 0, 0);
    if error != godot::global::Error::OK {
        return Err(format!("texture_copy failed: {error:?}"));
    }
    Ok(())
}

/// Blit Servo's FBO into `gl_texture`, flipping it vertically.
///
/// GL's origin is bottom-left and every API on the receiving side puts it
/// top-left, so the flip happens here, on the way through. A path that hands its
/// surface over untouched has nowhere to do this and reports `needs_v_flip()`
/// instead.
///
/// No explicit semaphore is available, so a `glFlush` afterwards stands in for
/// synchronization.
///
/// # Safety
///
/// `gl`'s context must be current and `gl_texture` must be valid.
#[cfg(any(windows, target_os = "android", target_os = "linux"))]
pub(crate) unsafe fn blit_flipped(
    gl: &glow::Context,
    source_fbo: u32,
    gl_texture: glow::Texture,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
    use glow::HasContext;

    let destination = gl.create_framebuffer().map_err(|error| error.to_string())?;
    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, Some(destination));
    gl.framebuffer_texture_2d(
        glow::DRAW_FRAMEBUFFER,
        glow::COLOR_ATTACHMENT0,
        glow::TEXTURE_2D,
        Some(gl_texture),
        0,
    );

    let source = std::num::NonZeroU32::new(source_fbo).map(glow::NativeFramebuffer);
    gl.bind_framebuffer(glow::READ_FRAMEBUFFER, source);

    let width = size.width as i32;
    let height = size.height as i32;
    gl.blit_framebuffer(
        0,
        0,
        width,
        height,
        0,
        height,
        width,
        0,
        glow::COLOR_BUFFER_BIT,
        glow::NEAREST,
    );
    gl.flush();

    gl.bind_framebuffer(glow::DRAW_FRAMEBUFFER, None);
    gl.delete_framebuffer(destination);
    Ok(())
}
