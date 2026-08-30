//! macOS / Metal の GPU 共有経路。
//!
//! 3 プラットフォームで最も単純になる。surfman が macOS で作るオフスクリーン
//! サーフェスの実体は IOSurface で、IOSurface はもともとプロセスやデバイスを
//! またいで共有できる。したがって Godot の `MTLDevice` にそのまま
//! `newTextureWithDescriptor:iosurface:plane:` させれば、コピーもハンドルの
//! やりとりも要らない。
//!
//! そのぶん転送が一切ないので、上下反転を吸収する場所もない。GL は左下原点なので
//! 結果は上下逆になる。`needs_v_flip()` が `true` を返すのはそのため。
//!
//! 注意: この経路は Windows 上では一度もコンパイルできていない。

use dpi::PhysicalSize;
use godot::classes::rendering_device::{
    DataFormat, TextureSamples, TextureType, TextureUsageBits,
};
use godot::classes::{RenderingServer, Texture2D, Texture2Drd};
use godot::prelude::*;
use objc2::rc::Retained;
use objc2::runtime::AnyObject;
use objc2_metal::{MTLPixelFormat, MTLTextureDescriptor, MTLTextureType, MTLTextureUsage};
use surfman::cgl::surface::NativeSurface;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct MetalBridge {
    /// IOSurface の参照を保持する。これが落ちると下のテクスチャの裏付けが消える。
    _native_surface: NativeSurface,
    /// Godot に渡した `id<MTLTexture>`。Godot 側が retain するとは限らないので
    /// こちらでも持っておく。
    _metal_texture: Retained<AnyObject>,
    rd_texture: Rid,
    texture: Gd<Texture2Drd>,
}

impl MetalBridge {
    pub fn new(
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let metal_device = super::godot_logical_device()?;

        // バインド中のサーフェスから IOSurface を取り出す。`&Surface` が要るので
        // 一度だけ unbind/rebind する。以降は同じサーフェスを使い続けるため、
        // ここで作ったテクスチャは寿命いっぱい有効。
        let native_surface = context
            .with_unbound_surface(|device, surface| device.native_surface(surface))
            .map_err(|error| format!("failed to take the surfman surface: {error:?}"))?;

        let metal_texture =
            unsafe { create_metal_texture(metal_device, &native_surface, size)? };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        let rd_texture = rendering_device.texture_create_from_extension(
            TextureType::TYPE_2D,
            // IOSurface は kCVPixelFormatType_32BGRA で作られる。
            DataFormat::B8G8R8A8_UNORM,
            TextureSamples::SAMPLES_1,
            TextureUsageBits::SAMPLING_BIT | TextureUsageBits::COLOR_ATTACHMENT_BIT,
            Retained::as_ptr(&metal_texture) as u64,
            size.width as u64,
            size.height as u64,
            1,
            1,
        );
        if !rd_texture.is_valid() {
            return Err("texture_create_from_extension returned an invalid RID".into());
        }

        let mut texture = Texture2Drd::new_gd();
        texture.set_texture_rd_rid(rd_texture);

        Ok(Self {
            _native_surface: native_surface,
            _metal_texture: metal_texture,
            rd_texture,
            texture,
        })
    }
}

impl TextureBridge for MetalBridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    /// 同じメモリを見ているので転送は要らない。GL の作業を流し切るだけ。
    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        use glow::HasContext;
        unsafe { context.glow().flush() };
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "metal-iosurface"
    }

    fn needs_v_flip(&self) -> bool {
        true
    }

    fn release(&mut self) {
        if self.rd_texture.is_valid() {
            self.texture.set_texture_rd_rid(Rid::Invalid);
            if let Some(mut rendering_device) =
                RenderingServer::singleton().get_rendering_device()
            {
                rendering_device.free_rid(self.rd_texture);
            }
            self.rd_texture = Rid::Invalid;
        }
    }
}

/// Godot の `MTLDevice` に、Servo が描いている IOSurface を裏付けとするテクスチャを
/// 作らせる。
///
/// # Safety
///
/// `metal_device` は生存中の `id<MTLDevice>` でなければならない。
unsafe fn create_metal_texture(
    metal_device: u64,
    native_surface: &NativeSurface,
    size: PhysicalSize<u32>,
) -> Result<Retained<AnyObject>, String> {
    let descriptor = MTLTextureDescriptor::new();
    descriptor.setTextureType(MTLTextureType::Type2D);
    descriptor.setPixelFormat(MTLPixelFormat::BGRA8Unorm);
    descriptor.setWidth(size.width as usize);
    descriptor.setHeight(size.height as usize);
    descriptor.setDepth(1);
    descriptor.setMipmapLevelCount(1);
    descriptor.setSampleCount(1);
    // Godot 側が color attachment としても宣言するので、両方許可しておく。
    descriptor.setUsage(MTLTextureUsage::ShaderRead | MTLTextureUsage::RenderTarget);

    let device: &AnyObject = &*(metal_device as *const AnyObject);
    let io_surface = &*native_surface.0;

    let texture: Option<Retained<AnyObject>> = objc2::msg_send![
        device,
        newTextureWithDescriptor: &*descriptor,
        iosurface: io_surface,
        plane: 0usize,
    ];

    texture.ok_or_else(|| "newTextureWithDescriptor:iosurface:plane: returned nil".into())
}
