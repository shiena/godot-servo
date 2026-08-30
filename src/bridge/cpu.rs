//! どこでも動くフォールバック。
//!
//! `glReadPixels` で GPU からピクセルを吸い出し、`ImageTexture` に流し込む。
//! GPU→CPU→GPU の往復が入るので当然遅いが、レンダラを問わず動く。
//! GPU 共有が使えない環境でも拡張が起動しないという事態を避けるためにある。
//!
//! 往復そのものは減らせないので、せめて毎フレームの確保は起こさない。
//! ピクセルの置き場と `Image` を 2 組持ち、フレームごとに交互に使う。詳細は
//! [`CpuBridge::update`] を参照。

use dpi::PhysicalSize;
use godot::classes::image::Format;
use godot::classes::{Image, ImageTexture, Texture2D};
use godot::prelude::*;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct CpuBridge {
    texture: Gd<ImageTexture>,
    /// ピクセルの置き場と、それを指す `Image`。添字が揃っている。
    frames: [Frame; 2],
    /// 直近に `texture` へ渡した `frames` の添字。
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

    /// 1 フレーム分を読み出して `ImageTexture` に反映する。
    ///
    /// 書き込む先を毎フレーム入れ替えているのは、確保も複製も起こさないため。
    /// `PackedByteArray` は copy-on-write なので、`Image` が参照している側に書くと
    /// そこで複製が走る。直前に使った側には触らず、もう一方に書けば参照は 1 つのままで、
    /// `glReadPixels` の出力先がそのままテクスチャの中身になる。
    ///
    /// レンダリングサーバが別スレッドの場合、`texture.update()` はコマンドキューに
    /// 積まれるだけで、実際に読まれるのは最大 1 フレーム遅れる。2 組あれば、
    /// 読まれるのを待っている側を上書きすることはない。
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
