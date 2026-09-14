#!/usr/bin/env bash
# Applies patches/wgpu-core-<version>-android.patch to a copy of the wgpu-core
# crate in the dependency graph, and points cargo at that copy through
# .cargo/config.toml.
#
# wgpu-core as published gives Android no WebGPU adapter; see the WebGPU section
# of the README. CI and the release run this before building for Android, and
# every other build uses wgpu-core as published.
#
# The patch's file name carries the wgpu-core version it is written against.
# wgpu-core comes in through Servo and nothing holds it at that version, so a
# Servo update can move it. When Cargo.lock resolves a different version, this
# fails before patching anything. That is the signal to check whether Android
# still needs the patch (servo/servo#48024): rewrite it for the new version if
# it does, and delete it, this script and the steps that run it if it does not.
#
# Run locally, it leaves .cargo/config.toml behind, and every cargo command in
# the checkout uses the patched copy until that file is removed.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

patches=(patches/wgpu-core-*-android.patch)
if [ ! -f "${patches[0]}" ]; then
	echo "error: no patches/wgpu-core-<version>-android.patch." >&2
	echo "If the patch is gone for good, remove this script and its steps in ci.yaml and release.yaml." >&2
	exit 1
fi
if [ "${#patches[@]}" -ne 1 ]; then
	echo "error: more than one patches/wgpu-core-*-android.patch; keep only the current one." >&2
	exit 1
fi
expected=${patches[0]#patches/wgpu-core-}
expected=${expected%-android.patch}

crate=$(cargo metadata --format-version 1 --filter-platform aarch64-linux-android | python3 -c '
import json, sys
found = [p for p in json.load(sys.stdin)["packages"] if p["name"] == "wgpu-core"]
if len(found) != 1:
    sys.exit(f"expected one wgpu-core, found {len(found)}")
print(found[0]["version"], found[0]["manifest_path"])')
version=${crate%% *}

if [ "$version" != "$expected" ]; then
	echo "error: Cargo.lock resolves wgpu-core $version, but ${patches[0]} is written for $expected." >&2
	echo "Check whether Android gets a WebGPU adapter without the patch (servo/servo#48024)." >&2
	echo "If it does, delete the patch, this script and its steps in ci.yaml and release.yaml." >&2
	echo "If it does not, rewrite the patch for $version and rename it to match." >&2
	exit 1
fi

dest="$repo_root/target/patched/wgpu-core-$version"
rm -rf "$dest"
mkdir -p "$(dirname "$dest")"
cp -R "$(dirname "${crate#* }")" "$dest"
patch -p1 --fuzz=0 --forward -d "$dest" -i "$repo_root/${patches[0]}"

mkdir -p .cargo
printf '[patch.crates-io]\nwgpu-core = { path = "%s" }\n' "$dest" > .cargo/config.toml
echo "wgpu-core $version patched at $dest"
