//! ANGLE の DLL を、この拡張と同じフォルダから先に読み込んでおく。
//!
//! surfman は ANGLE を `LoadLibraryA("libEGL.dll")` で名前だけ指定して掴む。
//! GDExtension の DLL は Godot 本体とは別のフォルダに置かれるため、そのままでは
//! 検索パスに乗らず `Unable to load the libEGL shared object` で落ちる。
//!
//! 先に絶対パスで `LoadLibraryW` しておけば、あとから名前で要求されても
//! 読み込み済みのモジュールが返る。

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

    /// 自分自身 (この DLL) が置かれているフォルダ。
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
