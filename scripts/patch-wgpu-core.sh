#!/usr/bin/env bash
# Applies patches/wgpu-core-<version>-android.patch to a copy of the wgpu-core
# crate in the dependency graph, and points cargo at that copy through
# .cargo/config.toml.
#
# wgpu-core as published gives Android no WebGPU adapter; see the WebGPU section
# of the README. CI and the release run this before building for Android, and
# every other build uses wgpu-core as published.
#
# When wgpu-core has moved and the patch no longer applies, this fails. That is
# the signal to rewrite the patch for the new version.
#
# Run locally, it leaves .cargo/config.toml behind, and every cargo command in
# the checkout uses the patched copy until that file is removed.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

crate=$(cargo metadata --format-version 1 --filter-platform aarch64-linux-android | python3 -c '
import json, sys
found = [p for p in json.load(sys.stdin)["packages"] if p["name"] == "wgpu-core"]
if len(found) != 1:
    sys.exit(f"expected one wgpu-core, found {len(found)}")
print(found[0]["version"], found[0]["manifest_path"])')
version=${crate%% *}
dest="$repo_root/target/patched/wgpu-core-$version"

rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
cp -R "$(dirname "${crate#* }")" "$dest"
patch -p1 --fuzz=0 --forward -d "$dest" -i "$repo_root/patches/wgpu-core-$version-android.patch"

mkdir -p .cargo
printf '[patch.crates-io]\nwgpu-core = { path = "%s" }\n' "$dest" > .cargo/config.toml
echo "wgpu-core $version patched at $dest"
