//! Android の GPU 共有経路。
//!
//! 共有の器は `AHardwareBuffer`。Android で GPU バッファをプロセスや API を
//! またいで受け渡すための仕組みで、Windows の D3D11 共有テクスチャや macOS の
//! IOSurface にあたる。
//!
//! surfman も Android では内部で `AHardwareBuffer` を使っているが、その中身は
//! `pub(crate)` で外から取れない。そこで Windows と同じく**自前で 1 枚確保して
//! そこへ blit する**。
//!
//! 1. `AHardwareBuffer_allocate` でバッファを確保
//! 2. `eglCreateImageKHR(EGL_NATIVE_BUFFER_ANDROID)` で `EGLImage` にする
//! 3. Servo 側: `glEGLImageTargetTexture2DOES` で GL テクスチャにし、毎フレーム
//!    Servo の FBO から blit する
//! 4. Godot 側: 同じ `EGLImage` を `ExternalTexture` に渡す。Godot はこれを
//!    `GL_TEXTURE_EXTERNAL_OES` として受け取る
//!
//! `EGLImage` を 1 つで済ませているのは、Android の EGLDisplay が実質 1 つしか
//! ないため。同じ display 上の `EGLImage` はコンテキストをまたいで共有できる。
//!
//! この経路は **Compatibility (GLES3) レンダラでのみ動く**。Forward+ / Mobile は
//! `RenderingDevice` 側の `texture_external_initialize()` が空実装なので、
//! そちらでは CPU リードバックに落ちる。

use std::ffi::{c_void, CString};

use dpi::PhysicalSize;
use glow::HasContext;
use godot::classes::{ExternalTexture, Texture2D};
use godot::prelude::*;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

// ── Android / EGL の定数 ──────────────────────────────────────────────────

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

/// EGL / GLES の拡張関数。surfman の `get_proc_address` から引く。
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

        // SAFETY: いずれも EGL / GLES の標準的な関数で、シグネチャは仕様どおり。
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

// ── 橋 ────────────────────────────────────────────────────────────────────

pub struct AndroidBridge {
    /// 解放時に `eglDestroyImageKHR` を呼ぶために持っておく。
    egl: EglExtensions,
    buffer: *mut c_void,
    display: *mut c_void,
    image: *mut c_void,
    /// Servo 側から書き込むための GL テクスチャ。`image` を貼ったもの。
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
        // Servo の GL コンテキストをカレントにしてから EGL を触る。
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

        // SAFETY: 以降は EGL / AHardwareBuffer の素の API を順に呼ぶだけ。
        // 途中で失敗したら確保済みのものを解放してから返す。
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

            // Servo が書き込む先の GL テクスチャ。
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

            // ここから先は Godot 側の GL 呼び出しになる。`ExternalTexture` は
            // 内部で `glEGLImageTargetTexture2DOES` を呼ぶので、Servo のコンテキストが
            // カレントなまま作ると Godot ではない側にテクスチャができてしまう。
            // 先に Godot のコンテキストへ戻す。
            host.restore();

            // 同じ EGLImage を Godot に渡す。`ExternalTexture` は Texture2D なので
            // マテリアルにそのまま挿せる。
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

/// AHardwareBuffer を 1 枚確保する。
///
/// # Safety
///
/// 呼び出し側が返り値を `AHardwareBuffer_release` で解放すること。
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
        // SAFETY: GL のコンテキストはカレントで、テクスチャは生きている。
        unsafe { blit_flipped(context.glow(), source_fbo, self.gl_texture, self.size) }
    }

    fn backend_name(&self) -> &'static str {
        "android-ahardwarebuffer"
    }

    fn needs_external_sampler(&self) -> bool {
        true
    }

    fn release(&mut self) {
        // SAFETY: いずれも自分で確保したもの。二重解放しないよう null にする。
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

/// Servo の FBO を共有バッファへ上下反転して転送する。
///
/// # Safety
///
/// `gl` のコンテキストがカレントで、`gl_texture` が有効であること。
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
