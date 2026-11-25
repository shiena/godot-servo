"""Link the Android SDK archives once and expose their shared library to Cargo."""
import os
from pathlib import Path
import re
import subprocess
import sys

root, dest = map(Path, sys.argv[1:3])
abi = sys.argv[3]
ndk = Path(os.environ["ANDROID_NDK_HOME"])
makefile = Path(__file__).with_name("android-media") / "Android.mk"
dest.mkdir(parents=True, exist_ok=True)
subprocess.run([str(ndk / "ndk-build"), "-j1", f"NDK_PROJECT_PATH={dest}", f"APP_BUILD_SCRIPT={makefile.resolve()}",
                f"APP_ABI={abi}", "APP_PLATFORM=android-24", "APP_STL=c++_shared", "APP_MODULES=gstreamer_android",
                f"GSTREAMER_ROOT={root}", f"GSTREAMER_JAVA_SRC_DIR={dest}/java",
                f"GSTREAMER_ASSETS_DIR={dest}/assets"], cwd=dest, check=True)
lib = dest / "libs" / abi
if not (lib / "libgstreamer_android.so").exists():
    raise RuntimeError("The GStreamer SDK did not produce libgstreamer_android.so")
pcdir = dest / "pkgconfig"
pcdir.mkdir(exist_ok=True)
for pc in (root / "lib/pkgconfig").glob("*.pc"):
    if pc.stem.startswith("gstreamer") or pc.stem in ("glib-2.0", "gobject-2.0", "gio-2.0", "gmodule-2.0"):
        version = re.search(r"^Version:\s*(.+)$", pc.read_text(), re.MULTILINE).group(1)
        (pcdir / pc.name).write_text(f"Name: {pc.stem}\nDescription: Android SDK\nVersion: {version}\nLibs: -L{lib} -lgstreamer_android\n")
# EGL entry points are provided by libgstgl in the Android universal SDK.
(pcdir / "gstreamer-gl-egl-1.0.pc").write_bytes((pcdir / "gstreamer-gl-1.0.pc").read_bytes())
