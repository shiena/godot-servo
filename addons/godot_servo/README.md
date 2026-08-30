# godot_servo addon

The GDExtension that embeds Servo. See the [repository README](../../README.md) for what it does
and how to use it.

`bin/` holds build output and is not committed. Build once after cloning:

```sh
scripts/build.ps1     # Windows
./scripts/build.sh    # Linux and macOS
```

Release archives contain `bin/`. Drop this folder into your project alongside
`godot_servo.gdextension`:

```
your-project/
  godot_servo.gdextension
  addons/godot_servo/bin/...
```

The `.gdextension` sits at the project root rather than inside the addon because on Windows the
ANGLE DLLs (`libEGL.dll` and `libGLESv2.dll`) have to live in the same folder as the extension, and
keeping it at the root makes the relative paths in `[dependencies]` straightforward.

There is no editor plugin. The extension registers the `ServoWebView` class itself, so nothing needs
enabling in the Plugins tab.
