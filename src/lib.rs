//! Servo (Rust 製ブラウザエンジン) を Godot の GDExtension として組み込む。
//!
//! Servo は ANGLE / CGL のオフスクリーンサーフェスに描画し、その GPU リソースを
//! Godot のテクスチャとして直接共有する。共有できないバックエンドでは
//! CPU 経由のフォールバックに落ちる。
//!
//! | バックエンド | 経路 |
//! |---|---|
//! | Windows / D3D12 | ANGLE D3D11 共有テクスチャ (NT ハンドル) → `ID3D12Resource` |
//! | macOS / Metal | IOSurface → `MTLTexture` |
//! | その他 | `glReadPixels` → `ImageTexture` |

use godot::prelude::*;

pub mod angle_loader;
pub mod bridge;
pub mod delegate;
pub mod rendering_context;
pub mod waker;
pub mod webview_node;

struct GodotServo;

#[gdextension]
unsafe impl ExtensionLibrary for GodotServo {}
