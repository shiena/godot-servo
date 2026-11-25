#!/usr/bin/env bash
# The local development pipeline for godot-servo (Linux / macOS).
# The counterpart of build.ps1.
#
# Usage:
#   ./scripts/build.sh                    # build and stage (debug)
#   ./scripts/build.sh --release
#   ./scripts/build.sh --run              # run the demo against what is staged
#   ./scripts/build.sh --run --scene flat
#   ./scripts/build.sh --test             # input and signal self check
#   ./scripts/build.sh --run --page webgl
#   ./scripts/build.sh --checks           # also run fmt and clippy
#   ./scripts/build.sh --android          # Android (arm64-v8a), needs cargo-ndk
#   ./scripts/build.sh --android --abi x86_64
#
# With no stage given, it only builds.

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile=debug
do_build=1
do_run=0
do_test=0
do_checks=0
scene=main
android=0
abi=arm64-v8a
page=""
screenshot=""
quit_after=0
godot="${GODOT:-godot}"

while [ $# -gt 0 ]; do
	case "$1" in
		--release)    profile=release ;;
		--run)        do_run=1 ;;
		--test)       do_test=1 ;;
		--checks)     do_checks=1 ;;
		--no-build)   do_build=0 ;;
		--android)    android=1 ;;
		--abi)        abi="$2"; shift ;;
		--scene)      scene="$2"; shift ;;
		--page)       page="$2"; shift ;;
		--screenshot) screenshot="$2"; shift ;;
		--quit-after) quit_after="$2"; shift ;;
		*) echo "unknown option: $1" >&2; exit 2 ;;
	esac
	shift
done

say() { printf '\033[36m>> %s\033[0m\n' "$1"; }
ok()  { printf '\033[32m%s\033[0m\n' "$1"; }
die() { printf '\033[31m%s\033[0m\n' "$1" >&2; exit 1; }

if [ "$android" = 1 ]; then
	case "$abi" in
		arm64-v8a)   triple=aarch64-linux-android ;;
		armeabi-v7a) triple=armv7-linux-androideabi ;;
		x86_64)      triple=x86_64-linux-android ;;
		x86)         triple=i686-linux-android ;;
		*)           die "unknown ABI: $abi" ;;
	esac
	bin_dir="$repo_root/addons/godot_servo/bin/android/$abi"
	built_path="${CARGO_TARGET_DIR:-$repo_root/target}/$triple/$profile/libgodot_servo.so"
	dest_name=libgodot_servo.so
else
	case "$(uname -s)" in
		Darwin) platform=macos; lib_name=libgodot_servo.dylib; dest_name=libgodot_servo.dylib ;;
		Linux)  platform=linux; lib_name=libgodot_servo.so;    dest_name=libgodot_servo.x86_64.so ;;
		*)      die "unsupported host: $(uname -s) (use scripts/build.ps1 on Windows)" ;;
	esac
	bin_dir="$repo_root/addons/godot_servo/bin/$platform"
	built_path="${CARGO_TARGET_DIR:-$repo_root/target}/$profile/$lib_name"
fi

if [ "$do_build" = 1 ] || [ "$do_checks" = 1 ]; then
    if [ "$android" = 1 ]; then
        sdk=$(python3 scripts/gstreamer_sdk.py android)
        case "$abi" in
            arm64-v8a) arch=arm64 ;; armeabi-v7a) arch=armv7 ;; *) arch=$abi ;;
        esac
        gst_root="$sdk/$arch"
        gst_android="${CARGO_TARGET_DIR:-$repo_root/target}/gst-android/$abi"
        python3 scripts/android_gstreamer.py "$gst_root" "$gst_android" "$abi"
        export PKG_CONFIG_ALLOW_CROSS=1
        export PKG_CONFIG_LIBDIR="$gst_android/pkgconfig"
        export PKG_CONFIG_PATH=
        export CARGO_NDK_PLATFORM=24
    elif [ "$platform" = macos ] || [ "$platform" = linux ]; then
        gst_root=$(python3 scripts/gstreamer_sdk.py "$platform")
        if [ "$platform" = macos ]; then export PATH="$gst_root/bin:$PATH"; fi
        export PKG_CONFIG_PATH="$gst_root/lib/pkgconfig:${PKG_CONFIG_PATH:-}"
    fi
fi

# ------------------------------------------------------------------ Build ---
cargo_cmd=(cargo)
profile_args=()
if [ "$profile" = release ]; then
	profile_args+=(--release)
fi
if [ "$android" = 1 ] && { [ "$do_build" = 1 ] || [ "$do_checks" = 1 ]; }; then
	cargo_cmd+=(ndk -t "$abi")
	# NDK r23 dropped libgcc in favour of libunwind, but something in the
	# dependency graph still asks the linker for `-lgcc` and the link fails.
	# Drop in a stub that redirects to libunwind and put its directory on the
	# search path.
	gcc_stub="$repo_root/target/android-libgcc-stub"
	mkdir -p "$gcc_stub"
	echo 'INPUT(-lunwind)' > "$gcc_stub/libgcc.a"
	export RUSTFLAGS="${RUSTFLAGS:-} -L $gcc_stub"

	# bindgen needs libclang. The NDK ships one, which works even where the
	# distribution's clang is not installed, so point at it when it is there.
	if [ -z "${LIBCLANG_PATH:-}" ]; then
		for candidate in "${ANDROID_NDK_HOME:-}"/toolchains/llvm/prebuilt/*/lib \
		                 "${ANDROID_NDK_HOME:-}"/toolchains/llvm/prebuilt/*/musl/lib; do
			if [ -e "$candidate/libclang.so" ]; then
				export LIBCLANG_PATH="$candidate"
				say "LIBCLANG_PATH=$LIBCLANG_PATH"
				break
			fi
		done
	fi
fi

if [ "$do_checks" = 1 ]; then
	say 'cargo fmt --check'
	cargo fmt --check || die 'cargo fmt --check failed (run `cargo fmt`)'
	say 'cargo clippy'
	"${cargo_cmd[@]}" clippy --all-targets ${profile_args[@]+"${profile_args[@]}"} -- -D warnings
fi

if [ "$do_build" = 1 ]; then
	say "${cargo_cmd[*]} build ($profile)"
	"${cargo_cmd[@]}" build ${profile_args[@]+"${profile_args[@]}"}
fi

# ------------------------------------------------------------------ Place ---
[ -f "$built_path" ] || die "Build artifact not found: $built_path"

mkdir -p "$bin_dir"
cp -f "$built_path" "$bin_dir/$dest_name"
if [ "$android" = 1 ] && [ -n "${gst_android:-}" ]; then
    cp "$gst_android/libs/$abi/"*.so "$bin_dir/"
fi
if [ "$android" = 0 ] && [ -n "${gst_root:-}" ]; then
    python3 scripts/stage_gstreamer.py "$platform" "$gst_root" "$bin_dir"
fi
python3 scripts/stage_gstreamer.py manifest godot_servo.gdextension addons/godot_servo/bin
ok "Placed: $bin_dir/$dest_name"

# -------------------------------------------------------------------- Run ---
if [ "$do_run" = 1 ] || [ "$do_test" = 1 ]; then
	[ "$do_test" = 1 ] && scene=autotest
	[ -f "$repo_root/demo/$scene.tscn" ] || die "Scene not found: demo/$scene.tscn"

	args=(--path "$repo_root" "res://demo/$scene.tscn")
	[ "$quit_after" -gt 0 ] && args+=(--quit-after "$quit_after")

	user_args=()
	[ -n "$page" ] && user_args+=(--page "$page")
	[ -n "$screenshot" ] && user_args+=(--screenshot "$screenshot")
	[ ${#user_args[@]} -gt 0 ] && args+=(-- "${user_args[@]}")

	say "$godot ${args[*]}"
	"$godot" "${args[@]}"
fi
