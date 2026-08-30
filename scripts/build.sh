#!/usr/bin/env bash
# godot-servo のローカル開発パイプライン (Linux / macOS)。
# build.ps1 の対。
#
# 使い方:
#   ./scripts/build.sh                    # ビルドして配置 (debug)
#   ./scripts/build.sh --release
#   ./scripts/build.sh --run              # 配置済みのものでデモを起動
#   ./scripts/build.sh --run --scene flat
#   ./scripts/build.sh --test             # 入力とシグナルのセルフチェック
#   ./scripts/build.sh --run --page webgl
#   ./scripts/build.sh --checks           # fmt + clippy も回す
#
# ステージを何も指定しなければビルドのみ。

set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

profile=debug
do_build=1
do_run=0
do_test=0
do_checks=0
scene=main
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

case "$(uname -s)" in
	Darwin) platform=macos; lib_name=libgodot_servo.dylib; dest_name=libgodot_servo.dylib ;;
	Linux)  platform=linux; lib_name=libgodot_servo.so;    dest_name=libgodot_servo.x86_64.so ;;
	*)      die "unsupported host: $(uname -s) (use scripts/build.ps1 on Windows)" ;;
esac
bin_dir="$repo_root/addons/godot_servo/bin/$platform"

# ------------------------------------------------------------------ Build ---
if [ "$do_checks" = 1 ]; then
	say 'cargo fmt --check'
	cargo fmt --check || die 'cargo fmt --check failed (run `cargo fmt`)'
	say 'cargo clippy'
	# 警告はエラー扱い。CI がこのフラグで回るので、ここを緩めると門番が消える。
	if [ "$profile" = release ]; then
		cargo clippy --all-targets --release -- -D warnings || die 'cargo clippy failed'
	else
		cargo clippy --all-targets -- -D warnings || die 'cargo clippy failed'
	fi
fi

if [ "$do_build" = 1 ]; then
	say "cargo build ($profile)"
	if [ "$profile" = release ]; then
		cargo build --release
	else
		cargo build
	fi
fi

# ------------------------------------------------------------------ Place ---
built="$repo_root/target/$profile/$lib_name"
[ -f "$built" ] || die "Build artifact not found: $built"

mkdir -p "$bin_dir"
cp -f "$built" "$bin_dir/$dest_name"
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
