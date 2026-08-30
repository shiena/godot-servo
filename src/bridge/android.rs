//! The Android GPU sharing path.
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
//! This path **only works with the Compatibility (GLES3) renderer**. Under
//! Forward+ and Mobile, `texture_external_initialize()` in the `RenderingDevice`
//! backend is a stub, so those fall back to CPU readback.

use std::ffi::{c_void, CString};

use dpi::PhysicalSize;
use glow::HasContext;
use godot::classes::{ExternalTexture, Texture2D};
use godot::prelude::*;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

// ── Android / EGL constants ──────────────────────────────────────────────

const AHARDWAREBUFFER_FORMAT_R8G8B8A8_UNORM: u32 = 1;
const AHARDWAREBUFFER_USAGE_GPU_SAMPLED_IMAGE: u64 = 1 << 8;
const AHARDWAREBUFFER_USAGE_GPU_COLOR_OUTPUT: u64 = 1 << 9;

const EGL_NONE: i32 = 0x3038;
const EGL_TRUE: i32 = 1;
const EGL_NATIVE_BUFFER_ANDROID: u32 = 0x3140;
const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;

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

/// EGL / GLES extension entry points, resolved through surfman's `get_proc_address`.
struct EglExtensions {
    get_current_display: unsafe extern "C" fn() -> *mut c_void,
    get_native_client_buffer: unsafe extern "C" fn(*const c_void) -> *mut c_void,
    create_image:
        unsafe extern "C" fn(*mut c_void, *mut c_void, u32, *mut c_void, *const i32) -> *mut c_void,
    destroy_image: unsafe extern "C" fn(*mut c_void, *mut c_void) -> u32,
    image_target_texture_2d: unsafe extern "C" fn(u32, *mut c_void),
}

impl EglExtensions {
    fn load(load: &dyn Fn(&str) -> *const c_void) -> Result<Self, String> {
        fn require(
            load: &dyn Fn(&str) -> *const c_void,
            name: &str,
        ) -> Result<*const c_void, String> {
            let pointer = load(name);
            if pointer.is_null() {
                return Err(format!("{name} is not available"));
            }
            Ok(pointer)
        }

        // SAFETY: all standard EGL / GLES entry points, with the signatures the
        // specification gives them.
        unsafe {
            Ok(Self {
                get_current_display: std::mem::transmute(require(load, "eglGetCurrentDisplay")?),
                get_native_client_buffer: std::mem::transmute(require(
                    load,
                    "eglGetNativeClientBufferANDROID",
                )?),
                create_image: std::mem::transmute(require(load, "eglCreateImageKHR")?),
                destroy_image: std::mem::transmute(require(load, "eglDestroyImageKHR")?),
                image_target_texture_2d: std::mem::transmute(require(
                    load,
                    "glEGLImageTargetTexture2DOES",
                )?),
            })
        }
    }
}

// ── The bridge ───────────────────────────────────────────────────────────

pub struct AndroidBridge {
    /// Kept so that `eglDestroyImageKHR` can be called on release.
    egl: EglExtensions,
    buffer: *mut c_void,
    display: *mut c_void,
    image: *mut c_void,
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

        let device = context.device();
        let gl_context = context.context_mut();
        let load = |name: &str| {
            let name = CString::new(name).expect("no interior nul");
            device.get_proc_address(&gl_context, name.to_str().expect("valid utf-8"))
        };
        let egl = EglExtensions::load(&load)?;
        drop(gl_context);
        drop(device);

        // SAFETY: what follows is a straight sequence of raw EGL and
        // AHardwareBuffer calls. Anything already allocated is released before
        // returning on failure.
        unsafe {
            let buffer = allocate_buffer(size)?;

            let display = (egl.get_current_display)();
            if display.is_null() {
                AHardwareBuffer_release(buffer);
                return Err("eglGetCurrentDisplay returned no display".into());
            }

            let client_buffer = (egl.get_native_client_buffer)(buffer);
            if client_buffer.is_null() {
                AHardwareBuffer_release(buffer);
                return Err("eglGetNativeClientBufferANDROID returned null".into());
            }

            let attributes = [EGL_IMAGE_PRESERVED_KHR, EGL_TRUE, EGL_NONE];
            let image = (egl.create_image)(
                display,
                std::ptr::null_mut(), // EGL_NO_CONTEXT
                EGL_NATIVE_BUFFER_ANDROID,
                client_buffer,
                attributes.as_ptr(),
            );
            if image.is_null() {
                AHardwareBuffer_release(buffer);
                return Err("eglCreateImageKHR failed".into());
            }

            // The GL texture Servo will write into.
            let gl = context.glow();
            let gl_texture = gl
                .create_texture()
                .map_err(|error| format!("glGenTextures failed: {error}"))?;
            gl.bind_texture(glow::TEXTURE_2D, Some(gl_texture));
            (egl.image_target_texture_2d)(glow::TEXTURE_2D, image);
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

/// Allocate one AHardwareBuffer.
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

impl TextureBridge for AndroidBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        let source_fbo = context.framebuffer();
        // SAFETY: the GL context is current and the texture is alive.
        unsafe { blit_flipped(context.glow(), source_fbo, self.gl_texture, self.size) }
    }

    fn backend_name(&self) -> &'static str {
        "android-ahardwarebuffer"
    }

    fn needs_external_sampler(&self) -> bool {
        true
    }

    fn release(&mut self) {
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

/// Blit Servo's FBO into the shared buffer, flipping it vertically.
///
/// # Safety
///
/// `gl`'s context must be current and `gl_texture` must be valid.
unsafe fn blit_flipped(
    gl: &glow::Context,
    source_fbo: u32,
    gl_texture: glow::Texture,
    size: PhysicalSize<u32>,
) -> Result<(), String> {
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
