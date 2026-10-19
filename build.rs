//! Strips release builds of the library through the linker.
//!
//! Cargo's `strip` does this on Linux and Android, but on Apple targets rustc
//! strips by rewriting the linked library with its bundled rust-objcopy, and
//! what that makes of Xcode 27's linker output no longer loads ("mis-aligned
//! LINKEDIT string pool"). So `strip` is off in Cargo.toml, and the linker
//! drops the symbols on every target that has any to drop.

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("PROFILE").as_deref() != Ok("release") {
        return;
    }
    let os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let vendor = std::env::var("CARGO_CFG_TARGET_VENDOR").unwrap_or_default();
    if vendor == "apple" {
        // ld64 leaves out every symbol that is not exported.
        println!("cargo:rustc-link-arg-cdylib=-Wl,-x");
    } else if os == "linux" || os == "android" {
        // What rustc passes for `strip = true` on these targets.
        println!("cargo:rustc-link-arg-cdylib=-Wl,--strip-all");
    }
    // Windows keeps its symbols in a separate .pdb, so the DLL has none.
}
