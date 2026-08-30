//! Servo が描いた内容を Godot のテクスチャに渡す経路。
//!
//! GPU リソースを直接共有できるバックエンドではそうし、できなければ CPU 経由の
//! フォールバックに落ちる。どちらを使ったかは `backend_name()` で確認できる。

use dpi::PhysicalSize;
use godot::classes::{RenderingServer, Texture2D};
use godot::prelude::*;

use crate::rendering_context::GodotRenderingContext;

pub mod cpu;

#[cfg(windows)]
pub mod d3d12;

#[cfg(target_os = "macos")]
pub mod metal;

pub trait TextureBridge {
    /// Godot 側に渡すテクスチャ。生存期間中は同じインスタンスを返し続ける。
    fn texture(&self) -> Gd<Texture2D>;

    /// Servo の `paint()` 直後、`present()` の前に呼ぶ。
    fn update(&mut self, context: &GodotRenderingContext) -> Result<(), String>;

    fn backend_name(&self) -> &'static str;

    /// テクスチャが上下反転しているか。
    ///
    /// GL は左下原点なので、途中で転送を挟む経路では転送時に直せる。IOSurface を
    /// そのまま共有する macOS だけは直す場所がないため `true` を返す。
    fn needs_v_flip(&self) -> bool {
        false
    }

    /// ノードが木から外れるときに呼ぶ。Godot の RID を解放する。
    fn release(&mut self) {}
}

/// このプロセスで GPU 共有が使えるかを判定して、適切な橋を作る。
///
/// GPU 経路の初期化に失敗した場合は警告を出して CPU 経路に落ちる。
/// 「対応レンダラでないと起動しない拡張」にはしない。
pub fn create(
    context: &GodotRenderingContext,
    size: PhysicalSize<u32>,
) -> Box<dyn TextureBridge> {
    let driver = RenderingServer::singleton()
        .get_current_rendering_driver_name()
        .to_string();

    match try_create_shared(&driver, context, size) {
        Some(Ok(bridge)) => return bridge,
        Some(Err(error)) => {
            godot_warn!(
                "godot-servo: GPU texture sharing on the '{driver}' backend failed \
                 ({error}); falling back to CPU readback."
            );
        }
        None => {
            godot_print!(
                "godot-servo: no GPU sharing path for the '{driver}' backend; \
                 using CPU readback."
            );
        }
    }

    Box::new(cpu::CpuBridge::new(size))
}

/// 共有経路がある組み合わせなら `Some`、そもそも無ければ `None`。
#[allow(unused_variables)]
fn try_create_shared(
    driver: &str,
    context: &GodotRenderingContext,
    size: PhysicalSize<u32>,
) -> Option<Result<Box<dyn TextureBridge>, String>> {
    #[cfg(windows)]
    if driver == "d3d12" {
        return Some(
            d3d12::D3d12Bridge::new(context, size)
                .map(|bridge| Box::new(bridge) as Box<dyn TextureBridge>),
        );
    }

    #[cfg(target_os = "macos")]
    if driver == "metal" {
        return Some(
            metal::MetalBridge::new(context, size)
                .map(|bridge| Box::new(bridge) as Box<dyn TextureBridge>),
        );
    }

    None
}

/// GPU 経路の橋が共通で使う、Godot の論理デバイスハンドル。
///
/// Vulkan なら `VkDevice`、D3D12 なら `ID3D12Device*`、Metal なら `id<MTLDevice>`。
#[cfg(any(windows, target_os = "macos"))]
pub(crate) fn godot_logical_device() -> Result<u64, String> {
    use godot::classes::rendering_device::DriverResource;

    let rendering_device = RenderingServer::singleton()
        .get_rendering_device()
        .ok_or("no RenderingDevice (the Compatibility renderer has none)")?;

    let handle =
        rendering_device.get_driver_resource(DriverResource::LOGICAL_DEVICE, Rid::Invalid, 0);
    if handle == 0 {
        return Err("get_driver_resource returned a null logical device".into());
    }
    Ok(handle)
}
