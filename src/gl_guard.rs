//! Holds Godot's GL context aside while Servo borrows the thread.
//!
//! Servo keeps its own GL context through surfman and makes it current every
//! time it draws. Where Godot renders with Vulkan, D3D12 or Metal, nothing else
//! on the thread wants a GL context, so keeping it costs nothing.
//!
//! Android's Compatibility (GLES3) renderer is the exception: Godot draws from
//! its own EGL context on the same thread. Borrowing without giving it back
//! leaves Godot compiling shaders and drawing against Servo's context, which
//! either corrupts the frame or crashes.
//!
//! `HostContext::capture()` records whatever is current; drop restores it.

/// The GL context that was current before the loan. Restored on drop.
pub struct HostContext(imp::Saved);

impl HostContext {
    /// Record the GL context that is current right now.
    pub fn capture() -> Self {
        Self(imp::Saved::capture())
    }

    /// Restore the recorded context immediately, rather than waiting for drop.
    /// Use it before calling into Godot's own GL. Safe to call repeatedly.
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
            // SAFETY: queries only. EGL returns null when nothing is current.
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
            // SAFETY: puts back exactly the values that were recorded.
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
    /// Off Android, Godot holds no GL context, so there is nothing to save.
    pub struct Saved;

    impl Saved {
        pub fn capture() -> Self {
            Self
        }

        pub fn restore(&self) {}
    }
}
