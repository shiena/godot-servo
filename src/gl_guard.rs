//! Servo に GL コンテキストを貸している間、Godot のものを退避しておく。
//!
//! Servo は surfman 経由で自前の GL コンテキストを持ち、描画のたびにそれを
//! カレントにする。Godot が Vulkan / D3D12 / Metal で描いている環境ではスレッドの
//! GL コンテキストを誰も使っていないので、奪いっぱなしでも実害が無い。
//!
//! Android の Compatibility (GLES3) レンダラだけは事情が違う。Godot 自身が同じ
//! スレッドで EGL コンテキストを持って描画しているため、返さずにいると Godot が
//! Servo のコンテキストに向かって描画・シェーダコンパイルを始めてしまう。
//! 実際 Beam Pro では `CopyShaderGLES3: Program linking failed` のあと、Godot の
//! 描画中に SIGSEGV していた。
//!
//! `HostContext::capture()` で今カレントなものを控え、drop で戻す。

/// 借りる前の GL コンテキストを控えておくもの。drop で元に戻す。
pub struct HostContext(imp::Saved);

impl HostContext {
    /// いまカレントな GL コンテキストを控える。
    pub fn capture() -> Self {
        Self(imp::Saved::capture())
    }

    /// 控えたコンテキストへ今すぐ戻す。drop を待たずに Godot 側の GL を呼びたい
    /// ときに使う。何度呼んでもよい。
    pub fn restore(&self) {
        self.0.restore();
    }
}

#[cfg(target_os = "android")]
mod imp {
    use std::ffi::c_void;

    const EGL_DRAW: i32 = 0x3059;
    const EGL_READ: i32 = 0x305A;

    #[link(name = "EGL")]
    unsafe extern "C" {
        fn eglGetCurrentDisplay() -> *mut c_void;
        fn eglGetCurrentSurface(readdraw: i32) -> *mut c_void;
        fn eglGetCurrentContext() -> *mut c_void;
        fn eglMakeCurrent(
            display: *mut c_void,
            draw: *mut c_void,
            read: *mut c_void,
            context: *mut c_void,
        ) -> u32;
    }

    pub struct Saved {
        display: *mut c_void,
        draw: *mut c_void,
        read: *mut c_void,
        context: *mut c_void,
    }

    impl Saved {
        pub fn capture() -> Self {
            // SAFETY: EGL の問い合わせのみ。カレントが無ければ null が返る。
            unsafe {
                Self {
                    display: eglGetCurrentDisplay(),
                    draw: eglGetCurrentSurface(EGL_DRAW),
                    read: eglGetCurrentSurface(EGL_READ),
                    context: eglGetCurrentContext(),
                }
            }
        }
    }

    impl Saved {
        pub fn restore(&self) {
            if self.display.is_null() || self.context.is_null() {
                return;
            }
            // SAFETY: 控えた値をそのまま戻すだけ。
            unsafe {
                eglMakeCurrent(self.display, self.draw, self.read, self.context);
            }
        }
    }

    impl Drop for Saved {
        fn drop(&mut self) {
            self.restore();
        }
    }
}

#[cfg(not(target_os = "android"))]
mod imp {
    /// Android 以外では Godot が GL コンテキストを持たないので、何もしない。
    pub struct Saved;

    impl Saved {
        pub fn capture() -> Self {
            Self
        }

        pub fn restore(&self) {}
    }
}
