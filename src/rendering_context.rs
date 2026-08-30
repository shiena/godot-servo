//! The `RenderingContext` implementation for Godot.
//!
//! One windowless surfman context, with a single offscreen surface bound to it
//! for Servo to draw into: an ANGLE pbuffer (a D3D11 texture) on Windows, an
//! IOSurface on macOS.
//!
//! There is no double buffering. Keeping to one surface means the RID of the
//! texture handed to Godot never changes for the life of the view. In exchange
//! Godot can sample while Servo draws, but that has caused no visible problem
//! as long as `paint()`, `present()` (glFlush), and Godot's own render stay in
//! that order on the main thread.

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
        // No window handle is passed. On Windows, with `sm-angle-default`
        // enabled, this yields a Device on the ANGLE (D3D11) backend.
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

    /// `make_current` callable directly rather than through the trait.
    pub fn make_current_public(&self) -> Result<(), Error> {
        let device = self.device.borrow();
        let context = self.context.borrow();
        device.make_context_current(&context)
    }

    /// The id of the FBO Servo draws into. The blit source for the shared texture.
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

    /// Unbind the current surface temporarily and hand it to `f`.
    ///
    /// Used on macOS to get at the IOSurface. `context_surface_info()` does not
    /// hand back a `&Surface`, so this detour is the only way to reach one.
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

    /// Recreate the surface. The caller must rebuild the `TextureBridge` too.
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

    /// Read the FBO back as RGBA8, flipped vertically. For the CPU fallback.
    pub fn read_pixels_flipped(&self, size: PhysicalSize<u32>) -> Option<Vec<u8>> {
        let mut pixels = vec![0u8; size.width as usize * size.height as usize * 4];
        self.read_pixels_flipped_into(size, &mut pixels)
            .then_some(pixels)
    }

    /// `read_pixels_flipped()`, writing straight into the caller's buffer.
    ///
    /// The CPU fallback calls this every frame. Letting the caller reuse its
    /// allocation removes several MB of allocation and copying per frame.
    /// `out` must be exactly `width * height * 4` bytes long.
    pub fn read_pixels_flipped_into(&self, size: PhysicalSize<u32>, out: &mut [u8]) -> bool {
        let stride = size.width as usize * 4;
        let rows = size.height as usize;
        if out.len() != stride * rows {
            return false;
        }

        let fbo = self.framebuffer();
        self.gleam_gl.bind_framebuffer(gl::FRAMEBUFFER, fbo);
        // Works around a known OSMesa bug, matching Servo's own implementation.
        self.gleam_gl.bind_vertex_array(0);

        self.gleam_gl.read_pixels_into_buffer(
            0,
            0,
            size.width as i32,
            size.height as i32,
            gl::RGBA,
            gl::UNSIGNED_BYTE,
            out,
        );

        let error = self.gleam_gl.get_error();
        if error != gl::NO_ERROR {
            log::warn!("glReadPixels left GL error 0x{error:x}");
            return false;
        }

        // GL's origin is bottom-left, so swap the rows top to bottom.
        for y in 0..rows / 2 {
            let top = y * stride;
            let bottom = (rows - 1 - y) * stride;
            let (head, tail) = out.split_at_mut(bottom);
            head[top..top + stride].swap_with_slice(&mut tail[..stride]);
        }
        true
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

    /// Single buffered, so there is nothing to swap. Flush the outstanding GL
    /// work before Godot reads.
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
        // `surface` is moved just below, so read its size first.
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
