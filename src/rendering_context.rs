//! Godot 用の `RenderingContext` 実装。
//!
//! ウィンドウを持たない surfman コンテキストを 1 つ作り、そこに紐づけた
//! オフスクリーンサーフェス (Windows なら ANGLE の pbuffer = D3D11 テクスチャ、
//! macOS なら IOSurface) に Servo を描かせる。
//!
//! ダブルバッファは使わない。サーフェスを 1 枚に固定することで、Godot 側に渡す
//! テクスチャの RID が生存期間中ずっと変わらずに済む。代償として Servo の描画中に
//! Godot がサンプルしうるが、`paint()` -> `present()` (glFlush) -> Godot の描画
//! という順序をメインスレッドで守っている限り実害は出ない。

use std::cell::{Ref, RefCell, RefMut};
use std::rc::Rc;
use std::sync::Arc;

use dpi::PhysicalSize;
use euclid::default::Size2D;
use gleam::gl::{self, Gl};
use image::RgbaImage;
use servo::{DeviceIntRect, RenderingContext};
use surfman::{
    Connection, Context, ContextAttributeFlags, ContextAttributes, Device, Error, GLApi, Surface,
    SurfaceAccess, SurfaceTexture, SurfaceType,
};

pub struct GodotRenderingContext {
    gleam_gl: Rc<dyn Gl>,
    glow_gl: Arc<glow::Context>,
    device: RefCell<Device>,
    context: RefCell<Context>,
    size: RefCell<PhysicalSize<u32>>,
}

impl Drop for GodotRenderingContext {
    fn drop(&mut self) {
        let device = self.device.borrow_mut();
        let mut context = self.context.borrow_mut();
        if let Ok(Some(mut surface)) = device.unbind_surface_from_context(&mut context) {
            let _ = device.destroy_surface(&mut context, &mut surface);
        }
        let _ = device.destroy_context(&mut context);
    }
}

impl GodotRenderingContext {
    pub fn new(size: PhysicalSize<u32>) -> Result<Self, Error> {
        // ウィンドウハンドルを渡さない。Windows で `sm-angle-default` が効いていれば
        // ここで ANGLE (D3D11) バックエンドの Device になる。
        let connection = Connection::new()?;
        let adapter = connection.create_adapter()?;
        let device = connection.create_device(&adapter)?;

        let flags = ContextAttributeFlags::ALPHA
            | ContextAttributeFlags::DEPTH
            | ContextAttributeFlags::STENCIL;
        let gl_api = connection.gl_api();
        let version = match &gl_api {
            GLApi::GLES => surfman::GLVersion { major: 3, minor: 0 },
            GLApi::GL => surfman::GLVersion { major: 3, minor: 2 },
        };
        let descriptor = device.create_context_descriptor(&ContextAttributes { flags, version })?;
        let mut context = device.create_context(&descriptor, None)?;

        let gleam_gl = match gl_api {
            GLApi::GL => unsafe {
                gl::GlFns::load_with(|name| device.get_proc_address(&context, name))
            },
            GLApi::GLES => unsafe {
                gl::GlesFns::load_with(|name| device.get_proc_address(&context, name))
            },
        };
        let glow_gl = Arc::new(unsafe {
            glow::Context::from_loader_function(|name| device.get_proc_address(&context, name))
        });

        let surface = device.create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: to_surfman_size(size),
            },
        )?;
        device
            .bind_surface_to_context(&mut context, surface)
            .map_err(|(err, mut surface)| {
                let _ = device.destroy_surface(&mut context, &mut surface);
                err
            })?;
        device.make_context_current(&context)?;

        Ok(Self {
            gleam_gl,
            glow_gl,
            device: RefCell::new(device),
            context: RefCell::new(context),
            size: RefCell::new(size),
        })
    }

    pub fn device(&self) -> Ref<'_, Device> {
        self.device.borrow()
    }

    pub fn context_mut(&self) -> RefMut<'_, Context> {
        self.context.borrow_mut()
    }

    pub fn glow(&self) -> &Arc<glow::Context> {
        &self.glow_gl
    }

    /// トレイト経由でなく直接呼べる `make_current`。
    pub fn make_current_public(&self) -> Result<(), Error> {
        let device = self.device.borrow();
        let context = self.context.borrow();
        device.make_context_current(&context)
    }

    /// Servo が描き込んでいる FBO の ID。共有テクスチャへの blit 元になる。
    pub fn framebuffer(&self) -> u32 {
        let device = self.device.borrow();
        let context = self.context.borrow();
        device
            .context_surface_info(&context)
            .ok()
            .flatten()
            .and_then(|info| info.framebuffer_object)
            .map(|fbo| fbo.0.get())
            .unwrap_or(0)
    }

    /// バインド中のサーフェスを一時的に取り外して `f` に渡す。
    ///
    /// macOS で IOSurface を取り出すときに使う。`&Surface` は
    /// `context_surface_info()` からは得られないため、この経路が必要になる。
    pub fn with_unbound_surface<T>(
        &self,
        f: impl FnOnce(&Device, &Surface) -> T,
    ) -> Result<T, Error> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();

        let surface = device
            .unbind_surface_from_context(&mut context)?
            .ok_or(Error::Failed)?;
        let result = f(&device, &surface);
        device
            .bind_surface_to_context(&mut context, surface)
            .map_err(|(err, mut surface)| {
                let _ = device.destroy_surface(&mut context, &mut surface);
                err
            })?;
        device.make_context_current(&context)?;
        Ok(result)
    }

    /// サーフェスを作り直す。呼び出し側は `TextureBridge` も作り直すこと。
    pub fn recreate_surface(&self, size: PhysicalSize<u32>) -> Result<(), Error> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();

        if let Some(mut old) = device.unbind_surface_from_context(&mut context)? {
            device.destroy_surface(&mut context, &mut old)?;
        }
        let surface = device.create_surface(
            &context,
            SurfaceAccess::GPUOnly,
            SurfaceType::Generic {
                size: to_surfman_size(size),
            },
        )?;
        device
            .bind_surface_to_context(&mut context, surface)
            .map_err(|(err, mut surface)| {
                let _ = device.destroy_surface(&mut context, &mut surface);
                err
            })?;
        device.make_context_current(&context)?;
        *self.size.borrow_mut() = size;
        Ok(())
    }

    /// FBO の中身を上下反転して RGBA8 で吸い出す。CPU フォールバック用。
    pub fn read_pixels_flipped(&self, size: PhysicalSize<u32>) -> Option<Vec<u8>> {
        let fbo = self.framebuffer();
        self.gleam_gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        // OSMesa の既知バグ回避 (servo 本体の実装に合わせる)。
        self.gleam_gl.bind_vertex_array(0);

        let width = size.width as i32;
        let height = size.height as i32;
        let mut pixels =
            self.gleam_gl
                .read_pixels(0, 0, width, height, gl::RGBA, gl::UNSIGNED_BYTE);

        let error = self.gleam_gl.get_error();
        if error != gl::NO_ERROR {
            log::warn!("glReadPixels left GL error 0x{error:x}");
            return None;
        }

        // GL は左下原点なので、行単位で上下を入れ替える。
        let stride = size.width as usize * 4;
        let rows = size.height as usize;
        if pixels.len() < stride * rows {
            return None;
        }
        for y in 0..rows / 2 {
            let top = y * stride;
            let bottom = (rows - 1 - y) * stride;
            let (head, tail) = pixels.split_at_mut(bottom);
            head[top..top + stride].swap_with_slice(&mut tail[..stride]);
        }
        Some(pixels)
    }
}

fn to_surfman_size(size: PhysicalSize<u32>) -> Size2D<i32> {
    Size2D::new(size.width.max(1) as i32, size.height.max(1) as i32)
}

impl RenderingContext for GodotRenderingContext {
    fn prepare_for_rendering(&self) {
        let fbo = self.framebuffer();
        self.gleam_gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
    }

    fn read_to_image(&self, source_rectangle: DeviceIntRect) -> Option<RgbaImage> {
        let size = PhysicalSize::new(
            source_rectangle.width() as u32,
            source_rectangle.height() as u32,
        );
        let pixels = self.read_pixels_flipped(size)?;
        RgbaImage::from_raw(size.width, size.height, pixels)
    }

    fn size(&self) -> PhysicalSize<u32> {
        *self.size.borrow()
    }

    fn resize(&self, size: PhysicalSize<u32>) {
        if *self.size.borrow() == size {
            return;
        }
        if let Err(error) = self.recreate_surface(size) {
            log::error!("Failed to resize the Servo rendering surface: {error:?}");
        }
    }

    /// 単一バッファなので swap はしない。Godot が読む前に GL の作業を流し切る。
    fn present(&self) {
        self.gleam_gl.flush();
    }

    fn make_current(&self) -> Result<(), Error> {
        let device = self.device.borrow();
        let context = self.context.borrow();
        device.make_context_current(&context)
    }

    fn gleam_gl_api(&self) -> Rc<dyn Gl> {
        self.gleam_gl.clone()
    }

    fn glow_gl_api(&self) -> Arc<glow::Context> {
        self.glow_gl.clone()
    }

    fn create_texture(&self, surface: Surface) -> Option<(SurfaceTexture, u32, Size2D<i32>)> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        // surface はこの後 move されるので、サイズは先に取っておく。
        let size = device.surface_info(&surface).size;
        let surface_texture = device.create_surface_texture(&mut context, surface).ok()?;
        let gl_texture = device
            .surface_texture_object(&surface_texture)
            .map(|texture| texture.0.get())
            .unwrap_or(0);
        Some((surface_texture, gl_texture, size))
    }

    fn destroy_texture(&self, surface_texture: SurfaceTexture) -> Option<Surface> {
        let device = self.device.borrow();
        let mut context = self.context.borrow_mut();
        device
            .destroy_surface_texture(&mut context, surface_texture)
            .ok()
    }

    fn connection(&self) -> Option<Connection> {
        Some(self.device.borrow().connection())
    }
}
