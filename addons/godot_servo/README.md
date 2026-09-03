# godot_servo addon

The GDExtension that embeds [Servo](https://servo.org/), the Rust browser engine, and hands the
rendered page to Godot as a GPU texture. It registers a `ServoWebView` node by itself, so there is
no editor plugin and nothing to enable in the Plugins tab.

What it does, the full API, and the per-platform notes are in the repository README:
<https://github.com/shiena/godot-servo#readme>

## Helpers

Scripts ship alongside the library for the parts a project cannot do without and should
not have to write twice. None uses `class_name`: the global class table is written by the
editor and is missing on a checkout that has only run `--import`, and a script naming an
absent global class fails to parse. Reach them with `preload()` and a path.

Two of them do most of the work. Attach one to the node the page is displayed on, point
`browser` at the `ServoWebView`, and they take over binding the texture, converting
pointer positions, forwarding input, the cursor, and the IME anchor:

| | |
| --- | --- |
| `servo_texture_rect.gd` | On a `TextureRect`. Also follows the control's size, telling Servo once a drag has settled so the page reflows. |
| `servo_panel_3d.gd` | On a `MeshInstance3D` with a `QuadMesh` and a `CollisionObject3D` child. Also arbitrates which of the game and the page holds the keyboard, and projects page positions onto the screen. |

Neither answers the page for you. The URL, `bridge_event`, and the dialogs stay with the
game, and the rest are there for that:

| | |
| --- | --- |
| `local_pages.gd` | A `file://` URL for a page bundled under `res://`. After an export those files live inside the PCK with nothing behind them, so this copies the tree out to `user://` first. |
| `select_picker.gd` | A `PopupMenu` that answers a page's `<select>`. Servo draws no dropdown of its own. |
| `cursors.gd` | Turns the CSS cursor names `cursor_changed` carries into Godot cursor shapes. Both components above use it already. |
| `servo_external.gdshader` | Declares `samplerExternalOES`, which the Android Compatibility path needs to read its texture at all. `servo_external_canvas.gdshader` is the `canvas_item` counterpart. |

The demo scenes are the worked example: `demo/main.tscn` for the panel, `demo/flat.tscn`
for the control.

## Install

Merge this folder into your project so that it lands at `addons/godot_servo/`:

```
your-project/
  addons/godot_servo/
    godot_servo.gdextension
    bin/...
```

Keep exactly one copy of `godot_servo.gdextension`; two manifests register the extension twice. The
`res://` paths inside it are absolute, so it resolves the libraries the same wherever the file sits.
In this repository it is at the project root instead, because the repository root *is* the demo
project and that keeps it next to `project.godot`.

## Build from source

`bin/` holds build output and is not committed. A release archive has it filled in already; a clone
needs one build first:

```sh
scripts/build.ps1     # Windows
./scripts/build.sh    # Linux and macOS
```

## License

Dual-licensed under Apache 2.0 and MIT, at your option. The full texts ship with the addon as
`LICENSE-APACHE` and `LICENSE-MIT`.
