# godot_servo addon

The GDExtension that embeds [Servo](https://servo.org/), the Rust browser engine, and hands the
rendered page to Godot as a GPU texture. It registers a `ServoWebView` node by itself, so there is
no editor plugin and nothing to enable in the Plugins tab.

What it does, the full API, and the per-platform notes are in the repository README:
<https://github.com/shiena/godot-servo#readme>

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
