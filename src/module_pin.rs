//! Keeps this module mapped for the life of the process.
//!
//! Godot unloads a GDExtension's library before the process exits:
//! `unregister_core_types()` deletes the `GDExtensionManager`, that drops the
//! last `Ref<GDExtension>`, and `~GDExtension` calls `close_library()`. It is
//! entitled to. Unloading a library with a thread still running in it is
//! undefined on every platform, and an extension is supposed to have stopped
//! its threads by then.
//!
//! This one cannot. `ServoServer` shuts Servo down when the extension leaves
//! the `Scene` level, before the unload, but Servo's shutdown does not join
//! everything it started. Rayon's global pool, `GlobalPool#0` and on, has no
//! shutdown API and stays parked for the life of the process. And Servo 0.5.0
//! never tells its WebGPU thread to exit: `Constellation::handle_shutdown`
//! looks for the channels in the browsing context group that closing the last
//! webview has already removed.
//!
//! Parked threads survive an unload, because they never touch their own code
//! again. A working one does not: once the leaked `WGPU poller` is among them,
//! the process ends in an access violation at exit.
//!
//! Nothing is given up by pinning. `godot_servo.gdextension` already sets
//! `reloadable = false`, because Servo can only be initialised once per process.

/// Pins the module. Safe to call more than once.
///
/// `GODOT_SERVO_NO_PIN=1` skips it, which is how the crash it prevents can be
/// reproduced again.
pub fn pin() {
    if std::env::var("GODOT_SERVO_NO_PIN").as_deref() == Ok("1") {
        godot::prelude::godot_warn!(
            "godot-servo: module not pinned (GODOT_SERVO_NO_PIN); Godot will \
             unload this library while Servo's threads are still in it"
        );
        return;
    }
    pin_impl();
}

#[cfg(windows)]
fn pin_impl() {
    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_PIN,
    };

    // SAFETY: `pin_impl` is an address inside this module, which is what
    // GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS wants. The handle is discarded:
    // pinning is the whole point of the call.
    let pinned = unsafe {
        let mut module = HMODULE::default();
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_PIN,
            PCWSTR(pin_impl as *const () as *const u16),
            &mut module,
        )
        .is_ok()
    };
    if !pinned {
        godot::prelude::godot_warn!(
            "godot-servo: could not pin the module; the process may fault at exit"
        );
    }
}

/// Nothing to do yet.
///
/// On macOS dyld does not unload a dylib that has thread-local variables, and
/// this one has them, so `dlclose` leaves it mapped anyway. Linux and Android
/// have not been measured; `dlopen`ing this library again with `RTLD_NODELETE`
/// is what would pin it there.
#[cfg(not(windows))]
fn pin_impl() {}
