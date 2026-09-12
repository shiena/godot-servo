<p align="center">
  <img src="addons/godot_servo/icon.svg" alt="godot-servo logo" width="128" height="128">
</p>

<h1 align="center">godot-servo</h1>

<p align="center">
  English | <a href="README_ja.md">日本語</a>
</p>

`godot-servo` embeds [Servo](https://servo.org/), the Rust browser engine, into Godot 4 as a GDExtension and hands the rendered page to Godot **as a GPU texture**.

Put web UI directly on in-game panels, forward pointer, touch, and keyboard input seamlessly, and receive button clicks and page events back as Godot signals.

![A Servo-rendered page on a 3D panel in Godot](shot_d3d12.png)

## Features

- **No CPU overhead**: Servo renders into an offscreen GPU surface that Godot samples directly.
- **Full web rendering**: Supports HTML, CSS, JavaScript, WebGL 1 / 2, and three.js (including the latest release).
- **In-game texture, not an overlay**: Output is provided as a standard `Texture2D`, so you can apply it directly to 3D panels, materials, or a `TextureRect`.
- **Mouse, touch, and keyboard input**: Full input forwarding, including IME text composition for Japanese and other languages.
- **Two-way communication**: Forward user inputs to the page and receive page events back as Godot signals.
- **Reliable fallback**: If GPU memory sharing is unavailable, it automatically falls back to CPU readback instead of failing to start.

## Supported platforms

GPU memory is shared across platforms using each platform's native graphics stack mechanisms.
When no direct sharing path is available, the extension falls back to `glReadPixels` CPU readback. This introduces a round-trip transfer overhead per frame, but ensures it works reliably in any environment.

| Platform | Sharing path | Status |
| --- | --- | --- |
| Windows / D3D12 | ANGLE D3D11 shared texture (NT handle) → `ID3D12Resource` | Verified |
| Windows / Vulkan | ANGLE D3D11 shared texture (NT handle) → `VkImage` | Verified |
| Android / Compatibility | `AHardwareBuffer` → `EGLImage` → `ExternalTexture` | Verified |
| macOS / Metal | IOSurface → `MTLTexture` | Verified |
| Linux / Vulkan | `VkImage` → opaque fd → `GL_EXT_memory_object` | Verified (llvmpipe) |
| Android / Forward+ · Mobile | `VkImage` → opaque fd → `GL_EXT_memory_object` | Verified |
| macOS / Vulkan (MoltenVK) | IOSurface → `VkImage` | Verified |

Call `ServoWebView.get_backend_name()` to check which path is currently in use.

### Project settings required for Windows and macOS

A Vulkan device's extensions are fixed when the device is created, long before any GDExtension is loaded.
When memory sharing requires extensions that Godot does not request by default, they must be requested via project settings. This requires [godotengine/godot#114940](https://github.com/godotengine/godot/pull/114940), which adds the `rendering/rendering_device/vulkan/additional_device_extensions` project setting and `RenderingDevice.get_device_enabled_extensions()` to query the extensions actually enabled on the device.

```ini
[rendering]

rendering_device/vulkan/additional_device_extensions=PackedStringArray("VK_KHR_external_memory_win32", "VK_EXT_metal_objects")
```

| Platform | Device extension | In stock Godot |
| --- | --- | --- |
| Windows | `VK_KHR_external_memory_win32` | Disabled (requires setting) |
| macOS (MoltenVK) | `VK_EXT_metal_objects` | Disabled (requires setting) |
| Linux / Android | `VK_KHR_external_memory_fd` | **Enabled by default** |

Linux and Android do not require additional configuration because of the handle type used for sharing: an opaque fd (file descriptor).
This is the only external memory handle that stock Godot enables out of the box (Godot registers `VK_KHR_external_memory_fd` for an unrelated reason: to suppress noisy validation layer warnings on some platforms). Building both sharing paths on top of opaque fd allows GPU memory sharing to work on an unpatched, unconfigured stock Godot engine. By contrast, using dma-buf or `AHardwareBuffer` directly would require extensions that Godot does not register.

Only paths that require additional configuration probe for the configuration methods at startup. If unavailable, the Vulkan renderer logs the reason and falls back to CPU readback without failing to start. If available, it inspects the enabled extensions to select the sharing path.
Furthermore, every path verifies function pointer resolution with the device itself: an extension might be listed even if its entry points fail to resolve. A listed name is merely a declaration, whereas a resolved function pointer is what guarantees functionality.

### Renderer settings and behavior

- **Windows**: Both Vulkan and D3D12 renderers support texture sharing. Vulkan is Godot's default and requires the project setting mentioned above. Setting `rendering/rendering_device/driver.windows` to `d3d12` requires nothing beyond Godot 4.4, so the included demo project defaults to `d3d12` to run out of the box on stock engines.
- **Android**: Works on all three renderers via two distinct paths:
  - **Compatibility (GLES3)**: Shares an `AHardwareBuffer` and receives it as an `ExternalTexture`. The shader requires `samplerExternalOES` (see `needs_external_sampler()` below).
  - **Forward+ / Mobile**: Shares a `VkImage` via opaque fd instead, arriving as a standard `sampler2D` texture.
  *Note*: The `ExternalTexture` path is exclusive to Compatibility because `texture_external_initialize()` is currently an empty stub in Godot's `RenderingDevice` backends.
- **macOS**: Defaults to Metal and requires no extra project settings. The Vulkan path is provided for projects running on MoltenVK.

## Requirements

- **Godot 4.4 or later**: `RenderingDevice.texture_create_from_extension()` and `get_driver_resource()` were exposed to GDExtension in 4.4.
- **Rust 1.94 or later** (when building from source).
- **For Android builds**: A Linux or macOS host, **cargo-ndk**, and the Android NDK.

## Repository layout

The repository root is itself a Godot project. Simply clone it and open it in Godot to run.

```
godot_servo.gdextension          GDExtension manifest (at project root)
addons/godot_servo/
  servo_texture_rect.gd          Displays web pages on a TextureRect and forwards input
  servo_panel_3d.gd              Does the same on a 3D QuadMesh panel
  local_pages.gd                 Converts res:// bundled pages to file:// URLs
  select_picker.gd               PopupMenu that handles <select> elements on the page
  cursors.gd                     Maps CSS cursor names to Godot cursor shapes
  servo_external.gdshader        samplerExternalOES shader for Android GLES3 (Compatibility)
  servo_external_canvas.gdshader Control (2D Canvas) variant of the above shader
  bin/                           Build artifacts (not committed)
    windows/godot_servo.x86_64.dll
    windows/libEGL.dll           ANGLE (dynamically loaded at runtime)
    windows/libGLESv2.dll
    android/arm64-v8a/libgodot_servo.so
demo/                            Demo scenes and web pages
project.godot
scripts/build.ps1 | build.sh     Build and staging scripts for bin/
src/                             GDExtension source code (Rust)
```

A release archive contains the complete `addons/godot_servo/` folder, with prebuilt binaries in `bin/` and `godot_servo.gdextension` inside. Copy this folder directly into your project's `addons/` directory.
In this repository, the manifest is located at the root only because the repository root serves as the demo project. The `res://` paths in the manifest are absolute, so it resolves identically whether placed at the root or under `addons/godot_servo/`. However, keep only **one copy** of the manifest to prevent duplicate extension registration errors.

## Build instructions

```sh
scripts/build.ps1                 # Windows: debug build and staging
scripts/build.ps1 -Release        # Windows: release build
./scripts/build.sh                # Linux / macOS: debug build
./scripts/build.sh --release      # Linux / macOS: release build
./scripts/build.sh --android      # Android (arm64-v8a): requires cargo-ndk
```

Running `cargo build` alone does not stage files into the Godot addon directory. Always use the provided build scripts.
In addition to copying the compiled library to `addons/godot_servo/bin/`, the script extracts the `libEGL.dll` and `libGLESv2.dll` produced by the `mozangle` crate from its `OUT_DIR` and stages them. Surfman loads ANGLE by filename at runtime, so both DLLs must reside in the same folder as the extension library (`src/angle_loader.rs` preloads them by absolute path).

### Android builds

Cross-compile from Linux or macOS (including WSL). Windows hosts cannot build for Android because no host toolchain simultaneously satisfies both of Servo's C dependencies (jemalloc's `configure` rejects the MSVC host triple, and glsl-optimizer fails to compile under MinGW).

Set `ANDROID_NDK_HOME` to your NDK path, then run:

```sh
export ANDROID_NDK_HOME=~/android/android-ndk-r27c
./scripts/build.sh --release --android
```

*Note*: The build script creates a stub `libgcc.a` containing `INPUT(-lunwind)` in `target/` and adds it to the linker search path. This addresses an issue where NDK r23 replaced `libgcc` with `libunwind`, but some upstream dependencies still request `-lgcc` from the linker.

## Running the demo

```sh
# Set GODOT environment variable to your Godot binary path
export GODOT=~/.local/godot/4.7.2-stable/Godot_v4.7.2-stable_win64_console.exe

scripts/build.ps1 -Run                 # 3D in-game browser demo
scripts/build.ps1 -Run -Scene flat     # 2D flat panel (for isolating issues)
scripts/build.ps1 -Test                # Input and signal self-check
```

The self-check (`-Test`) runs through an end-to-end suite of extension features and reports the results:

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

### Building and installing an Android APK

```sh
./scripts/build.sh --release --android

godot --headless --path . --export-debug Android godot-servo.apk
adb install -r godot-servo.apk
```

Release builds are stripped via `strip = true` in `[profile.release]`, bringing the arm64-v8a library to ~119 MB and the final APK to ~146 MB. Debug builds weigh in at ~1474 MB and cannot reasonably fit in an APK, so Android is only built in release mode (most of the binary size comes from SpiderMonkey, Stylo, WebRender, and ICU data).

## Usage

The quickest way to get started is with the addon's built-in components:
- **2D UI**: Attach `servo_texture_rect.gd` to a `TextureRect`.
- **3D Panel**: Attach `servo_panel_3d.gd` to a `MeshInstance3D` that has a `QuadMesh` and a child `CollisionObject3D`.

After attaching the script, set its `browser` property in the inspector to point to your `ServoWebView` node. These components automatically handle texture updates, coordinate transformations, input event forwarding, cursor shapes, and IME anchor positioning.
Your project only needs to handle two things:
1. Setting the URL to open
2. Handling events received from the page

See `demo/main.tscn` (3D) and `demo/flat.tscn` (2D) for complete working examples.

If you prefer to write custom control logic, here is what the components do internally:

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

# Forward input (local_position is in WebView pixel coordinates)
browser.feed_input(event, local_position)

# Receive events from the web page
browser.bridge_event.connect(func(name: String, payload: String) -> void:
    print(name, " ", payload)
)
```

There are two platform-specific texture considerations (both demo scenes handle them):

- `is_texture_flipped_v()`: Returns `true` on the macOS IOSurface path. Because this path has no intermediate blit step to flip OpenGL's bottom-left origin, flip the V coordinate on the material.
- `needs_external_sampler()`: Returns `true` on Android with the Compatibility renderer. The buffer arrives as a `GL_TEXTURE_EXTERNAL_OES` texture. Standard `sampler2D` samplers will read it as black, so the material must use a shader declaring `samplerExternalOES`. A minimal shader is included at `addons/godot_servo/servo_external.gdshader`.

### Forwarding input

`feed_input(event, position)` accepts mouse, touch, and keyboard events.
`position` must be in WebView local pixel coordinates, so convert it first:

- **For a `TextureRect`**: Subtract the control's position and scale by `view_size / rect.size`.
- **For a 3D panel**: Take the intersection position from `CollisionObject3D.input_event`, convert it to UV coordinates, and multiply by `view_size` (see `demo/main.gd` for bidirectional conversion logic).

You can safely pass both mouse and touch events through together. Godot's `input_devices/pointing/emulate_mouse_from_touch` is enabled by default and generates synthetic mouse events from touch, but `feed_input()` automatically discards events where `device == DEVICE_ID_EMULATION`, preventing duplicate actions. The reverse `emulate_touch_from_mouse` is filtered out identically.

Touch input is forwarded to Servo as native touch events. The page receives standard `touchstart`, `touchmove`, and `touchend` events, and scrolling/flinging physics are handled by Servo's built-in touch handlers.

### Japanese and IME input

When an editable text element receives focus on the page, Servo notifies the extension, which enables the OS IME and emits `ime_requested(caret, multiline)`. The `caret` argument is a `Rect2` in WebView pixel coordinates.

Because the OS renders the IME candidate window in window coordinates, the extension cannot know where a 3D panel appears on screen. Project the caret position to screen space and assign it to `ime_anchor`:

```gdscript
browser.ime_requested.connect(func(caret: Rect2, _multiline: bool) -> void:
    var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
    browser.ime_anchor = camera.unproject_position(view_pixels_to_world(bottom_left))
)
```

Both demo scenes implement this projection. Without it, the candidate window defaults to the top-left of the window.

To drive text composition from custom in-game UI (e.g. a virtual keyboard) instead of the OS IME, call `feed_ime_composition(state, text)` with `"start"`, `"update"`, or `"end"`. The text passed to `"end"` is committed.
Alternatively, `feed_ime_preedit(text)` follows the OS IME pipeline: pass the preedit string, then an empty string, and send committed characters as key events via `feed_input()`.

> [!NOTE]
> **Known limitation**: Canceling an active composition leaves the preedit text in the input field. This occurs because Servo's `compositionend` handler only clears selection when data is empty, and Servo's Composition API does not currently offer a mechanism to clear preedit text.

### Responding to page dialogs and selection menus

JavaScript `alert()`, `confirm()`, `prompt()`, and HTML `<select>` elements block the page's JavaScript execution until the host environment responds.
Because the extension does not provide its own dialog UI, these requests are emitted as Godot signals, allowing the game to display whatever UI it chooses and return the response.
`<select>` elements work the same way: Servo does not draw dropdown menus itself, but instead emits the list of options. Clicking a `<select>` will appear to do nothing until the game displays a menu. A lightweight `PopupMenu` component is included at `addons/godot_servo/select_picker.gd` to handle this.

```gdscript
browser.dialog_confirm.connect(func(message: String) -> void:
    var accepted: bool = await my_dialog.ask(message)
    browser.respond_to_dialog(accepted, "")
)

browser.select_element_requested.connect(func(options: Array, multiple: bool) -> void:
    # options: [{ id, label, disabled, group }, ...] (<optgroup> items are flattened)
    var chosen: int = await my_menu.pick(options)
    browser.respond_to_select([chosen])
)
```

**Always respond to dialog requests**. If a dialog request is ignored, the page's JavaScript execution will remain blocked indefinitely. If the user closes the UI without making a choice, call `cancel_pending_dialog()` (you can check whether a dialog is awaiting response using `has_pending_dialog()`).
Also, because dialog strings originate from untrusted web pages, present them with appropriate styling so players cannot confuse them with genuine game UI.

Note: File pickers, color pickers, and context menus are not surfaced as signals. Servo automatically receives a default cancellation response, and page execution continues uninterrupted.

### Sending events from the page to Godot

The extension automatically injects `window.godot` into every loaded page:

```js
godot.emit("buy", { item: "potion", price: 120 });   // payload arrives as a JSON string
```

You can also send events using standard HTML links without writing JavaScript:

```html
<a href="godot:buy?item=potion">Buy</a>              <!-- payload arrives as a query string -->
```

- `godot.emit()` outputs a tagged `console.log` internally, which is intercepted by the extension's `show_console_message` hook. Because it triggers no page navigation, it preserves page state completely.
- The link format intercepts navigation to the `godot:` custom scheme in `request_navigation` and cancels the navigation.

### API reference

| Category / Methods & Properties | Description |
| --- | --- |
| `url`, `view_size`, `autostart`, `ime_anchor` | Exported properties (configurable in Inspector) |
| `start()`, `stop()`, `is_running()` | Lifecycle management |
| `get_texture()`, `is_texture_flipped_v()`, `needs_external_sampler()`, `get_backend_name()` | Texture retrieval, display configuration, backend info |
| `load_url()`, `reload()`, `go_back()`, `go_forward()` | Navigation |
| `evaluate_javascript(code) -> int` | Executes JavaScript (result emitted exactly once via `script_result(id, value, error)`) |
| `feed_input(event, position)`, `notify_pointer_left()` | Input event forwarding, pointer leave notification |
| `feed_ime_composition(state, text)`, `feed_ime_preedit(text)`, `cancel_ime_composition()` | IME text composition control |
| `respond_to_dialog(accepted, text)`, `respond_to_select(ids)`, `cancel_pending_dialog()`, `has_pending_dialog()` | Dialog and picker response handling |
| `set_view_size_px(size)` | Viewport resolution setting |

**Signals**:
`frame_updated`, `title_changed`, `url_changed`, `load_started`, `load_finished`, `cursor_changed`, `console_message`, `bridge_event`, `script_result`, `ime_requested`, `ime_dismissed`, `crashed`, `dialog_alert`, `dialog_confirm`, `dialog_prompt`, `select_element_requested`

### ServoServer

`ServoServer` is an engine singleton that manages the single Servo instance per process.
Servo can only be initialized once per process, so it is created when the first `ServoWebView` starts and persists until the game exits. Individual `ServoWebView` nodes can be created and freed with scenes, while the underlying Servo instance remains alive.

| Property | Description |
| --- | --- |
| `enable_webgl2` | Enable WebGL 2.0 (enabled by default) |
| `enable_webgpu` | Enable WebGPU (enabled by default; see [WebGPU](#webgpu)) |

These settings are read once when Servo is initialized. Changing them afterward has no effect and produces a warning.
Configure them before the first `ServoWebView` starts: in an autoload script's `_init()`, or in any scene node's `_ready()` when the WebView node uses `autostart` (since `autostart` starts the WebView after the entire scene is ready).

```gdscript
# Autoload script example
func _init() -> void:
    ServoServer.enable_webgl2 = false
```

## WebGL

WebGL output renders directly onto the same shared GPU texture. Test pages are located in `demo/web/`.

| Feature | Status |
| --- | --- |
| WebGL 1.0 | Works |
| WebGL 2.0 | Works (requires `enable_webgl2`) |
| three.js r128 | Works |
| three.js 0.180 (latest) | Works (via WebGL 2.0) |

![WebGL 2.0 rendering three.js 0.180](shot_three.png)

Servo disables WebGL 2 by default, so `ServoServer` provides `enable_webgl2` to toggle the `dom_webgl2_enabled` preference. It defaults to enabled because modern three.js releases require WebGL 2.

```sh
scripts/build.ps1 -Run -Page webgl          # Plain WebGL sample
scripts/build.ps1 -Run -Page three-legacy   # three.js r128 sample

# Pages using ES Modules must be served via HTTP (local server)
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/three.html
```

The `-Page` flag opens `res://demo/web/<name>.html`. A string starting with `http` is treated as a direct URL.

## WebGPU

WebGPU output also renders onto the same shared texture as WebGL.
`demo/web/webgpu.html` renders to a canvas, while `demo/web/webgpu-compute.html` executes a compute shader without a canvas.

| Platform / Renderer | Status |
| --- | --- |
| Windows / D3D12 | Works in release builds |
| Windows / Vulkan | Works in release builds |
| macOS / Metal | Works |
| macOS / Vulkan (MoltenVK) | Compute works (canvas rendering unverified) |
| Linux / Vulkan | Works |
| Android | No adapter available (unsupported) |

`ServoServer.enable_webgpu` toggles the `dom_webgpu_enabled` preference (default: enabled).
Enabling Cargo's `webgpu` feature links wgpu and naga into the binary regardless of whether a page uses them.

**Android cannot acquire a WebGPU adapter.**
`servo-webgpu` unconditionally configures its wgpu instance with `STRICT_WEBGPU_COMPLIANCE`. This flag requires every adapter to satisfy all items in `DownlevelFlags::compliant()`. One requirement is `SURFACE_VIEW_FORMATS`, which wgpu derives from `VK_KHR_swapchain_mutable_format` and explicitly documents as unsupported on Android. Consequently, all adapters are discarded and JavaScript's `requestAdapter()` returns `null`.
This flag pertains to presenting swapchain images through alternate view formats, a feature unnecessary for this integration since Servo renders offscreen. Other extension features are unaffected: on the same device, the demo runs through `android-ahardwarebuffer`, WebGL pages render normally, and self-checks pass.

**`file://` pages cannot use WebGPU.**
`Constellation::handle_wgpu_request` retrieves the page host from `registered_domain_name`. Because `file://` pages have an opaque origin and no host, Servo discards the request without responding. `navigator.gpu` remains present, but the Promise returned by `requestAdapter()` neither resolves nor rejects, hanging indefinitely. Always serve WebGPU pages over HTTP:

```sh
( cd demo/web && python -m http.server 8731 --bind 127.0.0.1 & )
scripts/build.ps1 -Run -Page http://127.0.0.1:8731/webgpu.html
```

**Servo 0.5.0 does not stop its WebGPU thread.**
`Constellation::handle_shutdown` looks for WebGPU communication channels in the browsing context group. However, closing the last WebView destroys that group first. As a result, the `WGPU` thread and its poller thread continue running after Servo shuts down.
Godot unloads GDExtensions near the end of `Main::cleanup()`, which leaves the poller running unmapped code, causing an access violation (`0xC0000005`) on process exit in Windows debug builds.
This extension uses `src/module_pin.rs` to pin the library in process memory and prevent unloading, avoiding the crash. (Because threads leak across all build types, this pin is always active; you can disable it with `GODOT_SERVO_NO_PIN=1` if you need to observe library unloading).

**Windows debug builds with WebGPU cause Godot to lose its GPU device.**
Servo initializes wgpu with `InstanceFlags::from_build_config()`, which enables GPU validation whenever `debug_assertions` is active. This causes wgpu's DX12 backend to enable the D3D12 debug layer, which invalidates all previously created D3D12 devices.
Under the D3D12 renderer, Godot's own device is invalidated and the next allocation fails with `0x887a0005`. Under the Vulkan renderer, Godot's Vulkan device is lost (`VK_ERROR_DEVICE_LOST`) as soon as the DX12 backend is enumerated.
Release builds do not enable validation and are unaffected. When testing WebGPU on Windows, always use release builds (`-Release`).

## Design notes

### Single buffering

Instead of using a swapchain, the extension allocates and maintains a single offscreen surfman surface. This keeps the RID of the texture handed to Godot valid for the entire lifetime of the WebView, allowing projects to bind the texture once without rebinding.
The trade-off is the potential for Godot to sample the surface while Servo is drawing into it. Strict execution ordering prevents this race condition: within a single frame, `paint()`, blit, `glFlush()`, and Godot's own rendering run sequentially on the main thread.

### Synchronization

Godot's `RenderingDevice` API does not provide a mechanism to bind external semaphores to command submissions (`submit()` and `sync()` are local to the internal device). GPU execution ordering therefore relies on `glFlush()`. Equivalent interop libraries like `wgpu-native-texture-interop` face the same limitation, noting that explicit semaphores are "not yet handled by any built-in synchronizer".

This synchronization gap is not purely theoretical, but is mitigated by the current display model: panels are composited once per frame from the same main thread that ordered the flush. In compositors that resample textures asynchronously on independent schedules, this assumption breaks down.
A prime example is Android XR (VR/XR), where fast head rotation causes visible texture tearing without GPU-to-GPU synchronization. In such cases, submitting a command buffer with an exportable `SYNC_FD` semaphore is necessary. The current architecture can accommodate this extension: `VK_KHR_external_semaphore_fd` can be requested through the same `additional_device_extensions` project setting already used for texture import.

### OpenGL context management

Servo maintains its own OpenGL context and makes it current whenever drawing. When Godot renders via Vulkan, D3D12, or Metal, no other system on the main thread uses an OpenGL context.
Android's Compatibility renderer is the exception: Godot renders using its own EGL context on the same thread. Therefore, `src/gl_guard.rs` captures the active context before handing control to Servo, restoring it afterward. The Linux and Android Vulkan paths also invoke GL APIs when creating or freeing shared textures, but they always explicitly make Servo's context current before doing so.

The same rule applies to any Godot-side operation that issues GL calls. For example, `ExternalTexture` calls `glEGLImageTargetTexture2DOES` when its buffer ID is assigned, so the Android bridge restores Godot's host context before creating it.

### Why the RenderingDevice path performs an internal copy

`Texture2DRD` calls `texture_create_shared()` internally, but Godot's `RenderingDevice` drivers (both D3D12 and Vulkan) cannot directly display textures imported from external extensions:
- **D3D12 driver**: Outright rejects textures that lack an internal memory allocation, rendering completely white.
- **Vulkan driver**: Accepts the texture due to a `|| created_from_extension` exemption, but samples completely black.

The root cause is identical in both drivers: `texture_create_from_extension()` creates view and tracker entries over an external image without taking ownership of the image or its underlying memory, and no queue family ownership transfer occurs. Godot's layout tracker has no knowledge of the external image's true layout state; direct sampling therefore reads from a layout Godot never established.
A texture copy, however, is tracked by Godot on both ends, allowing proper layout transitions and consistent reads within Godot's resource tracking system.

Both paths therefore copy the imported texture into a Godot-owned texture using `RenderingDevice.texture_copy()` and display the copy. Because the copy occurs entirely on the GPU, there is no CPU round-trip overhead. (Metal is the exception: it bypasses `Texture2DRD` entirely and passes the texture directly, so no copy is needed).

*Note*: The main `RenderingDevice` records copies into Godot's frame command buffer rather than executing them immediately, meaning the copy executes slightly after the call. However, the destination texture is sampled by Godot's own rendering pipeline within the same frame graph, so no explicit wait is needed. Synchronization against Servo's writes remains governed by `glFlush()`, as described above.

### Zero-allocation CPU fallback

The CPU readback path maintains two pixel buffers and two `Image` objects, alternating between them each frame.
Because Godot's `PackedByteArray` is copy-on-write (CoW), modifying a buffer currently referenced by an `Image` would trigger a memory duplication. Writing to the alternate buffer keeps the reference count at 1, allowing `glReadPixels` to output directly into what becomes the texture data without intermediate copies. Two buffers suffice because even in a multi-threaded `RenderingServer`, queued `texture_2d_update` calls are consumed within one frame.

At 1280×720 in release builds, this approach takes 1.34 ms per frame update. Allocating a new buffer every frame would take 1.93 ms, incurring ~7 MB of allocation and an extra full-frame copy each frame.

### Matching GPU adapters between Servo and Godot

GPU texture sharing requires both Servo and Godot to run on the same physical GPU.
Godot selects its device based on type score or the `--gpu-index` command-line argument. Servo's device is chosen according to surfman's internal rules (the first non-Intel adapter on Windows, or the PRIME adapter `DRI_PRIME=1` on Mesa). On multi-GPU systems, these two choices can diverge, causing texture import to fail and falling back to CPU readback.

On Windows, the extension ensures both systems match. `src/gpu_adapter.rs` queries Godot for the LUID of the active adapter (`ID3D12Device::GetAdapterLuid` under D3D12, `VkPhysicalDeviceIDProperties::deviceLUID` under Vulkan), locates the matching device in DXGI's adapter list, and passes it to surfman. Setting `--gpu-index` to a secondary GPU therefore preserves the fast `d3d12-shared-nt-handle` path instead of falling back to CPU readback.

Linux and Android cannot be steered this way because surfman's EGL backend only allows choosing between hardware, low-power, and software categories, with no API to select a GPU by name or ID. Instead, the extension detects and reports mismatches: before allocating resources, the opaque fd path compares Godot's Vulkan `deviceUUID` with OpenGL's `GL_DEVICE_UUID_EXT`. If they differ, it logs the GPU names in use and safely falls back to CPU readback rather than crashing inside the driver. (If UUIDs cannot be queried—such as on GL without `glGetUnsignedBytevEXT` or Vulkan < 1.1—the check is skipped).

### Why jemalloc is rebuilt on Linux

Servo pulls in jemalloc via `servo-allocator`, which defaults to the initial-exec TLS (Thread Local Storage) model. This sets the `STATIC_TLS` flag on the shared object (`.so`) and pushes `PT_TLS` beyond glibc's static TLS surplus, preventing Godot from loading the extension via `dlopen`.
`Cargo.toml` therefore declares a direct dependency on `tikv-jemalloc-sys` on Linux with the `disable_initial_exec_tls` feature enabled, which resolves this limitation.

## Not supported / Limitations

- **Game background blur (scene color feedback)**: Blurring the game behind the page via CSS `backdrop-filter`. While Godot can provide the screen texture via `CompositorEffect`, Servo requires a custom fork adding `WebRenderImageHandlerType`.
- **iOS**: Unsupported. Neither surfman nor Servo targets iOS, and iOS forbids JIT compilation and dynamic `dlopen`.
- **File picker, color picker, and context menu**: While supported internally by Servo, the extension does not expose them as signals (default cancel actions are returned).
- **Multiple `ServoWebView` nodes simultaneously**: While architecturally designed to share a single `Servo` instance, running multiple simultaneous views has not been thoroughly tested.

## Related projects

Two other addons embed Servo into Godot. Both render using `SoftwareRenderingContext` and `read_to_image()`, which is equivalent to the CPU readback fallback used by this project when GPU sharing is unavailable.

| Project | Rendering path | License |
| --- | --- | --- |
| [Decapitated/Godot-Servo](https://github.com/Decapitated/Godot-Servo) | CPU readback | LGPL-3.0 |
| [emanuelbertey/web-servo-godot](https://github.com/emanuelbertey/web-servo-godot) | CPU readback | None stated |
| **godot-servo (this project)** | **GPU shared texture** + CPU fallback | MIT / Apache-2.0 |

If GPU-accelerated texture sharing is not required, `web-servo-godot` exposes a broader surface of Servo's embedding API (history, focus, favicon, fullscreen, permissions/authentication, and file/color/context-menu pickers). Note that it does not specify a license, meaning all rights are reserved by default.

## Contributing

Commit messages must follow the [Conventional Commits](https://www.conventionalcommits.org/) specification, as release notes are generated automatically from them. See [CONTRIBUTING.md](CONTRIBUTING.md) for allowed types, scopes, and pre-push validation commands.

## License

Dual-licensed under either the [Apache License 2.0](LICENSE-APACHE) or [MIT License](LICENSE-MIT) at your option.

- **Servo**: MPL-2.0. This crate depends on Servo without modifying its source code, so file-level copyleft provisions do not extend to your project code.
- **Vendored libraries**: The three.js builds under `demo/web/vendor/` are licensed under the MIT License and retain their original license headers.
