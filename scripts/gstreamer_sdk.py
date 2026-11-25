"""Fetch pinned GStreamer SDKs and verify the bytes before installing them."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile

LOCK = json.loads(Path(__file__).with_name("gstreamer-sdk.json").read_text())
VERSION = LOCK["version"]


def digest(path):
    with path.open("rb") as stream:
        value = hashlib.sha256()
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            value.update(block)
        return value.hexdigest()


def fetch(asset, cache):
    url = asset["url"]
    name = url.rsplit("/", 1)[1]
    path = cache / name
    if path.is_file() and digest(path) == asset["sha256"]:
        return path
    mirror = f"https://github.com/{LOCK['mirror']}/releases/download/gstreamer-{VERSION}/{name}"
    with tempfile.NamedTemporaryFile(dir=cache, delete=False) as handle:
        partial = Path(handle.name)
    try:
        for location in dict.fromkeys([mirror, url]):
            print(f"Downloading {location}", file=sys.stderr)
            result = subprocess.run(["curl", "--fail", "--location", "--silent", "--show-error",
                                     "--retry", "3", "--output", str(partial), location])
            if result.returncode == 0:
                if digest(partial) != asset["sha256"]:
                    raise ValueError(f"SHA-256 mismatch for {name}")
                os.replace(partial, path)
                return path
        raise RuntimeError(f"Could not download {name}")
    finally:
        partial.unlink(missing_ok=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("platform", choices=LOCK["assets"])
    parser.add_argument("--download-only", action="store_true")
    args = parser.parse_args()
    cache = Path(os.environ.get("GODOT_SERVO_SDK_CACHE", Path.home() / ".cache" / "godot-servo")) / VERSION
    cache.mkdir(parents=True, exist_ok=True)
    files = [fetch(asset, cache) for asset in LOCK["assets"][args.platform]]
    if args.download_only:
        print(cache)
        return
    root = cache / args.platform
    marker = root / ".installed"
    if args.platform == "macos":
        root = Path("/Library/Frameworks/GStreamer.framework/Versions/1.0")
        marker = cache / ".macos-installed"
    if not marker.exists() or not root.exists():
        if args.platform == "windows":
            subprocess.run([str(files[0]), "/VERYSILENT", "/SUPPRESSMSGBOXES", "/NORESTART",
                            "/CURRENTUSER", "/TYPE=devel", f"/DIR={root}"], check=True)
        elif args.platform == "macos":
            for path in files:
                subprocess.run(["sudo", "installer", "-pkg", str(path), "-target", "/"], check=True, stdout=sys.stderr)
        else:
            root.mkdir(parents=True, exist_ok=True)
            subprocess.run(["tar", "-xf", str(files[0]), "-C", str(root)], check=True)
        if args.platform == "linux":
            subprocess.run([str(root / "bin/python"), str(root / "bin/conda-unpack")], check=True)
        marker.touch()
    print(root)


if __name__ == "__main__":
    main()
