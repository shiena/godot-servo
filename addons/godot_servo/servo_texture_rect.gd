extends TextureRect
## Shows a `ServoWebView` in a Control, and drives it.
##
## Attach this script to a `TextureRect` and point `browser` at the WebView node.
## No `class_name`; see `local_pages.gd` for why, and use `preload()` and a path
## where a script needs to name this one.
##
## What it takes over, all of which is otherwise the game's to write:
##
## - Binding the texture, once, and again after a resize has rebuilt it. Whether
##   `is_texture_flipped_v()` or `needs_external_sampler()` applies depends on
##   the platform, and getting either wrong shows up as a wrong picture rather
##   than as an error.
## - Converting pointer positions into WebView pixels, and forwarding mouse,
##   touch and key events.
## - Telling Servo about a resize once the dragging has stopped. Every resize
##   reallocates its surface, so one per frame of a drag is not an option.
## - The cursor the page asks for, and where the IME candidate window goes.
##
## What stays with the game: the URL, `bridge_event`, and answering the page's
## dialogs and `<select>`. `select_picker.gd` covers the last of those, and
## `to_screen()` here says where to open it.

const Cursors = preload("res://addons/godot_servo/cursors.gd")

## The canvas_item counterpart of the addon's spatial external-sampler shader.
const EXTERNAL_SHADER := "res://addons/godot_servo/servo_external_canvas.gdshader"

## Frames to wait after the last resize before telling Servo. Dragging a window
## edge fires `resized` every frame, and each one reallocates Servo's surface.
const RESIZE_SETTLE_FRAMES := 10

## The WebView to show. Without it this script does nothing.
@export var browser: ServoWebView

## Whether the page reflows with the control. Off keeps the WebView at its own
## `view_size` and scales the result into the rect instead.
@export var follow_size := true

## Where the pointer last was, in this control's coordinates. A `<select>` menu
## opens here, because that is the element the player just clicked.
var last_point := Vector2.ZERO

var _bound: Texture2D
var _resize_countdown := 0


func _ready() -> void:
	if browser == null:
		push_error("godot-servo: the ServoTextureRect at %s has no browser." % get_path())
		return
	browser.frame_updated.connect(_on_frame_updated)
	browser.cursor_changed.connect(_on_cursor_changed)
	browser.ime_requested.connect(_on_ime_requested)
	gui_input.connect(_on_gui_input)
	mouse_exited.connect(_on_mouse_exited)
	resized.connect(_on_resized)


# ── Coordinates ──────────────────────────────────────────────────────────

## This control's local coordinates, in WebView pixels.
func to_view_pixels(local: Vector2) -> Vector2:
	if size.x < 1.0 or size.y < 1.0:
		return Vector2.ZERO
	return local * (Vector2(browser.view_size) / size)


## A point in WebView pixels, in this viewport's coordinates.
##
## Where anything drawn over the page belongs: a `<select>` menu, a tooltip, the
## IME candidate window.
func to_screen(point: Vector2) -> Vector2:
	var view := Vector2(browser.view_size)
	if view.x < 1.0 or view.y < 1.0:
		return global_position
	return global_position + point * (size / view)


# ── Input ────────────────────────────────────────────────────────────────

func _on_gui_input(event: InputEvent) -> void:
	# The rect takes focus when clicked, so key events only arrive here while the
	# page holds the keyboard; anywhere else they stay with the game. Accepting
	# them stops Tab from moving the focus on.
	if event is InputEventKey:
		browser.feed_input(event, Vector2.ZERO)
		accept_event()
		return

	# Mouse and touch both go through. Godot's emulate_mouse_from_touch turns
	# every touch into a synthetic mouse event as well, and the extension drops
	# those, so one gesture stays one gesture.
	var local: Vector2
	if event is InputEventMouse:
		local = (event as InputEventMouse).position
	elif event is InputEventScreenTouch:
		local = (event as InputEventScreenTouch).position
	elif event is InputEventScreenDrag:
		local = (event as InputEventScreenDrag).position
	else:
		return

	last_point = local
	browser.feed_input(event, to_view_pixels(local))


func _on_mouse_exited() -> void:
	browser.notify_pointer_left()


## The page asked for a different cursor: a link, a text field, a resizer. The
## rect is hovered whenever the page is, so its own shape is enough.
func _on_cursor_changed(shape: String) -> void:
	mouse_default_cursor_shape = Cursors.godot_shape(shape) as Control.CursorShape


## The OS places the candidate window in window coordinates, so the caret has to
## be projected out of the page. Its bottom-left corner is where it belongs.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = to_screen(bottom_left)


# ── Size ─────────────────────────────────────────────────────────────────

func _on_resized() -> void:
	if follow_size:
		_resize_countdown = RESIZE_SETTLE_FRAMES


func _process(_delta: float) -> void:
	if _resize_countdown == 0:
		return
	_resize_countdown -= 1
	if _resize_countdown > 0 or size.x < 1.0 or size.y < 1.0:
		return
	browser.set_view_size_px(Vector2i(size))
	# The resize rebuilds Servo's surface and with it the texture, so the one
	# bound here is gone. Take the new one on the next frame.
	_bound = null


# ── The texture ──────────────────────────────────────────────────────────

func _on_frame_updated() -> void:
	var current := browser.get_texture()
	if current == null or current == _bound:
		return
	_bound = current
	texture = current
	flip_v = browser.is_texture_flipped_v()
	if browser.needs_external_sampler():
		_read_through_external_sampler(current)


## Draw the rect with a shader that can read the buffer at all.
##
## Android's Compatibility renderer hands it over as a GL_TEXTURE_EXTERNAL_OES
## texture, and a `sampler2D` reads that as black. The texture stays assigned so
## the control still has something to size itself by; what is drawn comes from
## the uniform instead. Loaded here rather than kept in the scene, because
## desktop drivers reject `samplerExternalOES` outright.
##
## Verified on an Adreno 710 under GLES 3.2 Compatibility.
func _read_through_external_sampler(current: Texture2D) -> void:
	var shader_material := material as ShaderMaterial
	if shader_material == null:
		shader_material = ShaderMaterial.new()
		shader_material.shader = load(EXTERNAL_SHADER)
		material = shader_material
	shader_material.set_shader_parameter("servo_texture", current)
