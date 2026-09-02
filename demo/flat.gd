extends Control
## The 2D scene, for checking things.
##
## With no 3D panel in the way the coordinate maths is simple, so this is the
## place to start when isolating whether an image appears at all or whether input
## gets through.
##
## The TextureRect, the status line and the WebView node all sit in flat.tscn,
## along with the signal wiring. Only the URL and the handlers are left here.

const WebAssets = preload("res://demo/web_assets.gd")
const Cursors = preload("res://demo/cursors.gd")

@onready var browser: ServoWebView = $Browser
@onready var view: TextureRect = $Layout/View
@onready var status: Label = $Layout/Status
@onready var select_picker: PopupMenu = $SelectPicker
@onready var view_size := Vector2(browser.view_size)

## Frames to wait after the last resize before telling Servo. Dragging a window
## edge fires `resized` every frame, and each one reallocates Servo's surface.
const RESIZE_SETTLE_FRAMES := 10

var texture_bound := false
var _resize_countdown := 0

## Where the pointer last was, in the TextureRect's coordinates. The `<select>`
## menu opens there.
var last_point := Vector2.ZERO


func _ready() -> void:
	# The node starts with autostart off, so the URL can be set first.
	browser.url = WebAssets.page_url("index")
	browser.start()


## Converts the TextureRect's local coordinates to WebView pixels and forwards them.
## Mouse and touch both go through; the extension drops the duplicate synthetic events.
func _on_view_input(event: InputEvent) -> void:
	# The TextureRect takes focus when clicked, so key events only arrive here
	# while the page holds the keyboard; anywhere else they stay with the game.
	# Accepting them stops Tab from moving the focus on.
	if event is InputEventKey:
		browser.feed_input(event, Vector2.ZERO)
		view.accept_event()
		return

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
	browser.feed_input(event, local * (view_size / view.size))


func _on_view_exited() -> void:
	browser.notify_pointer_left()


## The panel is a window onto the page, so the page reflows with it rather than
## being scaled up. Servo is told once the dragging stops.
func _on_view_resized() -> void:
	_resize_countdown = RESIZE_SETTLE_FRAMES


func _process(_delta: float) -> void:
	if _resize_countdown == 0:
		return
	_resize_countdown -= 1
	if _resize_countdown > 0 or view.size.x < 1.0 or view.size.y < 1.0:
		return
	view_size = view.size
	browser.set_view_size_px(Vector2i(view_size))
	# Resizing rebuilds Servo's surface, and with it the texture, so the one bound
	# to the TextureRect is gone. Ask for the new one on the next frame.
	texture_bound = false


func _on_frame_updated() -> void:
	if texture_bound:
		return
	var texture: Texture2D = browser.get_texture()
	if texture == null:
		return
	view.texture = texture
	view.flip_v = browser.is_texture_flipped_v()
	texture_bound = true
	status.text = "path: %s" % browser.get_backend_name()


func _on_title_changed(title: String) -> void:
	status.text = title


## The page asked for a different mouse cursor: a link, a text field, a resizer.
## The TextureRect is hovered whenever the page is, so its own shape is enough.
func _on_cursor_changed(shape: String) -> void:
	view.mouse_default_cursor_shape = Cursors.godot_shape(shape) as Control.CursorShape


## Where to put the candidate window: undo the TextureRect's scaling to reach
## screen coordinates.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = view.global_position + bottom_left * (view.size / view_size)


func _on_bridge_event(event_name: String, payload: String) -> void:
	status.text = "%s %s" % [event_name, payload]
	print("godot-servo: bridge_event ", event_name, " ", payload)


# ── UI requests from the page ────────────────────────────────────────────
#
# The page's JavaScript is blocked until each of these is answered. The demo
# answers the dialogs straight away and shows what they said on the status line;
# `<select>` gets a real menu, because Servo draws no dropdown of its own.

func _on_dialog_alert(message: String) -> void:
	_note("alert: %s" % message)
	browser.respond_to_dialog(true, "")


func _on_dialog_confirm(message: String) -> void:
	_note("confirm: %s" % message)
	browser.respond_to_dialog(true, "")


func _on_dialog_prompt(message: String, default_value: String) -> void:
	_note("prompt: %s" % message)
	browser.respond_to_dialog(true, default_value)


func _on_select_element_requested(options: Array, allow_multiple: bool) -> void:
	_note("select: %d options" % options.size())
	select_picker.open(browser, options, allow_multiple, view.global_position + last_point)


func _on_crashed(reason: String) -> void:
	push_error("godot-servo: page crashed: %s" % reason)


## Shows a notification from the page on the status line.
func _note(text: String) -> void:
	status.text = text
	print("godot-servo: ", text)
