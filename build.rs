//! ビルド後に ANGLE の DLL を成果物の隣へ置く。
//!
//! surfman は実行時に `LoadLibraryA("libEGL.dll")` で ANGLE を掴む。mozangle が
//! ビルドした DLL は依存クレートの `OUT_DIR` の中にしか置かれず、cargo は成果物の
//! 隣にコピーしてくれない。放っておくと実行時に
//! `Unable to load the libEGL shared object` で落ちる。

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    #[cfg(windows)]
    copy_angle_dlls();
}

#[cfg(windows)]
fn copy_angle_dlls() {
    use std::path::PathBuf;

    let Ok(out_dir) = std::env::var("OUT_DIR") else {
        return;
    };
    let out_dir = PathBuf::from(out_dir);

    // OUT_DIR は target/<profile>/build/<crate>-<hash>/out なので 3 つ上がプロファイル。
    let Some(profile_dir) = out_dir.ancestors().nth(3) else {
        return;
    };
    let build_dir = profile_dir.join("build");
    let Ok(entries) = std::fs::read_dir(&build_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !name.starts_with("mozangle-") {
            continue;
        }
        let source_dir = entry.path().join("out");
        for dll in ["libEGL.dll", "libGLESv2.dll"] {
            let source = source_dir.join(dll);
            if !source.exists() {
                continue;
            }
            let destination = profile_dir.join(dll);
            if let Err(error) = std::fs::copy(&source, &destination) {
                println!("cargo:warning=could not copy {dll}: {error}");
            }
        }
    }
}
