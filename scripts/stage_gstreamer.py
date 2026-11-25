"""Stage the playback runtime with the extension and describe it for Godot export."""
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

PLUGINS = """coreelements app audioconvert audioresample audiomixer audioparsers
audiofx autodetect gio id3demux isomp4 matroska ogg opus playback typefindfunctions
videoconvertscale videofilter videorate videoparsersbad volume vorbis wavparse
flac mpg123 libav vpx theora deinterlace interleave nice dtls rtp rtpmanager
srtp webrtc opensles androidmedia wasapi wasapi2 mediafoundation osxaudio applemedia
pulseaudio alsa pipewire audiobuffersplit proxy id3tag opengl""".split()


# The desktop may already have loaded different versions of these libraries.
# Give the SDK copies private SONAMEs and update every bundled consumer.
PRIVATE_LINUX_LIBS = {name: "libgodot-servo-" + name[3:] for name in (
    "libSPIRV-Tools.so", "libSPIRV-Tools-opt.so", "libglib-2.0.so.0",
    "libgobject-2.0.so.0", "libgio-2.0.so.0", "libgmodule-2.0.so.0",
    "libgthread-2.0.so.0",
)}


def run(*args):
    return subprocess.check_output(args, text=True).strip()


def stage(platform, sdk, dest):
    plugins = dest / "gst-plugins"
    plugins.mkdir(parents=True, exist_ok=True)
    suffix = {"windows": ".dll", "macos": ".dylib", "linux": ".so"}[platform]
    prefix = "" if platform == "windows" else "lib"
    if platform == "linux":
        # Remove obsolete SDK copies on an incremental build.
        for name in PRIVATE_LINUX_LIBS:
            (dest / name).unlink(missing_ok=True)
    for name in PLUGINS:
        plugin = sdk / "lib/gstreamer-1.0" / (prefix + "gst" + name + suffix)
        if plugin.exists():
            shutil.copy2(plugin, plugins)
        elif name in ("coreelements", "app", "playback", "audioconvert", "isomp4"):
            raise RuntimeError(f"Missing required plugin: {plugin}")
    if platform == "windows":
        for dll in (sdk / "bin").glob("*.dll"):
            if dll.name.lower() not in ("libegl.dll", "libglesv2.dll"):
                shutil.copy2(dll, dest)
    else:
        extension = next(dest.glob("libgodot_servo*"))
        pending = [extension, *plugins.glob("*" + suffix)]
        visited = set()
        while pending:
            binary = pending.pop()
            if binary in visited:
                continue
            visited.add(binary)
            if platform == "macos":
                refs = [line.strip().split(" (")[0] for line in run("otool", "-L", binary).splitlines()[1:]]
            else:
                refs = [line.split("Shared library: [")[1].split("]")[0]
                        for line in run("readelf", "-d", binary).splitlines() if "Shared library: [" in line]
            for ref in refs:
                name = Path(ref).name
                lib = sdk / "lib" / name
                if platform == "linux" and not lib.is_file():
                    lib = sdk / "lib/pulseaudio" / name
                if not lib.is_file():
                    continue
                staged_name = PRIVATE_LINUX_LIBS.get(name, name) if platform == "linux" else name
                target = dest / staged_name
                if staged_name != name:
                    subprocess.run(["patchelf", "--replace-needed", name, staged_name, binary], check=True)
                if target != binary and target not in visited and target not in pending:
                    shutil.copy2(lib, target)
                    if staged_name != name:
                        subprocess.run(["patchelf", "--set-soname", staged_name, target], check=True)
                    pending.append(target)
                if platform == "macos" and target != binary:
                    rel = os.path.relpath(target, binary.parent)
                    subprocess.run(["install_name_tool", "-change", ref, f"@loader_path/{rel}", binary], check=True)
            if platform == "macos":
                if binary != extension:
                    subprocess.run(["install_name_tool", "-id", "@rpath/" + binary.name, binary], check=True)
                subprocess.run(["codesign", "--force", "--sign", "-", binary], check=True)
            else:
                rpath = "$ORIGIN/.." if binary.parent == plugins else "$ORIGIN"
                subprocess.run(["patchelf", "--set-rpath", rpath, binary], check=True)

    licenses = sdk / "share/licenses"
    if licenses.is_dir():
        shutil.copytree(licenses, dest / "licenses", dirs_exist_ok=True)
    if platform == "linux":
        for pkg in (sdk / "conda-meta").glob("*.json"):
            for item in json.loads(pkg.read_text()).get("files", []):
                if "license" in item.lower() and (sdk / item).is_file():
                    target = dest / "licenses" / pkg.stem / Path(item).name
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copy2(sdk / item, target)
    deps = {p.name: "" for p in dest.iterdir()
            if p.is_file() and p.name != "export-deps.json" and "godot_servo" not in p.name}
    plugin_target = "Contents/Frameworks/gst-plugins" if platform == "macos" else "gst-plugins"
    deps.update({f"gst-plugins/{p.name}": plugin_target for p in plugins.glob("*" + suffix)})
    (dest / "export-deps.json").write_text(json.dumps(deps, indent=2, sort_keys=True))


def manifest(file, bins):
    text = Path(__file__).with_name("godot_servo.gdextension.in").read_text().rstrip() + "\n"
    for platform in ("windows", "macos", "linux"):
        deps = bins / platform / "export-deps.json"
        if deps.exists():
            key = platform if platform == "macos" else platform + ".x86_64"
            mapping = {f"res://addons/godot_servo/bin/{platform}/{k}": v
                       for k, v in json.loads(deps.read_text()).items()}
            text += key + " = " + json.dumps(mapping, indent=2) + "\n"
    file.write_text(text)


if __name__ == "__main__":
    if sys.argv[1] == "manifest":
        manifest(Path(sys.argv[2]), Path(sys.argv[3]))
    else:
        stage(sys.argv[1], Path(sys.argv[2]), Path(sys.argv[3]))
