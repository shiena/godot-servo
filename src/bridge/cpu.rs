//! どこでも動くフォールバック。
//!
//! `glReadPixels` で GPU からピクセルを吸い出し、`ImageTexture` に流し込む。
//! GPU→CPU→GPU の往復が入るので当然遅いが、レンダラを問わず動く。
//! GPU 共有が使えない環境でも拡張が起動しないという事態を避けるためにある。

use dpi::PhysicalSize;
use godot::classes::image::Format;
use godot::classes::{Image, ImageTexture, Texture2D};
use godot::prelude::*;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct CpuBridge {
    texture: Gd<ImageTexture>,
    image: Gd<Image>,
    size: PhysicalSize<u32>,
}

impl CpuBridge {
    pub fn new(size: PhysicalSize<u32>) -> Self {
        let width = size.width.max(1) as i32;
        let height = size.height.max(1) as i32;

        let image = Image::create_empty(width, height, false, Format::RGBA8)
            .expect("Image::create_empty should not fail for a valid size");
        let texture = ImageTexture::create_from_image(&image)
            .expect("ImageTexture::create_from_image should not fail for a valid image");

        Self {
            texture,
            image,
            size,
        }
    }
}

impl TextureBridge for CpuBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        let pixels = context
            .read_pixels_flipped(self.size)
            .ok_or("glReadPixels failed")?;

        let data = PackedByteArray::from(pixels.as_slice());
        let image = Image::create_from_data(
            self.size.width as i32,
            self.size.height as i32,
            false,
            Format::RGBA8,
            &data,
        )
        .ok_or("Image::create_from_data failed")?;

        self.image = image;
        self.texture.update(&self.image);
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "cpu-readback"
    }
}
