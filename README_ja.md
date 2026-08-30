# godot-servo

[English](README.md) | 日本語

`godot-servo` は Rust 製のブラウザエンジン [Servo](https://servo.org/) を GDExtension として
Godot 4 に組み込み、描画結果を **GPU テクスチャのまま** Godot に渡すアドオンです。

ゲーム内のパネルに Web の UI を貼り、ポインタ・タッチ・キーボードの入力をそのまま転送し、
ページ側のボタン操作を Godot のシグナルで受け取れます。

![Godot の 3D パネルに表示した Servo の描画結果](shot_d3d12.png)

## できること

- **CPU を経由しない。** Servo がオフスクリーンの GPU サーフェスに描き、Godot がそれを直接サンプリングします。
- **本物の Web レンダリング。** HTML、CSS、JavaScript、WebGL 1 / 2、three.js (最新版を含む)。
- **オーバーレイではなく、ゲームの中。** 結果は `Texture2D` なので、3D パネルにもマテリアルにも `TextureRect` にも貼れます。
- **マウス・タッチ・キーボード入力**。日本語などの IME 変換にも対応します。
- **双方向のやり取り。** 入力をページへ転送し、ページのイベントをシグナルで受け取ります。
- **必ず起動するフォールバック。** GPU 共有が使えない環境では、起動を諦めずに CPU 読み戻しへ切り替えます。

## 対応プラットフォーム

各プラットフォームで、そのグラフィックススタックが用意している仕組みを使って GPU メモリを共有します。
共有経路が無い組み合わせでは `glReadPixels` による読み戻しに落とします。1 フレームあたりの往復は増えますが、
どこでも動きます。

| プラットフォーム | 経路 | 実機での状態 |
| --- | --- | --- |
| Windows / D3D12 | ANGLE の D3D11 共有テクスチャ (NT ハンドル) → `ID3D12Resource` | 確認済み |
| Android / Compatibility | `AHardwareBuffer` → `EGLImage` → `ExternalTexture` | 確認済み |
| macOS / Metal | IOSurface → `MTLTexture` | ビルドのみ、未実行 |
| Windows / Vulkan | CPU 読み戻し | 確認済み |
| Linux / Vulkan | CPU 読み戻し | 確認済み |
| Android / Forward+ · Mobile | CPU 読み戻し | 確認済み |

実際にどの経路を通ったかは `ServoWebView.get_backend_name()` で分かります。

GPU 共有が使えるかどうかは、レンダラの設定 2 つで決まります。

- **Windows** の既定は Vulkan です。共有テクスチャの経路を使うには
  `rendering/rendering_device/driver.windows` を `d3d12` にしてください。Vulkan でも実現はできますが、
  プロジェクトから `VK_KHR_external_memory_win32` を有効にするための
  [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940) が要ります。
- **Android** は Compatibility レンダラ (`rendering/renderer/rendering_method.mobile`) が必要です。
  RenderingDevice 側のバックエンドでは `texture_external_initialize()` が空実装なので、
  Forward+ と Mobile には外部テクスチャの受け口がありません。

## 必要なもの

- **Godot 4.4 以降。** `RenderingDevice.texture_create_from_extension()` と
  `get_driver_resource()` が GDExtension に出たのが 4.4 です。
- ソースからビルドするなら **Rust 1.94 以降**。
- Android 向けには **cargo-ndk** と NDK、そして Linux か macOS のホスト。

## リポジトリの構成

リポジトリのルートがそのまま Godot プロジェクトです。クローンして Godot で開けば動きます。

```
godot_servo.gdextension          拡張のマニフェスト。プロジェクト直下に置く
addons/godot_servo/bin/          ビルド成果物の置き場 (コミットしない)
  windows/godot_servo.x86_64.dll
  windows/libEGL.dll             ANGLE。実行時に名前で読まれる
  windows/libGLESv2.dll
  android/arm64-v8a/libgodot_servo.so
demo/                            デモのシーンとページ
project.godot
scripts/build.ps1 | build.sh     ビルドして bin/ へ配置する
src/                             拡張本体 (Rust)
```

配布するのは `godot_servo.gdextension` と `addons/godot_servo/` の 2 つです。
この 2 つを自分のプロジェクトに入れれば使えます。

## ビルド

```sh
scripts/build.ps1                 # Windows: ビルドして配置 (debug)
scripts/build.ps1 -Release
./scripts/build.sh                # Linux / macOS
./scripts/build.sh --release
./scripts/build.sh --android      # Android arm64-v8a。cargo-ndk が要る
```

`cargo build` だけでは配置まで行いません。ビルドスクリプトを使ってください。
ライブラリを `addons/godot_servo/bin/` へ複製するほか、mozangle が生成した `libEGL.dll` と
`libGLESv2.dll` を、ビルド完了後にそのクレートの `OUT_DIR` から拾って配置します。
surfman は ANGLE を実行時にファイル名で読むので、この 2 つは拡張の DLL と同じ場所に要ります
(`src/angle_loader.rs` が絶対パスで先読みします)。

### Android

Linux か macOS (WSL を含む) からクロスコンパイルします。Windows ホストではビルドできません。
Servo の C 依存 2 つを同時に満たすホストツールチェーンが無いためです。jemalloc の `configure` は
MSVC のホストトリプルを受け付けず、glsl-optimizer は mingw でコンパイルが通りません。

`ANDROID_NDK_HOME` を NDK に向けてから実行します。

```sh
export ANDROID_NDK_HOME=~/android/android-ndk-r27c
./scripts/build.sh --release --android
```

スクリプトは `INPUT(-lunwind)` だけを書いた `libgcc.a` のスタブを `target/` に置き、
リンクの探索パスに足します。NDK r23 で libgcc が libunwind に置き換わったのに、
依存のどれかがまだリンカに `-lgcc` を要求するためです。

## デモを動かす

```sh
export GODOT=~/.local/godot/4.7.2-stable/Godot_v4.7.2-stable_win64_console.exe

scripts/build.ps1 -Run                 # 3D の in-game ブラウザ
scripts/build.ps1 -Run -Scene flat     # 2D。切り分け用
scripts/build.ps1 -Test                # 入力とシグナルのセルフチェック
```

セルフチェックは拡張を端から端まで動かして、何を確かめたかを出力します。

```
--- godot-servo self check ---
  path: d3d12-shared-nt-handle
  OK   bridge_event (godot.emit)  (expected 'ready')
  OK   evaluate_javascript / script_result  (button at (95.2, 189.4))
  OK   click -> onclick -> bridge_event  (expected 'buy')
  OK   touch tap -> onclick -> bridge_event  (expected 'buy')
  OK   touch drag -> scroll  (scrollTop 0 -> 429)
  OK   focus input -> ime_requested  (caret [P: (28.0, 509.0), S: (220.0, 36.0)])
  OK   ime composition -> input value  (value '日本語')
  OK   os ime sequence -> committed once  (value '日本')
  OK   wheel -> scroll  (scrollTop 0 -> 608)
--- 0 failed ---
```

### APK を作る

```sh
./scripts/build.sh --release --android
llvm-strip --strip-all addons/godot_servo/bin/android/arm64-v8a/libgodot_servo.so

godot --headless --path . --export-debug Android godot-servo.apk
adb install -r godot-servo.apk
```

strip は必須と考えてください。arm64-v8a のライブラリは debug で 1474 MB、release で 170 MB、
`--strip-all` 後で 119 MB、APK にして 146 MB です。大半は SpiderMonkey、Stylo、WebRender、
そして ICU のデータです。

## 使いかた

```gdscript
var browser := ServoWebView.new()
browser.view_size = Vector2i(1280, 720)
browser.url = "https://example.com"
add_child(browser)

browser.frame_updated.connect(func() -> void:
    material.albedo_texture = browser.get_texture()
    if browser.is_texture_flipped_v():
        material.uv1_scale = Vector3(1.0, -1.0, 1.0)
        material.uv1_offset = Vector3(0.0, 1.0, 0.0)
)

# 入力を転送する。position は WebView 内のピクセル座標。
browser.feed_input(event, local_position)

# ページからのイベントを受け取る。
browser.bridge_event.connect(func(name: String, payload: String) -> void:
    print(name, " ", payload)
)
```

テクスチャの扱いで 2 つだけプラットフォーム依存の分岐があります。デモのシーンはどちらも対応しています。

- `is_texture_flipped_v()` は macOS の IOSurface 経路で true になります。この経路には
  GL の左下原点を直す転送段が無いので、マテリアル側で上下を戻してください。
- `needs_external_sampler()` は Android で true になります。共有バッファが
  `GL_TEXTURE_EXTERNAL_OES` のテクスチャとして届くためです。`sampler2D` では黒くしか読めないので、
  `samplerExternalOES` を宣言したシェーダをマテリアルに使います。
  最小限のものが `demo/servo_external.gdshader` にあります。

### 入力を転送する

`feed_input(event, position)` はマウス・タッチ・キーボードのイベントを受け取ります。
`position` は WebView 内のピクセル座標なので、先に変換してください。

- `TextureRect` なら、コントロールの位置を引いて `view_size / rect.size` を掛けます。
- 3D パネルなら、`CollisionObject3D.input_event` の当たり位置を UV に直して `view_size` を掛けます。
  `demo/main.gd` に往復の変換があります。

マウスとタッチは両方まとめて流して構いません。Godot は
`input_devices/pointing/emulate_mouse_from_touch` (既定で有効) によってタッチから疑似マウスイベントも
作りますが、`feed_input()` は `device` が `DEVICE_ID_EMULATION` のイベントを落とすので、
1 回の操作が二重に届くことはありません。`emulate_touch_from_mouse` の逆向きも同じ規則で除かれます。

タッチは Servo に本物のタッチイベントとして渡ります。ページには `touchstart` / `touchmove` /
`touchend` が届き、スクロールと慣性は Servo 側のタッチハンドラが処理します。

### 日本語などを IME で入力する

ページ内の編集可能な要素にフォーカスが入ると Servo が拡張に通知し、拡張は OS の IME を有効にして
`ime_requested(caret, multiline)` を出します。`caret` は WebView 内のピクセル座標の矩形です。

変換候補のウィンドウは OS がウィンドウ座標で出すので、3D パネルが画面のどこに映っているかを
拡張は知りません。キャレットを自分で射影して `ime_anchor` に入れてください。

```gdscript
browser.ime_requested.connect(func(caret: Rect2, _multiline: bool) -> void:
    var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
    browser.ime_anchor = camera.unproject_position(view_pixels_to_world(bottom_left))
)
```

デモのシーンはどちらもこれをやっています。設定しないと候補がウィンドウの左上に出ます。

OS の IME ではなくゲーム側の入力 UI から変換を駆動したい場合は、
`feed_ime_composition(state, text)` を `"start"` / `"update"` / `"end"` で呼びます。
`"end"` に渡した文字列が確定分になります。`feed_ime_preedit(text)` は OS の IME と同じ経路で、
未確定文字列 → 空文字列 の順に渡し、確定した文字は `feed_input()` でキーイベントとして送ります。

**既知の制限。** 変換を取り消すと未確定文字列が欄に残ります。Servo の `compositionend` は
データが空のときに選択を外すだけで、未確定文字列を消す手段が composition の API に無いためです。

### ページから Godot へイベントを送る

拡張はすべてのページに `window.godot` を注入します。

```js
godot.emit("buy", { item: "potion", price: 120 });   // payload は JSON 文字列で届く
```

JavaScript を書かず、ただのリンクでも送れます。

```html
<a href="godot:buy?item=potion">購入</a>              <!-- payload はクエリ文字列 -->
```

前者は目印を付けた `console.log` を `show_console_message` で拾っています。
遷移を起こさないので、ページの状態に触りません。後者は `godot:` スキームへの遷移を
`request_navigation` で捕まえて拒否しています。

### API

| | |
| --- | --- |
| `url`, `view_size`, `autostart`, `enable_webgl2`, `ime_anchor` | エクスポートされたプロパティ |
| `start()`, `stop()`, `is_running()` | 生存管理 |
| `get_texture()`, `is_texture_flipped_v()`, `needs_external_sampler()`, `get_backend_name()` | 表示 |
| `load_url()`, `reload()`, `go_back()`, `go_forward()` | ナビゲーション |
| `evaluate_javascript(code) -> int` | 結果は `script_result(id, value)` で届く |
| `feed_input(event, position)`, `notify_pointer_left()` | 入力 |
| `feed_ime_composition(state, text)`, `feed_ime_preedit(text)`, `cancel_ime_composition()` | IME |
| `set_view_size_px(size)` | 解像度 |

シグナル: `frame_updated`, `title_changed`, `url_changed`, `load_started`, `load_finished`,
`cursor_changed`, `console_message`, `bridge_event`, `script_result`, `ime_requested`,
`ime_dismissed`

## WebGL

WebGL の描画結果も同じ共有テクスチャに乗ります。確認用のページが `demo/web/` にあります。

| | 結果 |
| --- | --- |
| WebGL 1.0 | 動く |
| WebGL 2.0 | `enable_webgl2` を付ければ動く |
| three.js r128 | 動く |
| three.js 0.180 (最新) | WebGL 2.0 経由で動く |

![WebGL 2.0 で描画した three.js 0.180](shot_three.png)

Servo は WebGL 2 を既定で無効にしているので、`ServoWebView` は `dom_webgl2_enabled` を設定する
`enable_webgl2` を持っています。最新の three.js が WebGL 2 を要求するため、既定は有効です。

```sh
scripts/build.ps1 -Run -Page webgl          # 素の WebGL
scripts/build.ps1 -Run -Page three-legacy   # three.js r128

# ES モジュールを使うページには実際のオリジンが要る。
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/three.html
```

`-Page` は `res://demo/web/<名前>.html` を開きます。`http` で始まる文字列はそのまま URL として扱います。

## 設計メモ

### シングルバッファ

スワップチェーンを使わず、オフスクリーンの surfman サーフェスを 1 枚だけ確保して持ち続けます。
こうすると Godot に渡したテクスチャの RID がビューの寿命の間ずっと有効なままになります。
引き換えに Servo が描いている最中に Godot がサンプリングし得ますが、`paint()`、ブリット、
`glFlush()`、Godot 自身の描画をこの順でメインスレッドに並べている限り、目に見える問題は出ていません。

### 同期

Godot の `RenderingDevice` には、外部セマフォをサブミットに結びつける手段がありません
(`submit()` と `sync()` はローカルデバイス専用です)。順序は `glFlush()` に頼っています。
wgpu 側の同種のライブラリ `wgpu-native-texture-interop` も同じ状況で、明示的なセマフォは
「まだどの組み込みシンクロナイザも扱っていない」と書かれています。

### GL コンテキストの貸し借り

Servo は自前の GL コンテキストを持ち、描画のたびにそれをカレントにします。Godot が Vulkan /
D3D12 / Metal で描いている環境では、そのスレッドの GL コンテキストを他に使う者がいません。
Android の Compatibility レンダラだけは事情が違い、Godot 自身が同じスレッドの EGL コンテキストで
描画しています。そこで `src/gl_guard.rs` が、Servo にスレッドを渡す前にカレントなものを控え、
あとで戻します。

同じ規則が Godot 側で GL を呼ぶものすべてに当てはまります。`ExternalTexture` はバッファ ID を
設定した時点で `glEGLImageTargetTexture2DOES` を呼ぶので、Android のブリッジは
それを作る前にホストのコンテキストへ戻します。

### D3D12 経路が 1 回コピーする理由

Godot の D3D12 ドライバは `_texture_create_shared_from_slice()` で、自前のアロケーションを持つ
テクスチャしか受け付けず、インポートしたものを弾きます。`Texture2DRD` は内部で
`texture_create_shared()` を呼ぶため、インポートしたテクスチャをそのまま渡すと真っ白になります。
Vulkan ドライバは `|| created_from_extension` の除外条件でこの場合を通しますが、D3D12 には
その除外がありません。

そのため D3D12 経路では、インポートしたテクスチャを `RenderingDevice.texture_copy()` で
Godot 所有のテクスチャに複製します。コピーは GPU 内で完結するので、CPU の往復は発生しません。
Metal ドライバにはこの制限が無いので、そちらはテクスチャをそのまま渡します。

### CPU フォールバックが毎フレーム確保しない理由

読み戻し経路はピクセルの置き場と `Image` を 2 組持ち、フレームごとに交互に使います。
`PackedByteArray` は copy-on-write なので、いま `Image` が参照している側に書くとそこで複製が
走ります。もう一方に書けば参照は 1 つのままで、`glReadPixels` の出力先がそのまま
テクスチャの中身になります。2 組で足りるのは、レンダリングサーバが別スレッドでも
積んだ `texture_2d_update` を 1 フレーム以内に消化するためです。

1280×720 の release ビルドで、1 フレームあたりの更新が 1.93 ms から 1.34 ms になり、
毎フレームの確保 約 7 MB と全画面 1 回分の複製が消えます。

### Linux で jemalloc をビルドし直す理由

Servo は `servo-allocator` を通じて jemalloc を取り込みますが、jemalloc は既定で initial-exec TLS を
使います。これは共有オブジェクトに `STATIC_TLS` を立て、`PT_TLS` を glibc の静的 TLS 余剰枠の外へ
押し出すので、Godot が `dlopen` できなくなります。そこで `Cargo.toml` では Linux に限って
`tikv-jemalloc-sys` を直接宣言し、まさにこの事態のために用意されている
`disable_initial_exec_tls` フィーチャを有効にしています。

## 対応していないもの

- **WebGPU。** Servo の実装がこの組み込み方だとクラッシュします。デバイスの作成もコンピュートシェーダも
  動きますが、canvas へのプレゼント時には必ず、canvas を使わなくても終了時に SIGSEGV で落ちます。
  `webgpu` フィーチャは切ってあります。有効にすると wgpu と naga も抱き込み、ただでさえ大きい
  バイナリがさらに膨らみます。将来の再確認用に `demo/web/webgpu.html` と `webgpu-compute.html` を
  置いてあります。
- **シーンの色を読むこと。** CSS の `backdrop-filter` でページの後ろのゲーム画面をぼかす用途です。
  Godot 側は `CompositorEffect` で足りますが、Servo 側は `WebRenderImageHandlerType` を足した
  フォークが要ります。
- **iOS。** surfman も Servo も対象にしておらず、iOS は JIT と `dlopen` を禁じています。
- **Linux の GPU 共有経路。** `VK_EXT_external_memory_dma_buf` のために
  [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940) が要ります。
- **`ServoWebView` を複数置くこと。** 設計上は `Servo` 本体を 1 つ共有しますが、未検証です。

## 似たプロジェクト

Servo を Godot に組み込むアドオンが他に 2 つあります。どちらも `SoftwareRenderingContext` と
`read_to_image()` で描画しています。これは本プロジェクトが GPU 共有を使えないときに落とす経路と
同じものです。

| | 描画 | ライセンス |
| --- | --- | --- |
| [Decapitated/Godot-Servo](https://github.com/Decapitated/Godot-Servo) | CPU 読み戻し | LGPL-3.0 |
| [emanuelbertey/web-servo-godot](https://github.com/emanuelbertey/web-servo-godot) | CPU 読み戻し | 記載なし |
| 本プロジェクト | GPU 共有テクスチャ + CPU フォールバック | MIT / Apache-2.0 |

GPU 経路が要らないなら、現時点では `web-servo-godot` のほうが守備範囲が広めです
(シグナルが多く、JavaScript API があり、Linux と Android のビルドが通っています)。
ただしライセンスの記載が無いため、既定では著作権者に全権が留保されている扱いになります。

## ライセンス

[Apache License 2.0](LICENSE-APACHE) と [MIT license](LICENSE-MIT) のデュアルライセンスです。
どちらを選んでも構いません。

Servo 自体は MPL-2.0 です。本クレートは Servo を改変せずに依存しているだけなので、
ファイル単位のコピーレフトが利用者のコードに及ぶことはありません。`demo/web/vendor/` に同梱した
three.js のビルドは MIT で、元のライセンスヘッダを残しています。
