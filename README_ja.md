<p align="center">
  <img src="addons/godot_servo/icon.svg" alt="godot-servo ロゴ" width="128" height="128">
</p>

<h1 align="center">godot-servo</h1>

<p align="center">
  <a href="README.md">English</a> | 日本語
</p>

`godot-servo` は、Rust 製のブラウザエンジン [Servo](https://servo.org/) を GDExtension として Godot 4 に組み込み、描画結果を **GPU テクスチャのまま** Godot へ渡すアドオンです。

ゲーム内のパネルに Web UI を配置し、ポインタ・タッチ・キーボードの入力をそのまま転送できます。また、Web ページ側のボタン操作などを Godot のシグナルとして双方向に受け取ることも可能です。

![Godot の 3D パネルに表示した Servo の描画結果](shot_d3d12.png)

## 主な特徴

- **CPU を経由しない**: Servo がオフスクリーンの GPU サーフェスに直接描き、Godot がそれをそのままサンプリングします。
- **本格的な Web レンダリング**: HTML、CSS、JavaScript、WebGL 1 / 2、three.js（最新版を含む）に対応。
- **オーバーレイではなくゲーム内に描画**: 描画結果は `Texture2D` なので、3D パネル、マテリアル、`TextureRect` などにそのまま貼れます。
- **マウス・タッチ・キーボード入力**: 日本語などの IME によるテキスト変換入力にも対応。
- **双方向のやり取り**: 入力をページへ転送し、ページ側のイベントをシグナルで受け取れます。
- **必ず起動するフォールバック**: GPU メモリ共有が使えない環境でも、起動エラーにならず CPU リードバックへ自動で切り替わります。

## 対応プラットフォーム

各プラットフォームで、グラフィックススタックが提供するネイティブな仕組みを使って GPU メモリを共有します。
共有経路がない組み合わせでは `glReadPixels` による CPU リードバックへフォールバックします。1 フレームあたりの往復オーバーヘッド（CPU/GPU 転送）は生じますが、どの環境でも確実に動作します。

| プラットフォーム | 共有経路 | 動作状況 |
| --- | --- | --- |
| Windows / D3D12 | ANGLE の D3D11 共有テクスチャ (NT ハンドル) → `ID3D12Resource` | 動作確認済み |
| Windows / Vulkan | ANGLE の D3D11 共有テクスチャ (NT ハンドル) → `VkImage` | 動作確認済み |
| Android / Compatibility | `AHardwareBuffer` → `EGLImage` → `ExternalTexture` | 動作確認済み |
| macOS / Metal | IOSurface → `MTLTexture` | 動作確認済み |
| Linux / Vulkan | `VkImage` → opaque fd → `GL_EXT_memory_object` | 動作確認済み (llvmpipe) |
| Android / Forward+ · Mobile | `VkImage` → opaque fd → `GL_EXT_memory_object` | 動作確認済み |
| macOS / Vulkan (MoltenVK) | IOSurface → `VkImage` | 動作確認済み |

実際にどの経路が選択されたかは、`ServoWebView.get_backend_name()` で確認できます。

### Windows と macOS でプロジェクト設定が必要な理由

Vulkan のデバイス拡張は、GDExtension が読み込まれるよりずっと前、デバイスの生成時に確定します。
Godot が要求していない拡張がテクスチャ共有に必要な場合、プロジェクト設定から要求するしかありません。それには [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940) が必要です。この PR は、プロジェクト設定 `rendering/rendering_device/vulkan/additional_device_extensions` と、実際に有効になった拡張を取得する `RenderingDevice.get_device_enabled_extensions()` を追加します。

```ini
[rendering]

rendering_device/vulkan/additional_device_extensions=PackedStringArray("VK_KHR_external_memory_win32", "VK_EXT_metal_objects")
```

| プラットフォーム | デバイス拡張 | 標準の Godot での状態 |
| --- | --- | --- |
| Windows | `VK_KHR_external_memory_win32` | 無効（設定が必要） |
| macOS (MoltenVK) | `VK_EXT_metal_objects` | 無効（設定が必要） |
| Linux / Android | `VK_KHR_external_memory_fd` | **既定で有効** |

この表で Linux と Android が追加設定なし（既定で有効）となっているのは、共有するハンドルの種類によるものです。
opaque fd（不透明ファイル記述子）は、標準の Godot が最初から有効にしている唯一の外部メモリハンドルです（Godot が `VK_KHR_external_memory_fd` を登録しているのはメモリ共有のためではなく、一部のプラットフォームで検証レイヤーに大量の警告が出るのを抑えるためです）。この 2 つの経路を opaque fd ベースで構築したことで、パッチも追加設定もない素の Godot でも GPU メモリを共有できます。もし dma-buf や `AHardwareBuffer` を使おうとすると、Godot が登録していない拡張が別途必要になります。

追加設定が必要な経路のみ、起動時にメソッドの有無を調べます。メソッドがなければ、Vulkan レンダラは起動に失敗することなく、理由をログに出力して CPU リードバックへ切り替えます。メソッドがあれば、有効になっている拡張を確認して動作を決めます。
さらにどの経路でも、デバイス自身に関数ポインタの解決を確認します。拡張一覧に名前が載っていてもエントリポイントが解決できないことがあり、一覧上の名前は単なる宣言にすぎず、実際に解決できた関数だけが確実だからです。

### レンダラ別の設定と挙動

- **Windows**: Vulkan と D3D12 のどちらのレンダラでも共有テクスチャを利用できます。Godot の既定は Vulkan で、その場合は前述のプロジェクト設定が必要です。一方、`rendering/rendering_device/driver.windows` を `d3d12` に設定すれば、Godot 4.4 以降であること以外は何も要求されません。デモプロジェクトは、パッチなしの標準 Godot で動くように `d3d12` を設定しています。
- **Android**: 3 つのレンダラすべてで動作しますが、経路は 2 種類あります。
  - **Compatibility (GLES3)**: `AHardwareBuffer` を共有し、`ExternalTexture` として受け取ります。シェーダに `samplerExternalOES` が必要です（後述の `needs_external_sampler()` を参照）。
  - **Forward+ / Mobile**: `VkImage` を fd 経由で共有し、通常の `sampler2D` テクスチャとして届きます。
  ※ `ExternalTexture` の経路が Compatibility 限定なのは、Godot の `RenderingDevice` 側で `texture_external_initialize()` が空実装（スタブ）になっているためです。
- **macOS**: 既定は Metal で、プロジェクト設定は不要です。Vulkan 経路は MoltenVK で動かす場合のためのものです。

## 必要な環境

- **Godot 4.4 以降**: `RenderingDevice.texture_create_from_extension()` と `get_driver_resource()` が GDExtension で使えるようになったのが 4.4 からです。
- **Rust 1.94 以降**（ソースからビルドする場合）
- **Android 向けビルド**: Linux または macOS ホスト、**cargo-ndk**、および Android NDK。

## リポジトリの構成

リポジトリのルート自体が Godot プロジェクトになっています。クローンして Godot で開くだけでそのまま動作します。

```
godot_servo.gdextension          GDExtension マニフェスト（プロジェクト直下に配置）
addons/godot_servo/
  servo_texture_rect.gd          TextureRect に Web ページを表示して操作を通すスクリプト
  servo_panel_3d.gd              上記と同様の処理を 3D の QuadMesh パネルで行うスクリプト
  local_pages.gd                 res:// 内のページを開くための file:// URL 変換
  select_picker.gd               Web ページの <select> 要素に応答する PopupMenu
  cursors.gd                     CSS カーソル名を Godot のカーソル形状へマッピング
  servo_external.gdshader        Android GLES3 (Compatibility) 経路用の samplerExternalOES シェーダ
  servo_external_canvas.gdshader 上記シェーダの Control (2D Canvas) 版
  bin/                           ビルド成果物の配置先（Git 管理外）
    windows/godot_servo.x86_64.dll
    windows/libEGL.dll           ANGLE（実行時に動的ロードされる）
    windows/libGLESv2.dll
    android/arm64-v8a/libgodot_servo.so
demo/                            デモ用のシーンと Web ページ
project.godot
scripts/build.ps1 | build.sh     ビルドおよび bin/ への配置スクリプト
src/                             拡張本体のソースコード (Rust)
```

リリースアーカイブには、バイナリ入りの `bin/` と `godot_servo.gdextension` を含む `addons/godot_servo/` 一式が入っています。このフォルダをご自身のプロジェクトの `addons/` へそのままコピーすれば導入できます。
なお、本リポジトリでマニフェストがルート直下にあるのは、リポジトリルートがそのままデモプロジェクトを兼ねているためです。マニフェスト内の `res://` パスは絶対パスなので、ルート直下でも `addons/` 内でも同様に解決されます。ただし、二重登録エラーを防ぐため、マニフェストはいずれか **1 か所のみ** に配置してください。

## ビルド手順

```sh
scripts/build.ps1                 # Windows: デバッグビルドと配置
scripts/build.ps1 -Release        # Windows: リリースビルド
./scripts/build.sh                # Linux / macOS: デバッグビルド
./scripts/build.sh --release      # Linux / macOS: リリースビルド
./scripts/build.sh --android      # Android (arm64-v8a): cargo-ndk が必要
```

単に `cargo build` を実行しただけでは、ファイルの配置（ステージング）までは行われません。必ずビルドスクリプトを使ってください。
スクリプトはライブラリを `addons/godot_servo/bin/` へコピーするだけでなく、mozangle が生成した `libEGL.dll` と `libGLESv2.dll` を、ビルド完了後にクレートの `OUT_DIR` から取り出して配置します。surfman は実行時にファイル名で ANGLE を読み込むため、この 2 つの DLL は拡張 DLL と同じ場所に置く必要があります（`src/angle_loader.rs` が絶対パスで事前ロードします）。

### Android 向けビルド

Linux または macOS（WSL を含む）からクロスコンパイルします。Windows ホスト環境ではビルドできません。Servo が依存する 2 つの C ライブラリを同時に満たせるホストツールチェーンが存在しないためです（jemalloc の `configure` が MSVC ホストトリプルを受け付けず、glsl-optimizer は MinGW でコンパイルが通りません）。

環境変数 `ANDROID_NDK_HOME` を NDK に設定してから実行します。

```sh
export ANDROID_NDK_HOME=~/android/android-ndk-r27c
./scripts/build.sh --release --android
```

※ なおスクリプトは、`INPUT(-lunwind)` だけを書いたスタブの `libgcc.a` を `target/` に置き、リンクの検索パスに追加します。NDK r23 で libgcc が libunwind に置き換わったにもかかわらず、依存クレートのいずれかがリンカに `-lgcc` を要求するためです。

## デモの実行

```sh
# 環境変数 GODOT に Godot の実行ファイルパスを設定
export GODOT=~/.local/godot/4.7.2-stable/Godot_v4.7.2-stable_win64_console.exe

scripts/build.ps1 -Run                 # 3D ゲーム内ブラウザのデモ
scripts/build.ps1 -Run -Scene flat     # 2D プレーン表示（問題の切り分け用）
scripts/build.ps1 -Test                # 入力とシグナルのセルフチェック
```

セルフチェック（`-Test`）を実行すると、拡張を一通り動かして各機能の検証結果を出力します。

```
--- godot-servo self check ---
  path: d3d12-shared-nt-handle
  OK   [  0.2s] bridge_event (godot.emit)  (expected 'ready')
  OK   [  0.5s] evaluate_javascript / script_result  (button at (96.2, 189.4))
  OK   [  0.6s] click -> onclick -> bridge_event  (expected 'buy')
  OK   [  0.7s] touch tap -> onclick -> bridge_event  (expected 'buy')
  OK   [  2.5s] touch drag -> scroll  (scrollTop 0 -> 476)
  OK   [  2.9s] focus input -> ime_requested  (caret [P: (28.0, 509.0), S: (220.0, 36.0)])
  OK   [  3.4s] ime composition -> input value  (value '日本語')
  OK   [  4.0s] os ime sequence -> committed once  (value '日本')
  OK   [  4.1s] alert -> dialog_alert  (message 'hello from the page')
  OK   [  4.4s] respond_to_dialog releases the page  (no pending)
  OK   [  4.9s] confirm -> respond_to_dialog(true)  (value true)
  OK   [  5.0s] prompt -> dialog_prompt  (default 'hero')
  OK   [  5.5s] prompt -> respond_to_dialog(text)  (value 'godot')
  OK   [  5.6s] select -> select_element_requested  (4 options, last group 'Advanced')
  OK   [  6.1s] respond_to_select sets the value  (value 'sage')
  OK   [  7.3s] wheel -> scroll  (scrollTop 0 -> 608)
--- 0 failed ---
```

### Android APK のビルドとインストール

```sh
./scripts/build.sh --release --android

godot --headless --path . --export-debug Android godot-servo.apk
adb install -r godot-servo.apk
```

リリースビルドは `[profile.release]` の `strip = true` によって不要なシンボルが削除され、arm64-v8a ライブラリが 119 MB、APK 全体で 146 MB になります。デバッグビルドは 1474 MB もあり APK に収まらないため、Android はリリースビルド（`--release`）でのみビルドします（サイズの大半は SpiderMonkey、Stylo、WebRender、および ICU データです）。

## 使い方

もっとも手軽なのは、アドオンに同梱されているコンポーネントを使う方法です。
- **2D UI**: `TextureRect` に `servo_texture_rect.gd` をアタッチ
- **3D パネル**: `QuadMesh` と子ノードの `CollisionObject3D` を持つ `MeshInstance3D` に `servo_panel_3d.gd` をアタッチ

スクリプトをアタッチしたら、インスペクターの `browser` プロパティに対象の `ServoWebView` ノードを指定します。テクスチャの管理、座標変換、入力の転送、カーソル形状の変更、IME アンカーの追従などはすべてこれらのコンポーネントが自動で処理します。
プロジェクト側で書く必要があるのは実質的に 2 つだけです。
1. 開く URL の指定
2. ページから届くイベントの処理

実際の実装例は `demo/main.tscn`（3D）と `demo/flat.tscn`（2D）を参照してください。

以降は、自分で直接制御コードを書く方向けに、コンポーネントが内部で行っている基本処理の解説です。

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

# 入力を転送（local_position は WebView 内のピクセル座標）
browser.feed_input(event, local_position)

# ページからのイベントを受信
browser.bridge_event.connect(func(name: String, payload: String) -> void:
    print(name, " ", payload)
)
```

テクスチャの扱いには、プラットフォーム依存の分岐が 2 つあります（デモシーンはどちらも対応済みです）。

- `is_texture_flipped_v()`: macOS の IOSurface 経路で `true` になります。この経路には OpenGL 特有の左下原点を直す転送ステップがないため、マテリアル側で上下を反転してください。
- `needs_external_sampler()`: Android の Compatibility レンダラで `true` になります。共有バッファが `GL_TEXTURE_EXTERNAL_OES` テクスチャとして届くためです。通常の `sampler2D` では黒くしかサンプリングできないため、`samplerExternalOES` を宣言したシェーダをマテリアルに使います。最小限の実装例を `addons/godot_servo/servo_external.gdshader` として同梱しています。

### 入力イベントの転送

`feed_input(event, position)` は、マウス・タッチ・キーボードのイベントを受け取ります。
`position` は WebView 内のピクセル座標なので、事前に変換してください。

- **`TextureRect` の場合**: コントロールの位置を引き、表示倍率（`view_size / rect.size`）を掛けます。
- **3D パネルの場合**: `CollisionObject3D.input_event` の衝突位置を UV 座標に変換し、`view_size` を掛けます（`demo/main.gd` に双方向の変換処理があります）。

マウスとタッチのイベントは、両方まとめて渡して構いません。Godot では `input_devices/pointing/emulate_mouse_from_touch` が既定で有効になっており、タッチ操作から擬似マウスイベントも生成されますが、`feed_input()` は `device` が `DEVICE_ID_EMULATION` の合成イベントを除外するため、1 回の操作が二重に届くことはありません。逆方向の `emulate_touch_from_mouse` も同様に除外されます。

タッチ操作は Servo に本物のタッチイベントとして渡ります。Web ページ側には `touchstart` / `touchmove` / `touchend` が届き、スクロールや慣性の処理も Servo 側のタッチハンドラが行います。

### 日本語などの IME 入力

ページ内の入力欄にフォーカスが当たると、Servo が拡張に通知し、OS の IME を有効にして `ime_requested(caret, multiline)` シグナルを発火します。`caret` は WebView 内のピクセル座標の矩形（`Rect2`）です。

IME の変換候補ウィンドウは OS がウィンドウ座標系で表示するため、拡張側では 3D パネルが画面のどこに映っているか分かりません。キャレット位置を画面座標に射影変換して、`ime_anchor` に代入してください。

```gdscript
browser.ime_requested.connect(func(caret: Rect2, _multiline: bool) -> void:
    var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
    browser.ime_anchor = camera.unproject_position(view_pixels_to_world(bottom_left))
)
```

デモシーンはいずれもこの設定を行っています。設定しない場合、変換候補がウィンドウの左上（原点）に出てしまいます。

OS の IME ではなく、ゲーム独自の入力 UI（仮想キーボードなど）から変換を制御したい場合は、`feed_ime_composition(state, text)` を `"start"` / `"update"` / `"end"` で呼び出します。`"end"` に渡した文字列が確定文字列になります。
また `feed_ime_preedit(text)` は OS の IME と同じ経路で動作し、未確定文字列 → 空文字列 の順に渡し、確定した文字を `feed_input()` 経由でキーイベントとして送信します。

> [!NOTE]
> **既知の制限事項**: 変換をキャンセルした際、未確定文字列が入力欄に残ってしまう制限があります。これは Servo の `compositionend` ハンドラが、データが空のときに選択範囲を解除するだけで、未確定文字列を消去する手段が現在の Composition API にないためです。

### ページのダイアログや選択メニューへの応答

`alert()` / `confirm()` / `prompt()` や `<select>` は、組み込み側（Godot 側）が応答するまでページの JavaScript 実行を一時停止（ブロック）します。
拡張自身はダイアログ UI を持たないため、これらをシグナルとして Godot に渡し、ゲーム側で好みの UI を表示して応答を返す設計になっています。
`<select>` 要素も同様で、Servo は自前でドロップダウンメニューを描画せず、選択肢のリストをシグナルで渡してきます。そのため、ゲーム側でメニューを表示しない限り、ユーザーが `<select>` をクリックしても何も起きていないように見えます。この表示を行う小さな `PopupMenu` 実装として、`addons/godot_servo/select_picker.gd` を同梱しています。

```gdscript
browser.dialog_confirm.connect(func(message: String) -> void:
    var accepted: bool = await my_dialog.ask(message)
    browser.respond_to_dialog(accepted, "")
)

browser.select_element_requested.connect(func(options: Array, multiple: bool) -> void:
    # options は [{ id, label, disabled, group }, ...]（<optgroup> はフラット化済み）
    var chosen: int = await my_menu.pick(options)
    browser.respond_to_select([chosen])
)
```

シグナルを受け取ったら、**必ず応答を返してください**。応答のないダイアログを待つページは、JavaScript が止まったままになります。ユーザーが何も選ばずに UI を閉じた場合は、`cancel_pending_dialog()` を呼び出してください（応答待ちのダイアログがあるかどうかは `has_pending_dialog()` で確認できます）。
また、表示される文字列は Web ページ側が指定するものなので、ゲーム自身の正規のシステム UI と誤認されないようなデザインで表示することをおすすめします。

なお、ファイル選択・色選択・コンテキストメニューはシグナル化していません。Servo には既定の応答（選択キャンセル）が返され、ページ側の処理はそのまま続行されます。

### ページから Godot へイベントを送る

拡張は、すべてのページに `window.godot` を自動で注入します。

```js
godot.emit("buy", { item: "potion", price: 120 });   // payload は JSON 文字列として届く
```

JavaScript を書かず、通常のリンクでも送信できます。

```html
<a href="godot:buy?item=potion">購入</a>              <!-- payload はクエリ文字列として届く -->
```

- 前者（`godot.emit`）は、目印を付けた `console.log` を内部の `show_console_message` で拾っています。ページ遷移を起こさないため、ページの状態を崩さずに通信できます。
- 後者（リンク）は、`godot:` スキームへのページ遷移を `request_navigation` で検知してキャンセルすることで実現しています。

### 主な API

| カテゴリ / メソッド・プロパティ | 説明 |
| --- | --- |
| `url`, `view_size`, `autostart`, `ime_anchor` | エクスポートプロパティ（インスペクターから設定可能） |
| `start()`, `stop()`, `is_running()` | ライフサイクル管理 |
| `get_texture()`, `is_texture_flipped_v()`, `needs_external_sampler()`, `get_backend_name()` | テクスチャ取得・表示設定・バックエンド情報 |
| `load_url()`, `reload()`, `go_back()`, `go_forward()` | ナビゲーション（URL 読み込み、再読み込み、戻る、進む） |
| `evaluate_javascript(code) -> int` | JavaScript の実行（実行結果は必ず一度だけ `script_result(id, value, error)` シグナルで返却） |
| `feed_input(event, position)`, `notify_pointer_left()` | 入力イベントの転送、ポインタ離脱の通知 |
| `feed_ime_composition(state, text)`, `feed_ime_preedit(text)`, `cancel_ime_composition()` | IME 入力制御 |
| `respond_to_dialog(accepted, text)`, `respond_to_select(ids)`, `cancel_pending_dialog()`, `has_pending_dialog()` | ダイアログ・選択要素への応答と状態確認 |
| `set_view_size_px(size)` | 表示解像度（サイズ）の設定 |

**主なシグナル**:
`frame_updated`, `title_changed`, `url_changed`, `load_started`, `load_finished`, `cursor_changed`, `console_message`, `bridge_event`, `script_result`, `ime_requested`, `ime_dismissed`, `crashed`, `dialog_alert`, `dialog_confirm`, `dialog_prompt`, `select_element_requested`

### ServoServer

`ServoServer` は、プロセス内で単一の Servo インスタンスを保持するエンジンシングルトンです。
Servo はプロセスごとに一度しか生成できないため、最初の `ServoWebView` が開始したときに初期化され、ゲームが終了するまで保持されます。`ServoWebView` ノードはシーンの切り替えに伴って生成・破棄されますが、Servo 本体は常駐します。

| プロパティ | 説明 |
| --- | --- |
| `enable_webgl2` | WebGL 2.0 を有効にする（既定値: 有効） |
| `enable_webgpu` | WebGPU を有効にする（既定値: 有効。[WebGPU](#webgpu) を参照） |

各設定値は、Servo の初期化時に 1 回だけ読み込まれます。初期化後に変更しようとしても反映されず、警告が出ます。
そのため、最初の `ServoWebView` が開始する前に設定してください（Autoload の `_init()` で設定するか、対象ノードで `autostart` を使っている場合はシーン内の任意の `_ready()` から設定できます。※ `autostart` はシーン全体の準備が完了した後に WebView を開始します）。

```gdscript
# Autoload スクリプトでの設定例
func _init() -> void:
    ServoServer.enable_webgl2 = false
```

## WebGL

WebGL の描画結果も、同じ共有テクスチャ上に描画されます。`demo/web/` に動作確認用のページがあります。

| 項目 | 動作状況 |
| --- | --- |
| WebGL 1.0 | 動作 |
| WebGL 2.0 | `enable_webgl2` を有効にすれば動作 |
| three.js r128 | 動作 |
| three.js 0.180 (最新版) | WebGL 2.0 経由で動作 |

![WebGL 2.0 で描画した three.js 0.180](shot_three.png)

Servo は WebGL 2 を既定で無効にしているため、`ServoServer` は `dom_webgl2_enabled` を切り替える `enable_webgl2` プロパティを用意しています。最新の three.js は WebGL 2 を必要とするため、既定で有効にしています。

```sh
scripts/build.ps1 -Run -Page webgl          # 素の WebGL サンプル
scripts/build.ps1 -Run -Page three-legacy   # three.js r128 サンプル

# ES モジュールを使うページには HTTP サーバー（オリジン）が必要
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/three.html
```

`-Page` は `res://demo/web/<名前>.html` を開きます。`http` で始まる文字列を指定した場合は、そのまま URL として扱います。

## WebGPU

WebGPU の描画結果も、WebGL と同様に同じ共有テクスチャ上に描画されます。
`demo/web/webgpu.html` は canvas への描画を行い、`demo/web/webgpu-compute.html` は canvas を使わずにコンピュートシェーダのみを実行します。

| プラットフォーム / レンダラ | 動作状況 |
| --- | --- |
| Windows / D3D12 | リリースビルドで動作 |
| Windows / Vulkan | リリースビルドで動作 |
| macOS / Metal | 動作 |
| macOS / Vulkan (MoltenVK) | コンピュートシェーダは動作（canvas 描画は未検証） |
| Linux / Vulkan | 動作 |
| Android | アダプタ取得不可（非対応） |

`ServoServer.enable_webgpu` は `dom_webgpu_enabled` 設定を切り替えます（既定値: 有効）。
Cargo の `webgpu` フィーチャを有効にすると、ページ側で使うかどうかに関わらず、wgpu と naga がバイナリに含まれます。

**Android ではアダプタを取得できません。**
`servo-webgpu` は、wgpu インスタンスの生成時に `STRICT_WEBGPU_COMPLIANCE` を無条件で指定します。このフラグはすべてのアダプタに対して `DownlevelFlags::compliant()` の全項目を要求します。その中の 1 つに `SURFACE_VIEW_FORMATS` がありますが、wgpu はこれを `VK_KHR_swapchain_mutable_format` の有無から判定しており、Android では使えないと明記しています。そのためアダプタがすべて除外され、JavaScript の `requestAdapter()` は `null` を返します。
このフラグはスワップチェーンイメージを別フォーマットのビューで表示するための機能ですが、Servo はオフスクリーン描画を行うため、本アドオンの組み込み方ではその機能を使いません。なお、拡張の他の機能には影響せず、同じ端末上でデモは `android-ahardwarebuffer` 経路で動作し、WebGL の確認ページも描画され、セルフチェックも通過します。

**`file://` のページでは WebGPU を使えません。**
`Constellation::handle_wgpu_request` は、ページのホスト名を `registered_domain_name` から取得します。しかし `file://` のページはオリジンが不透明（opaque）でホスト名がありません。そのため Servo は要求に応答せず破棄してしまいます。このとき `navigator.gpu` は存在したままであり、`requestAdapter()` が返す Promise は解決も拒否もされずハング（待機状態）します。WebGPU を使うページは、必ずローカル HTTP サーバー等から配信してください。

```sh
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/webgpu.html
```

**Servo 0.5.0 は WebGPU のスレッドを停止しません。**
`Constellation::handle_shutdown` は、ブラウジングコンテキストグループから WebGPU の通信チャネルを探します。しかし、最後の WebView を閉じた時点でそのグループはすでに削除されています。そのため `WGPU` スレッドとその poller スレッドは、Servo の終了後も動作し続けます。
Godot は終了処理（`Main::cleanup()`）の終わり近くで GDExtension をアンロードするため、poller スレッドがすでにメモリから解放されたコードを実行しようとし、Windows のデバッグビルドでは終了時にアクセス違反（`0xC0000005`）が発生します。
本拡張では `src/module_pin.rs` によりライブラリをプロセス空間に固定（ピン留め）してアンロードを防ぐことで、このクラッシュを回避しています（スレッドの残存はビルドの種類を問わず発生するため、この固定は常に行われます。アンロード自体の動作を確認したい場合は、環境変数 `GODOT_SERVO_NO_PIN=1` で固定を無効化できます）。

**Windows では、WebGPU を有効にしたデバッグビルドで Godot が GPU デバイスを失います。**
Servo は wgpu インスタンスを `InstanceFlags::from_build_config()` で生成します。この関数は、`debug_assertions` が有効な場合に GPU バリデーションも有効にします。すると wgpu の DX12 バックエンドが D3D12 のデバッグレイヤーを有効化するため、それより前に生成された既存の D3D12 デバイスがすべて失われてしまいます。
D3D12 レンダラでは Godot 自身のデバイスがこれに巻き込まれ、次のリソース確保がエラーコード `0x887a0005` で失敗します。また Vulkan レンダラでも、DX12 バックエンドが列挙された瞬間に Godot の Vulkan デバイスが失われます（`VK_ERROR_DEVICE_LOST`）。
リリースビルドではバリデーションが有効化されないため影響を受けません。Windows で WebGPU を確認する際は、必ずリリースビルド（`-Release`）を使用してください。

## 設計ノート

### シングルバッファ

スワップチェーンを使わず、オフスクリーンの surfman サーフェスを 1 枚だけ確保して保持し続けます。これにより、Godot に渡したテクスチャの RID が WebView の生存期間中ずっと有効になり、プロジェクト側はテクスチャを 1 回バインドするだけで済みます。
その引き換えとして、Servo が描画している最中に Godot がサンプリングしてしまう競合の可能性があります。これを防いでいるのが厳密な実行順序です。同一フレーム内において、`paint()`、ブリット（Blit）、`glFlush()`、そして Godot 自身の描画が、この順序でメインスレッド上で直列に実行されます。

### 同期

Godot の `RenderingDevice` には、コマンドのサブミットに外部セマフォを結びつける手段がありません（`submit()` と `sync()` はローカルデバイス専用です）。そのため、GPU 上の描画順序は `glFlush()` に依存しています。wgpu 側の同種の相互運用ライブラリである `wgpu-native-texture-interop` でも同様で、「組み込みの同期機能では明示的なセマフォをまだ扱えない」と説明されています。

この同期の欠如は単なる理論上の懸念ではなく、表示方式によっては現実の問題になり得ます。現状問題が表面化しないのは、フラッシュを指示したのと同じメインスレッドから、1 フレームに 1 回だけパネルを合成しているためです。独自の非同期タイミングでテクスチャを再サンプリングするコンポジタでは、この前提が成り立ちません。
代表例が Android XR などの VR/XR 環境で、GPU 間同期がないと頭を素早く動かした際にテクスチャのティアリングが発生します。その場合は、エクスポート可能な `SYNC_FD` セマフォを付与したコマンドバッファを発行して同期する必要があります。本アドオンの設計でも同様の拡張が可能であり、インポートで既に使っている `additional_device_extensions` 経由で `VK_KHR_external_semaphore_fd` を要求することで対応できます。

### GL コンテキストの管理

Servo は独自の GL コンテキストを保持し、描画のたびにそれをカレント（アクティブ）にします。Godot が Vulkan / D3D12 / Metal で描画している環境では、同一スレッド上で他に GL コンテキストを使うものはありません。
しかし Android の Compatibility レンダラだけは例外で、Godot 自身も同じスレッドの EGL コンテキストで描画しています。そこで `src/gl_guard.rs` を使い、Servo にスレッドを渡す直前にカレントコンテキストを退避し、処理後に元のコンテキストへ復元します。また、Linux と Android の Vulkan 経路でも共有メモリ上にテクスチャを生成・解放する際に GL API を呼び出しますが、必ず Servo のコンテキストを明示的にカレントにしてから呼び出します。

同様のルールは、Godot 側で GL を呼び出すすべての処理に当てはまります。例えば `ExternalTexture` はバッファ ID を設定した時点で内部的に `glEGLImageTargetTexture2DOES` を呼び出すため、Android ブリッジ側では `ExternalTexture` を生成する前に必ず Godot ホスト側のコンテキストへ復元しています。

### RenderingDevice 経路で 1 回コピーを行う理由

`Texture2DRD` は内部で `texture_create_shared()` を呼び出しますが、Godot の `RenderingDevice` ドライバは D3D12 / Vulkan のどちらも、拡張からインポートしたテクスチャをそのままでは正常に表示できません。
- **D3D12 ドライバ**: 自前のアロケーション（メモリ確保領域）を持たないテクスチャを明示的に弾くため、インポートしたテクスチャをそのまま渡すと真っ白に描画されます。
- **Vulkan ドライバ**: `|| created_from_extension` の除外条件があるため受け付けはするものの、サンプリング結果が真っ黒になります。

両ドライバで共通する原因は、`texture_create_from_extension()` が外部イメージに対してビューとトラッカーのエントリを作るだけで、イメージやメモリの所有権を持たず、キューファミリーの所有権移動（ownership transfer）も行われない点にあります。そのため Godot のレイアウトトラッカーは外部イメージの正確な状態を把握できず、直接サンプリングしようとすると「Godot が一度も確立していないレイアウト」から読み出すことになってしまいます。
一方、コピー操作であればコピー元・コピー先の両方が Godot の管理対象となるため、Godot のリソース管理の枠組みの中で一貫したレイアウト遷移と正常な読み出しが行われます。

そのため両方の経路とも、インポートしたテクスチャを `RenderingDevice.texture_copy()` で Godot 所有のテクスチャへコピーし、そちらを表示用に使用しています。このコピーは GPU 内で完結するため、CPU との往復オーバーヘッドは生じません（Metal 経路だけは例外で、そもそも `Texture2DRD` を介さずテクスチャを直接渡せるためコピーは不要です）。

なお、このコピーには 1 つ注意点があります。メインの `RenderingDevice` はコピーを即座に実行するのではなく、Godot のフレームコマンドバッファに記録するため、実際のコピーは呼び出しより少し後に実行されます。ただし、コピー先を読むのは Godot 自身の描画処理であり、同じフレームグラフ内で順序付けられるため完了を待つ必要はありません。一方、Servo 側の書き込みとの同期順序については、前述の「同期」で説明した `glFlush()` に依存する形となります。

### CPU フォールバックが毎フレームメモリ確保しない理由

CPU リードバック経路では、ピクセルバッファと `Image` オブジェクトを 2 組保持し、フレームごとに交互に切り替えて使用します。
Godot の `PackedByteArray` は Copy-on-Write（CoW）のため、現在の `Image` が参照しているバッファに直接書き込むとバッファの複製が発生してしまいます。しかし、もう一方のバッファへ交互に書き込めば参照カウントが 1 のまま維持されるため、`glReadPixels` の出力先バッファをコピーなしでそのままテクスチャデータとして利用できます。2 組で足りるのは、別スレッドの `RenderingServer` であってもキューイングされた `texture_2d_update` を 1 フレーム以内に確実に消化するためです。

1280×720（リリースビルド）での計測では、この方式による 1 フレームあたりの更新時間は 1.34 ms です。フレームごとにバッファを新規確保（アロケーション）する実装にした場合は 1.93 ms かかり、毎フレーム約 7 MB のメモリ確保と全画面コピーが 1 回分余計に発生します。

### Servo と Godot の使用 GPU を一致させる

テクスチャの共有は、Servo と Godot が同一の GPU 上で動作している場合にのみ機能します。
Godot はデバイスのスコア、または起動オプション `--gpu-index` に基づいて GPU を選びます。一方、Servo が使用する GPU は surfman の選択ルール（Windows では「Intel 製以外の最初のアダプタ」、Mesa では PRIME 指定のアダプタ `DRI_PRIME=1`）で決まります。そのため、PC に複数の GPU が搭載されている環境では両者が一致しないことがあります。不一致の場合、テクスチャのインポートに失敗し、CPU リードバックへフォールバックしてしまいます。

Windows では、本拡張が両者の GPU を一致させます。`src/gpu_adapter.rs` が Godot の描画しているアダプタの LUID を取得し（D3D12 では `ID3D12Device::GetAdapterLuid`、Vulkan では `VkPhysicalDeviceIDProperties::deviceLUID`）、DXGI のアダプタ一覧から同じアダプタを特定して surfman に渡します。これにより、GPU が 2 つある環境で `--gpu-index` に 2 つ目の GPU を指定しても、`cpu-readback` に落ちずに `d3d12-shared-nt-handle` の共有経路を維持できます。

Linux と Android では同様の方法で指定できません。surfman の EGL バックエンドが GPU をデバイス名や ID で指定する機能を提供しておらず、選べるのが hardware、low-power、software だけだからです。そこで本拡張は、不一致を検出してログに報告する仕組みにしています。opaque fd 経路では、メモリを確保する前に Godot の Vulkan `deviceUUID` と GL の `GL_DEVICE_UUID_EXT` を比較します。両者が異なる場合は、それぞれが使っている GPU 名をログに出力したうえで、安全にリードバック経路へ切り替えます（ドライバ内部でクラッシュするより、原因をログに残してフォールバックするほうが対処しやすいためです）。なお、どちらか一方でも UUID を取得できない場合（GL に `glGetUnsignedBytevEXT` がない、または Vulkan 1.1 未満）は判定をスキップします。

### Linux で jemalloc を再ビルドする理由

Servo は `servo-allocator` 経由で jemalloc を取り込みますが、jemalloc は既定で initial-exec TLS（スレッドローカルストレージ）モデルを使用します。これにより共有ライブラリ（.so）に `STATIC_TLS` フラグが設定され、`PT_TLS` が glibc の静的 TLS 予備領域（static TLS surplus）を超えてしまうため、Godot が `dlopen` で読み込めなくなります。
そこで `Cargo.toml` では Linux に限り `tikv-jemalloc-sys` を直接宣言し、まさにこの問題のために用意されている `disable_initial_exec_tls` フィーチャを有効にしています。

## 未対応の機能・制限事項

- **背景ゲーム画面の取り込み（カラーフィードバック）**: CSS の `backdrop-filter` で Web ページの背後にあるゲーム画面をぼかす用途です。Godot 側は `CompositorEffect` で対応できますが、Servo 側に `WebRenderImageHandlerType` を追加するフォークが必要です。
- **iOS**: surfman も Servo も iOS を対象にしておらず、iOS では JIT コンパイルや `dlopen` が禁止されているため非対応です。
- **ファイル選択・色選択・コンテキストメニュー**: Servo 側には 3 つとも用意されていますが、本拡張ではシグナル化していません。既定の応答（選択キャンセル）が返されます。
- **複数の `ServoWebView` ノードの同時配置**: 設計上は単一の `Servo` インスタンスを共有しますが、十分な動作検証は行われていません。

## 関連プロジェクト

Servo を Godot に組み込むアドオンは、本プロジェクトのほかに 2 つあります。どちらも `SoftwareRenderingContext` と `read_to_image()` を使って描画しており、これは本アドオンが GPU 共有を使えないときにフォールバックする CPU リードバックと同じ仕組みです。

| プロジェクト | 描画方式 | ライセンス |
| --- | --- | --- |
| [Decapitated/Godot-Servo](https://github.com/Decapitated/Godot-Servo) | CPU リードバック | LGPL-3.0 |
| [emanuelbertey/web-servo-godot](https://github.com/emanuelbertey/web-servo-godot) | CPU リードバック | ライセンス表記なし |
| **godot-servo（本プロジェクト）** | **GPU 共有テクスチャ** + CPU フォールバック | MIT / Apache-2.0 |

GPU 共有による高速な描画が不要であれば、`web-servo-godot` のほうが Servo の組み込み API（閲覧履歴、フォーカス、ファビコン、全画面表示、権限や認証の要求、ファイル・色・コンテキストメニューの選択など）をより広く公開しています。ただしライセンスの記載がないため、既定では著作権者による全権留保（All Rights Reserved）となる点にご注意ください。

## コントリビューション

コミットメッセージは [Conventional Commits](https://www.conventionalcommits.org/) に従ってください。リリースノートがコミットログから自動生成されるため、仕様に従っていないものは除外されます。利用可能なコミット種別やスコープ、プッシュ前に実行すべきテストなどについては [CONTRIBUTING.md](CONTRIBUTING.md) を参照してください。

## ライセンス

本アドオンは [Apache License 2.0](LICENSE-APACHE) と [MIT License](LICENSE-MIT) のデュアルライセンスです。用途に合わせてどちらかを選択できます。

- **Servo 本体**: MPL-2.0 です。本クレートは Servo を改変せずに依存ライブラリとして使っているだけなので、ファイル単位のコピーレフト条項が利用者のコードに及ぶことはありません。
- **同梱ライブラリ**: `demo/web/vendor/` に同梱している three.js のビルドは MIT ライセンスで、元のライセンスヘッダーを残しています。
