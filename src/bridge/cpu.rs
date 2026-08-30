//! The fallback that works everywhere.
//!
//! `glReadPixels` pulls the pixels off the GPU and they go into an
//! `ImageTexture`. The GPU-CPU-GPU round trip makes it slow, but it works
//! whatever the renderer is, so the extension never fails to start just because
//! GPU sharing is unavailable.
//!
//! The round trip itself cannot be avoided, but the per-frame allocation can.
//! Two sets of pixel buffer and `Image` alternate frame by frame; see
//! [`CpuBridge::update`].

use dpi::PhysicalSize;
use godot::classes::image::Format;
use godot::classes::{Image, ImageTexture, Texture2D};
use godot::prelude::*;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct CpuBridge {
    texture: Gd<ImageTexture>,
    /// The pixel buffers and the `Image` pointing at each, index-aligned.
    frames: [Frame; 2],
    /// Index of the frame most recently handed to `texture`.
    current: usize,
    size: PhysicalSize<u32>,
}

struct Frame {
    pixels: PackedByteArray,
    image: Gd<Image>,
}

impl CpuBridge {
    pub fn new(size: PhysicalSize<u32>) -> Self {
        let width = size.width.max(1) as i32;
        let height = size.height.max(1) as i32;
        let length = width as usize * height as usize * 4;

        let frames = std::array::from_fn(|_| {
            let mut pixels = PackedByteArray::new();
            pixels.resize(length);
            let mut image = Image::create_empty(width, height, false, Format::RGBA8)
                .expect("Image::create_empty should not fail for a valid size");
            image.set_data(width, height, false, Format::RGBA8, &pixels);
            Frame { pixels, image }
        });

        let texture = ImageTexture::create_from_image(&frames[0].image)
            .expect("ImageTexture::create_from_image should not fail for a valid image");

        Self {
            texture,
            frames,
            current: 0,
            size,
        }
    }
}

impl TextureBridge for CpuBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    /// Read one frame back and push it into the `ImageTexture`.
    ///
    /// The destination alternates every frame so that nothing is allocated or
    /// copied. `PackedByteArray` is copy-on-write, so writing into the buffer the
    /// `Image` currently references would duplicate it. Leaving that one alone
    /// and writing into the other keeps the reference count at 1, and the
    /// destination of `glReadPixels` becomes the texture's contents directly.
    ///
    /// When the rendering server runs on its own thread, `texture.update()` only
    /// queues a command and the data is read up to one frame later. Two sets are
    /// enough that the side still waiting to be read is never overwritten.
    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        let next = self.current ^ 1;
        let frame = &mut self.frames[next];

        if !context.read_pixels_flipped_into(self.size, frame.pixels.as_mut_slice()) {
            return Err("glReadPixels failed".into());
        }

        frame.image.set_data(
            self.size.width as i32,
            self.size.height as i32,
            false,
            Format::RGBA8,
            &frame.pixels,
        );
        self.texture.update(&frame.image);
        self.current = next;
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "cpu-readback"
    }
}
