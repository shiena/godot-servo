//! The EGL entry points the Android path uses.
//!
//! They make an `AHardwareBuffer` — a buffer that lives outside GL — visible to
//! it as an `EGLImage`, and bind that to a texture Servo blits into.
//!
//! Everything is resolved through surfman's `get_proc_address`, which is
//! `eglGetProcAddress`, so the entry points come from the libEGL surfman is
//! already driving rather than from a second copy of it.

use std::ffi::c_void;

use crate::rendering_context::GodotRenderingContext;

pub type EglDisplay = *mut c_void;
pub type EglContext = *mut c_void;
pub type EglImage = *mut c_void;
pub type EglClientBuffer = *mut c_void;

pub const EGL_NONE: i32 = 0x3038;
pub const EGL_TRUE: i32 = 1;
pub const EGL_NO_CONTEXT: EglContext = std::ptr::null_mut();
pub const EGL_IMAGE_PRESERVED_KHR: i32 = 0x30D2;

pub struct Egl {
    pub get_current_display: unsafe extern "C" fn() -> EglDisplay,
    pub create_image:
        unsafe extern "C" fn(EglDisplay, EglContext, u32, EglClientBuffer, *const i32) -> EglImage,
    pub destroy_image: unsafe extern "C" fn(EglDisplay, EglImage) -> u32,
    pub image_target_texture_2d: unsafe extern "C" fn(u32, EglImage),
    pub get_native_client_buffer: unsafe extern "C" fn(*const c_void) -> EglClientBuffer,
}

impl Egl {
    /// Resolve the entry points against Servo's EGL implementation.
    ///
    /// The GL context does not have to be current for this, but every call made
    /// through the result does.
    pub fn load(context: &GodotRenderingContext) -> Result<Self, String> {
        let device = context.device();
        let gl_context = context.context_mut();

        let require = |name: &str| -> Result<*const c_void, String> {
            let pointer = device.get_proc_address(&gl_context, name);
            if pointer.is_null() {
                return Err(format!("{name} is not available"));
            }
            Ok(pointer)
        };

        // SAFETY: all standard EGL / GLES entry points, transmuted to the
        // signatures the specification gives them.
        unsafe {
            Ok(Self {
                get_current_display: std::mem::transmute(require("eglGetCurrentDisplay")?),
                create_image: std::mem::transmute(require("eglCreateImageKHR")?),
                destroy_image: std::mem::transmute(require("eglDestroyImageKHR")?),
                image_target_texture_2d: std::mem::transmute(require(
                    "glEGLImageTargetTexture2DOES",
                )?),
                get_native_client_buffer: std::mem::transmute(require(
                    "eglGetNativeClientBufferANDROID",
                )?),
            })
        }
    }

    /// The display Servo's context is on.
    ///
    /// # Safety
    ///
    /// Servo's GL context must be current.
    pub unsafe fn current_display(&self) -> Result<EglDisplay, String> {
        let display = (self.get_current_display)();
        if display.is_null() {
            return Err("eglGetCurrentDisplay returned no display".into());
        }
        Ok(display)
    }

    /// Create a GL texture that reads and writes through `image`.
    ///
    /// # Safety
    ///
    /// The GL context must be current and `image` must be a live `EGLImage`.
    pub unsafe fn bind_image_to_texture(
        &self,
        gl: &glow::Context,
        image: EglImage,
    ) -> Result<glow::Texture, String> {
        use glow::HasContext;

        let texture = gl
            .create_texture()
            .map_err(|error| format!("glGenTextures failed: {error}"))?;
        gl.bind_texture(glow::TEXTURE_2D, Some(texture));
        (self.image_target_texture_2d)(glow::TEXTURE_2D, image);
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
        if error != glow::NO_ERROR {
            gl.delete_texture(texture);
            return Err(format!(
                "glEGLImageTargetTexture2DOES left GL error 0x{error:x}"
            ));
        }
        Ok(texture)
    }
}
