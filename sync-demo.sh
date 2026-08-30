#!/bin/sh
# ビルド成果物をデモプロジェクトへ配る。
set -e
profile="${1:-debug}"
src="target/$profile"
dst="demo/addons/godot_servo/bin"
cp "$src/godot_servo.dll" "$src/libEGL.dll" "$src/libGLESv2.dll" "$dst/"
echo "copied $profile artifacts to $dst"
