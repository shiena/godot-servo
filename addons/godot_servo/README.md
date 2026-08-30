# godot_servo addon

The GDExtension that embeds Servo. See the [repository README](../../README.md) for what it does
and how to use it.

`bin/` holds build output and is not committed. Build once after cloning:

```sh
scripts/build.ps1     # Windows
./scripts/build.sh    # Linux and macOS
```

A release archive contains this folder complete, with `bin/` filled in and
`godot_servo.gdextension` inside it. Merge it into your project:

```
your-project/
  addons/godot_servo/
    godot_servo.gdextension
    bin/...
```

In this repository the manifest sits at the project root instead, because the repository root *is*
the demo project and that keeps it visible next to `project.godot`. Either location works — the
`res://` paths inside the file are absolute, so Godot resolves the libraries the same way. Ship only
one copy: two manifests register the extension twice.

There is no editor plugin. The extension registers the `ServoWebView` class itself, so nothing needs
enabling in the Plugins tab.
