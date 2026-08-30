# godot-servo

English | [日本語](README_ja.md)

`godot-servo` embeds [Servo](https://servo.org/), the Rust browser engine, into Godot 4 as a
GDExtension and hands the rendered page to Godot **as a GPU texture**.

Put web UI on an in-game panel, forward pointer, touch, and keyboard input to it, and receive
button presses back as Godot signals.

![A Servo-rendered page on a 3D panel in Godot](shot_d3d12.png)

## Features

- **No CPU round trip.** Servo renders into an offscreen GPU surface that Godot samples directly.
- **Real web rendering.** HTML, CSS, JavaScript, WebGL 1 and 2, and three.js, including the current
  release.
- **In-game, not an overlay.** The result is a `Texture2D`, so it goes on a 3D panel, a material, or
  a `TextureRect`.
- **Mouse, touch, and keyboard input**, including IME composition for Japanese and other languages.
- **Two-way communication.** Forward input to the page; receive page events as signals.
- **A fallback that always loads.** Where GPU sharing isn't available, the extension reads pixels
  back through the CPU instead of refusing to start.

## Supported platforms

Each platform shares GPU memory through whatever its graphics stack provides. Where no such path
exists, the extension falls back to `glReadPixels`, which costs a round trip per frame but works
everywhere.

| Platform | Path | Runtime status |
| --- | --- | --- |
| Windows / D3D12 | ANGLE D3D11 shared texture (NT handle) to `ID3D12Resource` | Verified |
| Android / Compatibility | `AHardwareBuffer` to `EGLImage` to `ExternalTexture` | Verified |
| macOS / Metal | IOSurface to `MTLTexture` | Verified |
| Windows / Vulkan | CPU readback | Verified |
| Linux / Vulkan | CPU readback | Verified |
| Android / Forward+ or Mobile | CPU readback | Verified |

Call `ServoWebView.get_backend_name()` to see which path a running instance took.

Two renderer settings decide whether the GPU path is available:

- **Windows** defaults to Vulkan. Set `rendering/rendering_device/driver.windows` to `d3d12` for the
  shared-texture path. Vulkan could work too, but it needs
  [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940), which lets a project
  enable `VK_KHR_external_memory_win32`.
- **Android** needs the Compatibility renderer, set through
  `rendering/renderer/rendering_method.mobile`. In the RenderingDevice backends,
  `texture_external_initialize()` is a stub, so Forward+ and Mobile have nothing to receive an
  external texture with.

## Requirements

- **Godot 4.4 or later.** `RenderingDevice.texture_create_from_extension()` and
  `get_driver_resource()` reached GDExtension in 4.4.
- **Rust 1.94 or later**, to build from source.
- For Android, **cargo-ndk** and an NDK, on a Linux or macOS host.

## Repository layout

The repository root is the Godot project. Clone it, open it in Godot, and it runs.

```
godot_servo.gdextension          Extension manifest, at the project root
addons/godot_servo/bin/          Build output (not committed)
  windows/godot_servo.x86_64.dll
  windows/libEGL.dll             ANGLE, loaded by name at runtime
  windows/libGLESv2.dll
  android/arm64-v8a/libgodot_servo.so
demo/                            Demo scenes and pages
project.godot
scripts/build.ps1 | build.sh     Build, then stage into bin/
src/                             The GDExtension itself (Rust)
```

A release archive contains `addons/godot_servo/` complete, with `bin/` filled in and
`godot_servo.gdextension` inside it. Merge that one folder into your project. The manifest sits at
the repository root here only because the repository root is the demo project; the `res://` paths
inside it are absolute, so either location resolves the same. Keep one copy — two manifests register
the extension twice.

## Build

```sh
scripts/build.ps1                 # Windows: build and stage (debug)
scripts/build.ps1 -Release
./scripts/build.sh                # Linux and macOS
./scripts/build.sh --release
./scripts/build.sh --android      # Android arm64-v8a, needs cargo-ndk
```

Plain `cargo build` doesn't stage anything. Use the build script. Besides copying the library into
`addons/godot_servo/bin/`, it stages the `libEGL.dll` and `libGLESv2.dll` that mozangle produces,
picking them out of that crate's `OUT_DIR` after the build finishes. Surfman loads ANGLE by filename
at runtime, so both DLLs have to sit beside the extension, and `src/angle_loader.rs` preloads them
by absolute path.

### Android

Cross-compile from Linux or macOS, including WSL. A Windows host cannot do it: no host toolchain
satisfies both of Servo's C dependencies. jemalloc's `configure` rejects the MSVC host triple, and
glsl-optimizer does not compile under mingw.

Point `ANDROID_NDK_HOME` at an NDK, then:

```sh
export ANDROID_NDK_HOME=~/android/android-ndk-r27c
./scripts/build.sh --release --android
```

The script drops a `libgcc.a` stub containing `INPUT(-lunwind)` into `target/` and puts it on the
link path, because NDK r23 removed libgcc in favour of libunwind while something in the dependency
graph still asks the linker for `-lgcc`.

## Run the demo

```sh
export GODOT=~/.local/godot/4.7.2-stable/Godot_v4.7.2-stable_win64_console.exe

scripts/build.ps1 -Run                 # 3D in-game browser
scripts/build.ps1 -Run -Scene flat     # 2D, for isolating problems
scripts/build.ps1 -Test                # input and signal self check
```

The self check drives the extension end to end and reports what it verified:

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

### Build an APK

```sh
./scripts/build.sh --release --android
llvm-strip --strip-all addons/godot_servo/bin/android/arm64-v8a/libgodot_servo.so

godot --headless --path . --export-debug Android godot-servo.apk
adb install -r godot-servo.apk
```

Strip the library. For arm64-v8a it is 1474 MB in debug, 170 MB in release, and 119 MB after
`--strip-all`, which gives a 146 MB APK. Most of that is SpiderMonkey, Stylo, WebRender, and the ICU
data.

## Quickstart

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

# Forward input. position is in WebView pixels.
browser.feed_input(event, local_position)

# Receive events from the page.
browser.bridge_event.connect(func(name: String, payload: String) -> void:
    print(name, " ", payload)
)
```

Two properties of the texture depend on the platform, and both demo scenes handle them:

- `is_texture_flipped_v()` is true on the macOS IOSurface path, which has no transfer step in which
  to correct GL's bottom-left origin. Flip it in the material.
- `needs_external_sampler()` is true on Android, where the buffer arrives as a
  `GL_TEXTURE_EXTERNAL_OES` texture. A `sampler2D` reads it as black, so the material has to use a
  shader that declares `samplerExternalOES`. `demo/servo_external.gdshader` is a minimal one.

### Forward input

`feed_input(event, position)` accepts mouse, touch, and keyboard events. `position` is in WebView
pixels, so convert first:

- For a `TextureRect`, subtract the control's position and scale by `view_size / rect.size`.
- For a 3D panel, take the hit position from `CollisionObject3D.input_event`, convert it to UV, then
  multiply by `view_size`. `demo/main.gd` shows that conversion in both directions.

Pass mouse and touch events through together. Godot's
`input_devices/pointing/emulate_mouse_from_touch`, on by default, turns every touch into a synthetic
mouse event as well; `feed_input()` drops any event whose `device` is `DEVICE_ID_EMULATION`, so one
gesture stays one gesture. The same rule covers `emulate_touch_from_mouse` in the other direction.

Touch reaches Servo as real touch events, so the page receives `touchstart`, `touchmove`, and
`touchend`, and Servo's own touch handler does the scrolling and flinging.

### Type Japanese and other IME input

When the page focuses an editable element, Servo notifies the extension, which turns on the
operating system IME and emits `ime_requested(caret, multiline)`. The `caret` rectangle is in
WebView pixels.

The IME candidate window is placed by the OS in window coordinates, so the extension can't guess
where a 3D panel appears on screen. Project the caret yourself and assign `ime_anchor`:

```gdscript
browser.ime_requested.connect(func(caret: Rect2, _multiline: bool) -> void:
    var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
    browser.ime_anchor = camera.unproject_position(view_pixels_to_world(bottom_left))
)
```

Both demo scenes do this. Without it, candidates appear at the top-left of the window.

To drive composition from your own input UI instead of the OS IME, call
`feed_ime_composition(state, text)` with `"start"`, `"update"`, or `"end"`. The text passed with
`"end"` is what gets committed. `feed_ime_preedit(text)` follows the same route the OS IME takes:
pass the preedit, then an empty string, then send the committed characters through `feed_input()`.

**Known limitation.** Cancelling a conversion leaves the preedit text in the field. Servo's
`compositionend` handler only clears the selection when the data is empty, and its composition API
offers no way to delete the preedit, so there is nothing to send.

### Send events from the page to Godot

The extension injects `window.godot` into every page.

```js
godot.emit("buy", { item: "potion", price: 120 });   // payload arrives as a JSON string
```

Plain links work too, with no JavaScript:

```html
<a href="godot:buy?item=potion">Buy</a>              <!-- payload is the query string -->
```

The first form goes through a marked `console.log` that `show_console_message` picks up. It triggers
no navigation, so it leaves page state alone. The second intercepts navigation to the `godot:` scheme
in `request_navigation` and denies it.

### API

| | |
| --- | --- |
| `url`, `view_size`, `autostart`, `enable_webgl2`, `ime_anchor` | Exported properties |
| `start()`, `stop()`, `is_running()` | Lifetime |
| `get_texture()`, `is_texture_flipped_v()`, `needs_external_sampler()`, `get_backend_name()` | Display |
| `load_url()`, `reload()`, `go_back()`, `go_forward()` | Navigation |
| `evaluate_javascript(code) -> int` | The result arrives on `script_result(id, value)` |
| `feed_input(event, position)`, `notify_pointer_left()` | Input |
| `feed_ime_composition(state, text)`, `feed_ime_preedit(text)`, `cancel_ime_composition()` | IME |
| `set_view_size_px(size)` | Resolution |

Signals: `frame_updated`, `title_changed`, `url_changed`, `load_started`, `load_finished`,
`cursor_changed`, `console_message`, `bridge_event`, `script_result`, `ime_requested`,
`ime_dismissed`

## WebGL

Servo puts WebGL output on the same shared texture. `demo/web/` holds check pages.

| | Result |
| --- | --- |
| WebGL 1.0 | Works |
| WebGL 2.0 | Works, with `enable_webgl2` |
| three.js r128 | Works |
| three.js 0.180 (current) | Works, over WebGL 2.0 |

![three.js 0.180 rendering over WebGL 2.0](shot_three.png)

Servo disables WebGL 2 by default, so `ServoWebView` exposes `enable_webgl2`, which sets the
`dom_webgl2_enabled` preference. It defaults to on, because current three.js requires it.

```sh
scripts/build.ps1 -Run -Page webgl          # plain WebGL
scripts/build.ps1 -Run -Page three-legacy   # three.js r128

# Pages that use ES modules need a real origin.
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/three.html
```

`-Page` opens `res://demo/web/<name>.html`. A value starting with `http` is used as a URL.

## Design notes

### Single buffering

The extension allocates one offscreen surfman surface and keeps it, rather than using a swap chain.
The RID of the texture handed to Godot then stays valid for the life of the view. In exchange, Godot
can sample while Servo draws, but keeping `paint()`, the blit, `glFlush()`, and Godot's own render
in that order on the main thread has caused no visible problem.

### Synchronization

Godot's `RenderingDevice` offers no way to attach an external semaphore to a submission — `submit()`
and `sync()` are for local devices only. Ordering therefore relies on `glFlush()`. The equivalent
wgpu library, `wgpu-native-texture-interop`, is in the same position: explicit semaphores are "not
yet handled by any built-in synchronizer".

### Lending the GL context

Servo keeps its own GL context and makes it current to draw. Where Godot renders with Vulkan, D3D12,
or Metal, nothing else on the thread wants a GL context. Android's Compatibility renderer is the
exception: Godot draws from its own EGL context on the same thread, so `src/gl_guard.rs` captures
whatever is current before Servo is given the thread and restores it afterwards.

The same rule applies to anything on Godot's side that issues GL calls. `ExternalTexture` calls
`glEGLImageTargetTexture2DOES` when its buffer id is set, so the Android bridge restores the host
context before creating it.

### Why the D3D12 path copies once

Godot's D3D12 driver accepts only textures that own an allocation in
`_texture_create_shared_from_slice()`, and rejects imported ones. `Texture2DRD` calls
`texture_create_shared()` internally, so passing the imported texture straight to it renders white.
The Vulkan driver allows this case through a `|| created_from_extension` exemption; D3D12 has no
such exemption.

The D3D12 path therefore copies the imported texture into a Godot-owned one with
`RenderingDevice.texture_copy()`. The copy stays on the GPU, so no CPU round trip appears. The Metal
driver has no such restriction, so that path hands the texture over directly.

### The CPU fallback allocates nothing per frame

The readback path keeps two pixel buffers and two `Image` objects and alternates between them.
`PackedByteArray` is copy-on-write, so writing into the buffer the current `Image` references would
duplicate it; writing into the other one leaves the reference count at 1, and `glReadPixels` lands
straight in what becomes the texture's contents. Two sets are enough because a threaded
`RenderingServer` consumes the queued `texture_2d_update` within one frame.

At 1280×720 in a release build, that takes the per-frame update from 1.93 ms to 1.34 ms, and removes
about 7 MB of allocation and one full-frame copy per frame.

### Why jemalloc is rebuilt on Linux

Servo pulls jemalloc in through `servo-allocator`, and jemalloc defaults to initial-exec TLS. That
sets `STATIC_TLS` on the shared object and pushes `PT_TLS` past glibc's static TLS surplus, so Godot
cannot `dlopen` it. `Cargo.toml` therefore declares `tikv-jemalloc-sys` directly on Linux with
`disable_initial_exec_tls`, the feature that crate ships for exactly this case.

## Not supported

- **WebGPU.** Servo's implementation crashes this embedding: device creation and compute shaders
  work, but the process dies with SIGSEGV, always when presenting to a canvas and on teardown even
  without one. The `webgpu` feature is off, and enabling it also pulls wgpu and naga into an already
  large binary. `demo/web/webgpu.html` and `webgpu-compute.html` are there for a future retest.
- **Scene color feedback**, for blurring the game behind the page with CSS `backdrop-filter`.
  `CompositorEffect` covers the Godot side, but the Servo side needs a fork that adds a
  `WebRenderImageHandlerType`.
- **iOS.** Neither surfman nor Servo targets it, and iOS forbids JIT and `dlopen`.
- **A GPU sharing path for Linux**, which would need
  [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940) for
  `VK_EXT_external_memory_dma_buf`.
- **Multiple `ServoWebView` nodes.** They share one `Servo` instance by design, but that is untested.

## Related projects

Two other addons embed Servo in Godot. Both render through `SoftwareRenderingContext` and
`read_to_image()`, which is the same CPU readback this project falls back to when GPU sharing is
unavailable.

| | Rendering | License |
| --- | --- | --- |
| [Decapitated/Godot-Servo](https://github.com/Decapitated/Godot-Servo) | CPU readback | LGPL-3.0 |
| [emanuelbertey/web-servo-godot](https://github.com/emanuelbertey/web-servo-godot) | CPU readback | none stated |
| this project | GPU shared texture, CPU fallback | MIT / Apache-2.0 |

If you don't need the GPU path, `web-servo-godot` covers more ground today: more signals, a
JavaScript API, and working Linux and Android builds. Note it states no license, so it is
all-rights-reserved by default.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT license](LICENSE-MIT) at your option.

Servo itself is MPL-2.0. This crate depends on it without modifying it, so the file-level copyleft
does not reach your own code. The three.js builds vendored under `demo/web/vendor/` are MIT and
carry their original license headers.
