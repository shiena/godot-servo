//! The Android GPU sharing path for the Compatibility renderer.
//!
//! The shared container is an `AHardwareBuffer`: Android's way of handing a GPU
//! buffer between processes and APIs, the counterpart of a D3D11 shared texture
//! on Windows or an IOSurface on macOS.
//!
//! Surfman uses `AHardwareBuffer` internally on Android too, but keeps it
//! `pub(crate)` and out of reach. So, as on Windows, this **allocates one and
//! blits into it**.
//!
//! 1. Allocate the buffer with `AHardwareBuffer_allocate`
//! 2. Turn it into an `EGLImage` with `eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID)`
//! 3. Servo side: bind it as a GL texture with `glEGLImageTargetTexture2DOES` and
//!    blit Servo's FBO into it every frame
//! 4. Godot side: hand the same `EGLImage` to an `ExternalTexture`, which Godot
//!    receives as a `GL_TEXTURE_EXTERNAL_OES`
//!
//! One `EGLImage` is enough because Android effectively has a single EGLDisplay,
//! and `EGLImage`s on the same display are shareable across contexts.
//!
//! Step 4 is what ties this path to the Compatibility (GLES3) renderer:
//! `texture_external_initialize()` in the `RenderingDevice` backends is a stub,
//! so Forward+ and Mobile have no `ExternalTexture` to receive the image. Those
//! take [`super::vulkan`] instead, which shares one allocation through an fd and
//! never needs an `AHardwareBuffer`.

use std::ffi::c_void;

use dpi::PhysicalSize;
use godot::classes::{ExternalTexture, Texture2D};
use godot::prelude::*;

use super::egl::{Egl, EglDisplay, EglImage, EGL_IMAGE_PRESERVED_KHR, EGL_NONE, EGL_TRUE};
use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

const AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM: u32 = 1;
const AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE: u64 = 1 << 8;
const AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT: u64 = 1 << 9;

const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;

#[repr(C)]
struct AHardwareBufferDesc {
    width: u32,
    height: u32,
    layers: u32,
    format: u32,
    usage: u64,
    stride: u32,
    rfu0: u32,
    rfu1: u64,
}

#[link(name = "android")]
unsafe extern "C" {
    fn AHardwareBuffer_allocate(desc: *const AHardwareBufferDesc, out: *mut *mut c_void) -> i32;
    fn AHardwareBuffer_release(buffer: *mut c_void);
}

/// Allocate one `AHardwareBuffer` for Servo to draw into.
///
/// # Safety
///
/// The caller must release the result with `AHardwareBuffer_release`.
unsafe fn allocate_buffer(size: PhysicalSize<u32>) -> Result<*mut c_void, String> {
    let descriptor = AHardwareBufferDesc {
        width: size.width,
        height: size.height,
        layers: 1,
        format: AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM,
        usage: AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE | AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT,
        stride: 0,
        rfu0: 0,
        rfu1: 0,
    };
    let mut buffer: *mut c_void = std::ptr::null_mut();
    let result = AHardwareBuffer_allocate(&descriptor, &mut buffer);
    if result != 0 || buffer.is_null() {
        return Err(format!("AHardwareBuffer_allocate failed ({result})"));
    }
    Ok(buffer)
}

/// Make an `AHardwareBuffer` visible to GL as an `EGLImage`.
///
/// # Safety
///
/// Servo's GL context must be current, and `buffer` must be a live
/// `AHardwareBuffer`. The caller must destroy the result with
/// `eglDestroyImageKHR`.
unsafe fn image_from_buffer(
    egl: &Egl,
    display: EglDisplay,
    buffer: *mut c_void,
) -> Result<EglImage, String> {
    let client_buffer = (egl.get_native_client_buffer)(buffer);
    if client_buffer.is_null() {
        return Err("eglGetNativeClientBufferANDROID returned null".into());
    }

    let attributes = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
    let image = (egl.create_image)(
        display,
        super::egl::EGL_NO_CONTEXT,
        EGL_NATIVE_BUFFER_ANDROID,
        client_buffer,
        attributes.as_ptr(),
    );
    if image.is_null() {
        return Err("eglCreateImageKHR failed for the AHardwareBuffer".into());
    }
    Ok(image)
}

pub struct AndroidBridge {
    /// Kept so that `eglDestroyImageKHR` can be called on release.
    egl: Egl,
    buffer: *mut c_void,
    display: EglDisplay,
    image: EglImage,
    /// The GL texture Servo writes through, with `image` bound to it.
    gl_texture: glow::Texture,
    texture: Gd<ExternalTexture>,
    size: PhysicalSize<u32>,
}

impl AndroidBridge {
    pub fn new(
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
        host: &crate::gl_guard::HostContext,
    ) -> Result<Self, String> {
        // Make Servo's GL context current before touching EGL.
        context
            .make_current_public()
            .map_err(|error| format!("make_current failed: {error:?}"))?;

        let egl = Egl::load(context)?;

        // SAFETY: what follows is a straight sequence of raw EGL and
        // AHardwareBuffer calls, with Servo's context current. Anything already
        // allocated is released before returning on failure.
        unsafe {
            let display = egl.current_display()?;
            let buffer = allocate_buffer(size)?;

            let image = match image_from_buffer(&egl, display, buffer) {
                Ok(image) => image,
                Err(error) => {
                    AHardwareBuffer_release(buffer);
                    return Err(error);
                }
            };

            let gl_texture = match egl.bind_image_to_texture(context.glow(), image) {
                Ok(texture) => texture,
                Err(error) => {
                    (egl.destroy_image)(display, image);
                    AHardwareBuffer_release(buffer);
                    return Err(error);
                }
            };

            // From here on the GL calls belong to Godot. `ExternalTexture` calls
            // `glEGLImageTargetTexture2DOES` internally, so creating it while
            // Servo's context is current would put the texture on the wrong
            // context. Hand the thread back first.
            host.restore();

            // Give Godot the same EGLImage. `ExternalTexture` is a Texture2D, so
            // it drops straight into a material.
            let mut texture = ExternalTexture::new_gd();
            texture.set_size(Vector2::new(size.width as f32, size.height as f32));
            texture.set_external_buffer_id(image as u64);

            Ok(Self {
                egl,
                buffer,
                display,
                image,
                gl_texture,
                texture,
                size,
            })
        }
    }
}

impl TextureBridge for AndroidBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        let source_fbo = context.framebuffer();
        // SAFETY: the GL context is current and the texture is alive.
        unsafe { super::blit_flipped(context.glow(), source_fbo, self.gl_texture, self.size) }
    }

    fn backend_name(&self) -> &'static str {
        "android-ahardwarebuffer"
    }

    fn needs_external_sampler(&self) -> bool {
        true
    }

    fn release(&mut self, context: &GodotRenderingContext) {
        // `eglDestroyImageKHR` wants the display Servo's context is on, and
        // nothing guarantees that context is still current by now.
        let _ = context.make_current_public();

        // SAFETY: everything here was allocated above. Nulled out so a second
        // call cannot double free.
        unsafe {
            if !self.image.is_null() && !self.display.is_null() {
                (self.egl.destroy_image)(self.display, self.image);
                self.image = std::ptr::null_mut();
            }
            if !self.buffer.is_null() {
                AHardwareBuffer_release(self.buffer);
                self.buffer = std::ptr::null_mut();
            }
        }
    }
}
