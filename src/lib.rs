//! Embeds Servo, the Rust browser engine, into Godot as a GDExtension.
//!
//! Servo renders into an offscreen ANGLE / CGL surface, and that GPU resource is
//! shared with Godot as a texture. Backends with no sharing path fall back to a
//! CPU readback.
//!
//! | Backend | Path |
//! |---|---|
//! | Windows / D3D12 | ANGLE D3D11 shared texture (NT handle) to `ID3D12Resource` |
//! | Windows / Vulkan | ANGLE D3D11 shared texture (NT handle) to `VkImage` |
//! | macOS / Metal | IOSurface to `MTLTexture` |
//! | macOS / Vulkan | IOSurface to `VkImage`, through MoltenVK |
//! | Android / Compatibility | `AHardwareBuffer` to `EGLImage` to `ExternalTexture` |
//! | Android / Forward+, Mobile | `VkImage` to opaque fd to a GL memory object |
//! | Linux / Vulkan | `VkImage` to opaque fd to a GL memory object |
//! | anything else | `glReadPixels` to `ImageTexture` |
//!
//! The Windows and macOS Vulkan paths need a device extension that only a
//! project can enable, so they check for that first and fall back to the
//! readback where it is absent; Linux and Android need nothing Godot does not
//! already enable. [`bridge::vulkan::caps`] has the detail.

use godot::prelude::*;

pub mod angle_loader;
pub mod bridge;
pub mod delegate;
pub mod gl_guard;
pub mod rendering_context;
pub mod waker;
pub mod webview_node;

// Never called directly. The dependency exists only to change jemalloc's TLS
// model on Linux (see the note in Cargo.toml).
#[cfg(target_os = "linux")]
use tikv_jemalloc_sys as _;

struct GodotServo;

#[gdextension]
unsafe impl ExtensionLibrary for GodotServo {}
