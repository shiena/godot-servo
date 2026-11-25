//! Select the bundled plugins before Servo initializes its GStreamer backend.

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn self_directory() -> Option<std::path::PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;
    unsafe {
        let mut info = std::mem::MaybeUninit::uninit();
        if libc::dladdr(self_directory as *const () as _, info.as_mut_ptr()) == 0 {
            return None;
        }
        let name = info.assume_init().dli_fname;
        if name.is_null() {
            return None;
        }
        let path = Path::new(OsStr::from_bytes(CStr::from_ptr(name).to_bytes()));
        path.parent().map(Path::to_path_buf)
    }
}

pub fn initialize() -> bool {
    #[cfg(windows)]
    let directory = crate::angle_loader::self_directory();
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    let directory = self_directory();
    #[cfg(any(windows, target_os = "linux", target_os = "macos"))]
    if let Some(plugins) = directory
        .map(|dir| dir.join("gst-plugins"))
        .filter(|dir| dir.is_dir())
    {
        #[cfg(windows)]
        {
            // Windows also resolves delay-loaded dependencies when a plugin starts.
            let runtime = plugins.parent().unwrap();
            std::env::set_var(
                "GST_PLUGIN_SCANNER_1_0",
                runtime.join("gst-plugin-scanner.exe"),
            );
            let mut paths = vec![runtime.to_path_buf()];
            paths.extend(std::env::split_paths(
                &std::env::var_os("PATH").unwrap_or_default(),
            ));
            if let Ok(path) = std::env::join_paths(paths) {
                std::env::set_var("PATH", path);
            }
        }
        std::env::set_var("GST_PLUGIN_SYSTEM_PATH_1_0", plugins);
    }
    if let Err(error) = gstreamer::init() {
        godot::global::godot_error!("godot-servo: GStreamer initialization failed: {error}");
        return false;
    }
    true
}
