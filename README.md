# godot-servo

English | [日本語](README_ja.md)

`godot-servo` embeds [Servo](https://servo.org/), the Rust browser engine, into Godot 4 as a
GDExtension and hands the rendered page to Godot **as a GPU texture**.

You put web UI on an in-game panel, forward clicks, scrolling, and keystrokes to it, and receive
button presses back as Godot signals.

![A Servo-rendered page on a 3D panel in Godot](shot_d3d12.png)

## Features

- **No CPU round trip.** Servo renders into an offscreen GPU surface that Godot samples directly.
- **Real web rendering.** HTML, CSS, JavaScript, WebGL 1 and 2, and three.js, including the current
  release.
- **In-game, not an overlay.** The result is a `Texture2D`, so it goes on a 3D panel, a material, or
  a `TextureRect`.
- **Two-way communication.** Forward input to the page; receive page events as signals.
- **A fallback that always loads.** Where GPU sharing isn't available, the extension falls back to
  `glReadPixels` instead of refusing to start.

## Status

Verified on Windows with the Direct3D 12 renderer.

```
--- godot-servo self check ---
  path: d3d12-shared-nt-handle
  OK   bridge_event (godot.emit)  (expected 'ready')
  OK   evaluate_javascript / script_result  (button at (95.2, 189.4))
  OK   click -> onclick -> bridge_event  (expected 'buy')
  OK   focus input -> ime_requested  (caret [P: (28.0, 509.0), S: (220.0, 36.0)])
  OK   ime composition -> input value  (value '日本語')
  OK   wheel -> scroll  (scrollTop 0 -> 608)
--- 0 failed ---
```

| Platform | Path | Status |
| --- | --- | --- |
| Windows / D3D12 | ANGLE D3D11 shared texture (NT handle) to `ID3D12Resource` | **Verified** |
| macOS / Metal | IOSurface to `MTLTexture` | Written, never compiled |
| Windows / Vulkan (default) | — | Falls back to CPU readback |
| Linux / Vulkan | — | Falls back to CPU readback |
| Anything else | `glReadPixels` to `ImageTexture` | Verified |

Call `ServoWebView.get_backend_name()` to see which path a running instance took.

To get GPU sharing on Vulkan, Godot needs
[godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940), which lets a project
enable extra Vulkan device extensions — `VK_KHR_external_memory_win32` on Windows, or
`VK_EXT_external_memory_dma_buf` on Linux.

## Requirements

- **Godot 4.4 or later.** `RenderingDevice.texture_create_from_extension()` and
  `get_driver_resource()` reached GDExtension in 4.4.
- **Rust 1.94 or later**, to build from source.
- On Windows, the **Direct3D 12** renderer for the GPU path. Official Godot builds ship it; set
  `rendering/rendering_device/driver.windows` to `d3d12`.

## Repository layout

The repository root is the Godot project. Clone it, open it in Godot, and it runs.

```
godot_servo.gdextension          Extension manifest, at the project root
addons/godot_servo/bin/          Build output (not committed)
  windows/godot_servo.x86_64.dll
  windows/libEGL.dll             ANGLE, loaded by name at runtime
  windows/libGLESv2.dll
demo/                            Demo scenes and pages
project.godot
scripts/build.ps1 | build.sh     Build, then stage into bin/
src/                             The GDExtension itself (Rust)
```

A release ships two things: `godot_servo.gdextension` and `addons/godot_servo/`. Drop both into your
own project.

## Build

```sh
scripts/build.ps1                 # Windows: build and stage (debug)
scripts/build.ps1 -Release
./scripts/build.sh                # Linux and macOS
./scripts/build.sh --release
```

`build.rs` copies the `libEGL.dll` and `libGLESv2.dll` that mozangle builds into
`target/<profile>/`, and the build script stages them into `addons/godot_servo/bin/windows/`.
Surfman loads ANGLE by filename at runtime, so both DLLs have to sit beside the extension.
`src/angle_loader.rs` preloads them by absolute path.

## Run the demo

```sh
export GODOT=~/.local/godot/4.7.2-stable/Godot_v4.7.2-stable_win64_console.exe

scripts/build.ps1 -Run                 # 3D in-game browser
scripts/build.ps1 -Run -Scene flat     # 2D, for isolating problems
scripts/build.ps1 -Test                # input and signal self check
```

`project.godot` sets the Windows renderer to `d3d12`. The demo still runs on the default `vulkan`
renderer, but it takes the CPU readback path.

## Quickstart

```gdscript
var browser := ServoWebView.new()
browser.view_size = Vector2i(1280, 720)
browser.url = "https://example.com"
add_child(browser)

browser.frame_updated.connect(func() -> void:
    material.albedo_texture = browser.get_texture()
    if browser.is_texture_flipped_v():
        # Only the macOS IOSurface path arrives upside down.
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
`"end"` is what gets committed.

### API

| | |
| --- | --- |
| `url`, `view_size`, `autostart`, `enable_webgl2`, `enable_webgpu` | Exported properties |
| `start()`, `stop()`, `is_running()` | Lifetime |
| `get_texture()`, `is_texture_flipped_v()`, `get_backend_name()` | Display |
| `load_url()`, `reload()`, `go_back()`, `go_forward()` | Navigation |
| `evaluate_javascript(code) -> int` | The result arrives on `script_result(id, value)` |
| `feed_input(event, position)`, `notify_pointer_left()` | Input |
| `feed_ime_composition(state, text)`, `cancel_ime_composition()`, `ime_anchor` | IME |
| `set_view_size_px(size)` | Resolution |

Signals: `frame_updated`, `title_changed`, `url_changed`, `load_started`, `load_finished`,
`cursor_changed`, `console_message`, `bridge_event`, `script_result`, `ime_requested`,
`ime_dismissed`

## WebGL and WebGPU

Servo puts WebGL and WebGPU output on the same shared texture. `demo/web/` holds check pages.

| | Result |
| --- | --- |
| WebGL 1.0 | Works |
| WebGL 2.0 | Works, with `enable_webgl2` |
| three.js r128 | Works |
| three.js 0.180 (current) | Works, over WebGL 2.0 |
| WebGPU | **Not usable. The process crashes** |

![three.js 0.180 rendering over WebGL 2.0](shot_three.png)

Servo disables WebGL 2 and WebGPU by default, so `ServoWebView` exposes `enable_webgl2` and
`enable_webgpu`, which set the `dom_webgl2_enabled` and `dom_webgpu_enabled` preferences.

```sh
scripts/build.ps1 -Run -Page webgl          # plain WebGL
scripts/build.ps1 -Run -Page three-legacy   # three.js r128

# Pages that use ES modules need a real origin.
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/three.html
```

`-Page` opens `res://demo/web/<name>.html`. A value starting with `http` is used as a URL.

### WebGPU details

Build with the `webgpu` feature and set `enable_webgpu`, and `navigator.gpu` appears.
`requestAdapter()`, `requestDevice()`, and compute shaders all work correctly:
`demo/web/webgpu-compute.html` reads back `[0, 20, 126]` from the GPU.

The process then **crashes with a segmentation fault** — always when presenting to a canvas, and on
teardown even without one. The cause is unknown. The property defaults to off.

Opening such a page over `file://` hangs in `requestAdapter()` instead. Over `http://127.0.0.1` it
gets further.

### Enabling the webgpu feature

The build fails as shipped. `ipc-channel` requires `windows ^0.61`, which pulls `gpu-allocator`
(`windows >=0.53, <=0.62`) down to 0.61 and breaks the types it shares with `wgpu-hal`, which
requires `windows 0.62`.

```sh
cargo update -p gpu-allocator   # move gpu-allocator's windows to 0.62
```

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

### Why D3D12 copies once

Godot's D3D12 driver accepts only textures that own an allocation in
`_texture_create_shared_from_slice()`, and rejects imported ones. `Texture2DRD` calls
`texture_create_shared()` internally, so passing the imported texture straight to it **renders
white**.

The Vulkan driver allows this case through a `|| created_from_extension` exemption. D3D12 has no such
exemption, which makes this a gap in that driver.

As a workaround, the D3D12 path copies the imported texture into a Godot-owned one with
`RenderingDevice.texture_copy()`. The copy stays on the GPU, so no CPU round trip appears. The Metal
driver has no such restriction, so that path passes the texture directly.

## Not implemented

- **Scene color feedback**, for blurring the game behind the page with CSS `backdrop-filter`.
  `CompositorEffect` covers the Godot side, but the Servo side needs a fork that adds a
  `WebRenderImageHandlerType`.
- **WebGPU**, for the reason above.
- **Multiple `ServoWebView` nodes.** They share one `Servo` instance by design, but that is untested.
- **macOS and Linux on real hardware.**

## License

Servo is MPL-2.0.
