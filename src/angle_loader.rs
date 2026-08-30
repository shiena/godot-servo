//! Loads the ANGLE DLLs from beside this extension, ahead of anyone asking for them.
//!
//! Surfman picks up ANGLE with `LoadLibraryA("libEGL.dll")`, by name alone. A
//! GDExtension's DLL lives in its own folder rather than next to the Godot
//! binary, so that name is not on the search path and startup fails with
//! `Unable to load the libEGL shared object`.
//!
//! Calling `LoadLibraryW` with the absolute path first means the later
//! by-name request resolves to the module that is already loaded.

#[cfg(windows)]
pub fn preload() {
    use std::os::windows::ffi::OsStrExt;
    use std::path::PathBuf;

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::HMODULE;
    use windows::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, LoadLibraryW,
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS, GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };

    let Some(directory) = self_directory() else {
        return;
    };

    for name in ["libEGL.dll", "libGLESv2.dll"] {
        let path = directory.join(name);
        if !path.exists() {
            godot::global::godot_warn!(
                "godot-servo: {name} is missing next to the extension; \
                 Servo will not be able to start. Copy it from \
                 target/<profile>/{name}."
            );
            continue;
        }
        let wide: Vec<u16> = path
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        if unsafe { LoadLibraryW(PCWSTR(wide.as_ptr())) }.is_err() {
            godot::global::godot_warn!("godot-servo: failed to preload {name}");
        }
    }

    /// The folder this DLL itself sits in.
    fn self_directory() -> Option<PathBuf> {
        unsafe {
            let mut module = HMODULE::default();
            let anchor = self_directory as *const () as *const u16;
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS
                    | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                PCWSTR(anchor),
                &mut module,
            )
            .ok()?;

            let mut buffer = [0u16; 32768];
            let length = GetModuleFileNameW(Some(module), &mut buffer) as usize;
            if length == 0 {
                return None;
            }
            let path = PathBuf::from(String::from_utf16_lossy(&buffer[..length]));
            path.parent().map(PathBuf::from)
        }
    }
}

#[cfg(not(windows))]
pub fn preload() {}
