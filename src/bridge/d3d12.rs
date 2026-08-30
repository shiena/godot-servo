//! Windows / D3D12 の GPU 共有経路。
//!
//! ANGLE (Servo 側) は D3D11、Godot は D3D12 なので、その間を DXGI の共有 NT
//! ハンドルで繋ぐ。
//!
//! 1. ANGLE の `ID3D11Device` 上に `SHARED | SHARED_NTHANDLE` のテクスチャを確保
//! 2. `IDXGIResource1::CreateSharedHandle` で NT ハンドルを export
//! 3. Godot の `ID3D12Device::OpenSharedHandle` で開く
//! 4. `RenderingDevice.texture_create_from_extension` で Godot のテクスチャにする
//!
//! 毎フレーム、共有テクスチャを一時的な EGL pbuffer として包み、Servo の FBO を
//! 上下反転しつつ blit する。D3D11 テクスチャ自体は使い回すので、Godot 側の RID は
//! 変わらない。
//!
//! keyed mutex は付けない。ANGLE が自前で作る pbuffer には mozangle の都合で
//! keyed mutex が付くが、Godot の `RenderingDevice` からは acquire/release できない。
//! 自前で確保したテクスチャなら surfman 側の同期方式は `None` になる。

use dpi::PhysicalSize;
use euclid::default::Size2D;
use glow::HasContext;
use godot::classes::rendering_device::{
    DataFormat, TextureSamples, TextureType, TextureUsageBits,
};
use godot::classes::{
    RdTextureFormat, RdTextureView, RenderingDevice, RenderingServer, Texture2D, Texture2Drd,
};
use godot::prelude::*;
use windows::core::{IUnknown, Interface, PCWSTR};
use windows::Win32::Foundation::{CloseHandle, GENERIC_ALL};
use windows::Win32::Graphics::Direct3D11::{
    ID3D11Device, ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE,
    D3D11_RESOURCE_MISC_SHARED, D3D11_RESOURCE_MISC_SHARED_NTHANDLE, D3D11_TEXTURE2D_DESC,
    D3D11_USAGE_DEFAULT,
};
use windows::Win32::Graphics::Direct3D12::{ID3D12Device, ID3D12Resource};
use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_R8G8B8A8_UNORM, DXGI_SAMPLE_DESC};
use windows::Win32::Graphics::Dxgi::IDXGIResource1;

use super::TextureBridge;
use crate::rendering_context::GodotRenderingContext;

pub struct D3d12Bridge {
    /// ANGLE の D3D11 デバイス上の共有テクスチャ。Servo が書き込む側。
    d3d11_texture: ID3D11Texture2D,
    /// 同じメモリを Godot の D3D12 デバイスで開いたもの。所有権保持のために持つ。
    _d3d12_resource: ID3D12Resource,
    /// 共有リソースを Godot の RD テクスチャとして包んだもの。コピー元。
    imported: Rid,
    /// Godot が自分で確保したテクスチャ。コピー先で、こちらを表示に使う。
    owned: Rid,
    texture: Gd<Texture2Drd>,
    size: PhysicalSize<u32>,
}

impl D3d12Bridge {
    pub fn new(
        context: &GodotRenderingContext,
        size: PhysicalSize<u32>,
    ) -> Result<Self, String> {
        let d3d11_device = angle_d3d11_device(context)?;
        let godot_device = super::godot_logical_device()?;

        let (d3d11_texture, d3d12_resource) =
            unsafe { create_shared_texture(&d3d11_device, godot_device, size)? };

        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;

        let imported = import_rd_texture(&mut rendering_device, &d3d12_resource, size)?;
        let owned = create_owned_rd_texture(&mut rendering_device, size)?;

        let mut texture = Texture2Drd::new_gd();
        texture.set_texture_rd_rid(owned);

        Ok(Self {
            d3d11_texture,
            _d3d12_resource: d3d12_resource,
            imported,
            owned,
            texture,
            size,
        })
    }
}

impl TextureBridge for D3d12Bridge {
    fn texture(&self) -> Gd<Texture2D> {
        self.texture.clone().upcast()
    }

    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String> {
        let source_fbo = context.framebuffer();
        let device = context.device();
        let mut gl_context = context.context_mut();

        // 共有テクスチャを ANGLE から見える形 (EGL pbuffer) に一時的に包む。
        let surface_texture = unsafe {
            let raw = self.d3d11_texture.clone().into_raw();
            let com_ptr = wio::com::ComPtr::from_raw(raw as *mut _);
            device
                .create_surface_texture_from_texture(
                    &mut gl_context,
                    &Size2D::new(self.size.width as i32, self.size.height as i32),
                    com_ptr,
                )
                .map_err(|error| format!("create_surface_texture_from_texture: {error:?}"))?
        };

        let gl_texture = device
            .surface_texture_object(&surface_texture)
            .ok_or("ANGLE returned no GL texture for the shared pbuffer")?;

        let blit_result = unsafe {
            blit_flipped(context.glow(), source_fbo, gl_texture, self.size)
        };

        // 包みは毎フレーム捨てる。中身の D3D11 テクスチャは COM 参照で生き続ける。
        let mut surface = device
            .destroy_surface_texture(&mut gl_context, surface_texture)
            .map_err(|(error, _)| format!("destroy_surface_texture: {error:?}"))?;
        device
            .destroy_surface(&mut gl_context, &mut surface)
            .map_err(|error| format!("destroy_surface: {error:?}"))?;
        blit_result?;

        // 取り込んだテクスチャを Godot 所有のテクスチャへ複製する。
        //
        // 遠回りに見えるが必要。Godot の D3D12 ドライバは
        // `texture_create_shared()` で「アロケーションを持つテクスチャ」しか
        // 受け付けず (`_texture_create_shared_from_slice` の DEBUG_ENABLED 判定)、
        // 外部から取り込んだテクスチャは弾かれる。`Texture2DRD` は内部で
        // その共有ビューを作るので、取り込んだテクスチャを直接渡すと白く抜ける。
        // Vulkan ドライバは `created_from_extension` を明示的に許可しているので、
        // これは D3D12 ドライバ側の穴。
        //
        // コピーは GPU 上で完結するので CPU 往復は発生しない。
        let mut rendering_device = RenderingServer::singleton()
            .get_rendering_device()
            .ok_or("no RenderingDevice")?;
        let extent = Vector3::new(self.size.width as f32, self.size.height as f32, 1.0);
        let error = rendering_device.texture_copy(
            self.imported,
            self.owned,
            Vector3::ZERO,
            Vector3::ZERO,
            extent,
            0,
            0,
            0,
            0,
        );
        if error != godot::global::Error::OK {
            return Err(format!("texture_copy failed: {error:?}"));
        }
        Ok(())
    }

    fn backend_name(&self) -> &'static str {
        "d3d12-shared-nt-handle"
    }

    fn release(&mut self) {
        self.texture.set_texture_rd_rid(Rid::Invalid);
        if let Some(mut rendering_device) = RenderingServer::singleton().get_rendering_device() {
            for rid in [&mut self.owned, &mut self.imported] {
                if rid.is_valid() {
                    rendering_device.free_rid(*rid);
                    *rid = Rid::Invalid;
                }
            }
        }
    }
}

/// 共有された `ID3D12Resource` を Godot の RD テクスチャとして包む。コピー元専用。
fn import_rd_texture(
    rendering_device: &mut Gd<RenderingDevice>,
    resource: &ID3D12Resource,
    size: PhysicalSize<u32>,
) -> Result<Rid, String> {
    let rid = rendering_device.texture_create_from_extension(
        TextureType::TYPE_2D,
        DataFormat::R8G8B8A8_UNORM,
        TextureSamples::SAMPLES_1,
        TextureUsageBits::SAMPLING_BIT | TextureUsageBits::CAN_COPY_FROM_BIT,
        resource.as_raw() as u64,
        size.width as u64,
        size.height as u64,
        1,
        1,
    );
    if rid.is_valid() {
        Ok(rid)
    } else {
        Err("texture_create_from_extension returned an invalid RID".into())
    }
}

/// 表示に使う、Godot 所有のテクスチャ。
fn create_owned_rd_texture(
    rendering_device: &mut Gd<RenderingDevice>,
    size: PhysicalSize<u32>,
) -> Result<Rid, String> {
    let mut format = RdTextureFormat::new_gd();
    format.set_format(DataFormat::R8G8B8A8_UNORM);
    format.set_texture_type(TextureType::TYPE_2D);
    format.set_width(size.width);
    format.set_height(size.height);
    format.set_depth(1);
    format.set_array_layers(1);
    format.set_mipmaps(1);
    format.set_samples(TextureSamples::SAMPLES_1);
    format.set_usage_bits(
        TextureUsageBits::SAMPLING_BIT
            | TextureUsageBits::CAN_COPY_TO_BIT
            | TextureUsageBits::CAN_COPY_FROM_BIT,
    );

    let view = RdTextureView::new_gd();
    let rid = rendering_device.texture_create(&format, &view);
    if rid.is_valid() {
        Ok(rid)
    } else {
        Err("texture_create returned an invalid RID".into())
    }
}

/// ANGLE の D3D11 デバイス上に共有テクスチャを作り、Godot の D3D12 デバイスで開く。
///
/// # Safety
///
/// `godot_device` は生存中の `ID3D12Device*` でなければならない。
unsafe fn create_shared_texture(
    d3d11_device: &ID3D11Device,
    godot_device: u64,
    size: PhysicalSize<u32>,
) -> Result<(ID3D11Texture2D, ID3D12Resource), String> {
    let descriptor = D3D11_TEXTURE2D_DESC {
        Width: size.width,
        Height: size.height,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_R8G8B8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC {
            Count: 1,
            Quality: 0,
        },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_RENDER_TARGET.0 | D3D11_BIND_SHADER_RESOURCE.0) as u32,
        CPUAccessFlags: 0,
        // KEYEDMUTEX は付けない。Godot 側で acquire できないため。
        MiscFlags: (D3D11_RESOURCE_MISC_SHARED.0 | D3D11_RESOURCE_MISC_SHARED_NTHANDLE.0) as u32,
    };

    let mut created: Option<ID3D11Texture2D> = None;
    d3d11_device
        .CreateTexture2D(&descriptor, None, Some(&mut created))
        .map_err(|error| format!("D3D11 CreateTexture2D: {error}"))?;
    let d3d11_texture = created.ok_or("D3D11 CreateTexture2D returned null")?;

    let dxgi_resource: IDXGIResource1 = d3d11_texture
        .cast()
        .map_err(|error| format!("cast to IDXGIResource1: {error}"))?;
    let nt_handle = dxgi_resource
        .CreateSharedHandle(None, GENERIC_ALL.0, PCWSTR::null())
        .map_err(|error| format!("DXGI CreateSharedHandle: {error}"))?;

    let device_pointer = godot_device as *mut core::ffi::c_void;
    let d3d12_device: &ID3D12Device = Interface::from_raw_borrowed(&device_pointer)
        .ok_or("Godot returned a null ID3D12Device")?;

    let mut opened: Option<ID3D12Resource> = None;
    let open_result = d3d12_device
        .OpenSharedHandle(nt_handle, &mut opened)
        .map_err(|error| format!("D3D12 OpenSharedHandle: {error}"));

    // 両側で開き終わったので、こちらの複製は閉じてよい。
    let _ = CloseHandle(nt_handle);
    open_result?;

    let d3d12_resource = opened.ok_or("D3D12 OpenSharedHandle returned null")?;
    Ok((d3d11_texture, d3d12_resource))
}

/// Servo の FBO を共有テクスチャへ上下反転して転送する。
///
/// 明示的なセマフォが使えないので、転送後に `glFlush` で同期の代わりとする。
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
    // GL は左下原点、D3D は左上原点。転送のついでに反転する。
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

fn angle_d3d11_device(context: &GodotRenderingContext) -> Result<ID3D11Device, String> {
    let native_device = context.device().native_device();
    if native_device.d3d11_device.is_null() {
        return Err(
            "surfman is not using the ANGLE backend, so there is no D3D11 device. \
             Was the servo `no-wgl` feature enabled?"
                .into(),
        );
    }
    unsafe {
        IUnknown::from_raw(native_device.d3d11_device as *mut _)
            .cast::<ID3D11Device>()
            .map_err(|error| format!("cast to ID3D11Device: {error}"))
    }
}
