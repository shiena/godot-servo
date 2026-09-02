extends RefCounted
## Turns the cursor names `cursor_changed` carries into Godot cursor shapes.
##
## The name is the CSS `cursor` value Servo settled on, lowercased and stripped
## of its hyphens: `pointer`, `text`, `nwse-resize` as `nwseresize`. CSS has more
## shapes than Godot, so several names land on the same one, and anything not
## listed here — `zoom-in`, `cell`, `context-menu` — keeps the arrow.
##
## The returned value is an `Input.CursorShape`, whose members hold the same
## numbers as `Control.CursorShape`, so it also fits a Control's
## `mouse_default_cursor_shape`.

const _SHAPES := {
	"pointer": Input.CURSOR_POINTING_HAND,
	"text": Input.CURSOR_IBEAM,
	"verticaltext": Input.CURSOR_IBEAM,
	"crosshair": Input.CURSOR_CROSS,
	"wait": Input.CURSOR_WAIT,
	"progress": Input.CURSOR_BUSY,
	"help": Input.CURSOR_HELP,
	"move": Input.CURSOR_MOVE,
	"allscroll": Input.CURSOR_MOVE,
	"grab": Input.CURSOR_CAN_DROP,
	"grabbing": Input.CURSOR_DRAG,
	"copy": Input.CURSOR_CAN_DROP,
	"alias": Input.CURSOR_CAN_DROP,
	"nodrop": Input.CURSOR_FORBIDDEN,
	"notallowed": Input.CURSOR_FORBIDDEN,
	# The eight resize directions collapse onto Godot's four.
	"eresize": Input.CURSOR_HSIZE,
	"wresize": Input.CURSOR_HSIZE,
	"ewresize": Input.CURSOR_HSIZE,
	"colresize": Input.CURSOR_HSIZE,
	"nresize": Input.CURSOR_VSIZE,
	"sresize": Input.CURSOR_VSIZE,
	"nsresize": Input.CURSOR_VSIZE,
	"rowresize": Input.CURSOR_VSIZE,
	"neresize": Input.CURSOR_BDIAGSIZE,
	"swresize": Input.CURSOR_BDIAGSIZE,
	"neswresize": Input.CURSOR_BDIAGSIZE,
	"nwresize": Input.CURSOR_FDIAGSIZE,
	"seresize": Input.CURSOR_FDIAGSIZE,
	"nwseresize": Input.CURSOR_FDIAGSIZE,
}


static func godot_shape(cursor_name: String) -> int:
	return _SHAPES.get(cursor_name, Input.CURSOR_ARROW)
