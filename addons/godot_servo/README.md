# godot_servo アドオン

Servo を GDExtension として組み込むためのアドオン。

`bin/` はビルド成果物なので、リポジトリには入っていない。クローン後に一度
ビルドすること。

```sh
scripts/build.ps1     # Windows
./scripts/build.sh    # Linux / macOS
```

リリースの zip には `bin/` 込みで入っている。その場合はこのフォルダごと、
`godot_servo.gdextension` と一緒にプロジェクトへ置けばよい。

```
your-project/
  godot_servo.gdextension
  addons/godot_servo/bin/...
```

`.gdextension` をアドオンの中ではなくプロジェクト直下に置いているのは、
Windows で ANGLE の DLL (`libEGL.dll` / `libGLESv2.dll`) を拡張本体と同じ
フォルダに置く必要があり、`[dependencies]` の相対解決を素直にするため。

エディタプラグインは無い。`ServoWebView` クラスは GDExtension が自分で登録するので、
プラグインの有効化は不要。
