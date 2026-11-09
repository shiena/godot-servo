#!/usr/bin/env bash
# Applies patches/wgpu-types-<version>-android.patch to a copy of the wgpu-types
# crate in the dependency graph, and points cargo at that copy through
# .cargo/config.toml.
#
# wgpu-types as published gives Android no WebGPU adapter; see the WebGPU section
# of the README. CI and the release run this before building for Android, and
# every other build uses wgpu-types as published.
#
# The patch's file name carries the wgpu-types version it is written against.
# wgpu-types comes in through Servo and nothing holds it at that version, so a
# Servo update can move it. When Cargo.lock resolves a different version, this
# fails before patching anything. That is the signal to check whether Android
# still needs the patch (gfx-rs/wgpu#10425): rewrite it for the new version if
# it does, and delete it, this script and the steps that run it if it does not.
#
# Run locally, it leaves .cargo/config.toml behind, and every cargo command in
# the checkout uses the patched copy until that file is removed.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

patches=(patches/wgpu-types-*-android.patch)
if [ ! -f "${patches[0]}" ]; then
	echo "error: no patches/wgpu-types-<version>-android.patch." >&2
	echo "If the patch is gone for good, remove this script and its steps in ci.yaml and release.yaml." >&2
	exit 1
fi
if [ "${#patches[@]}" -ne 1 ]; then
	echo "error: more than one patches/wgpu-types-*-android.patch; keep only the current one." >&2
	exit 1
fi
expected=${patches[0]#patches/wgpu-types-}
expected=${expected%-android.patch}

crate=$(cargo metadata --format-version 1 --filter-platform aarch64-linux-android | python3 -c '
import json, sys
found = [p for p in json.load(sys.stdin)["packages"] if p["name"] == "wgpu-types"]
if len(found) != 1:
    sys.exit(f"expected one wgpu-types, found {len(found)}")
print(found[0]["version"], found[0]["manifest_path"])')
version=${crate%% *}

if [ "$version" != "$expected" ]; then
	echo "error: Cargo.lock resolves wgpu-types $version, but ${patches[0]} is written for $expected." >&2
	echo "Check whether Android gets a WebGPU adapter without the patch (gfx-rs/wgpu#10425)." >&2
	echo "If it does, delete the patch, this script and its steps in ci.yaml and release.yaml." >&2
	echo "If it does not, rewrite the patch for $version and rename it to match." >&2
	exit 1
fi

dest="$repo_root/target/patched/wgpu-types-$version"
rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
cp -R "$(dirname "${crate#* }")" "$dest"
patch -p1 --fuzz=0 --forward -d "$dest" -i "$repo_root/${patches[0]}"

mkdir -p .cargo
printf '[patch.crates-io]\nwgpu-types = { path = "%s" }\n' "$dest" > .cargo/config.toml
echo "wgpu-types $version patched at $dest"
