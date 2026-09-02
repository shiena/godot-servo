extends Node3D
## The in-game browser demo.
##
## Puts what Servo rendered on a panel in 3D space and forwards clicks, scrolling
## and keystrokes straight to the WebView. Events coming back from the page
## arrive on ServoWebView's bridge_event signal.
##
## The panel, the camera, the HUD and the WebView node all sit in main.tscn, and
## the signals are wired there too. What is left here is only what a scene file
## cannot hold: the URL, which is built at runtime, and the handlers.

const WebAssets = preload("res://demo/web_assets.gd")

@onready var browser: ServoWebView = $Browser
@onready var screen: MeshInstance3D = $Screen
@onready var camera: Camera3D = $Camera3D
@onready var hud: RichTextLabel = $Hud/Status
@onready var select_picker: PopupMenu = $SelectPicker
@onready var material: StandardMaterial3D = screen.material_override as StandardMaterial3D

## The panel's physical size in metres and the WebView resolution in pixels, both
## read back from the scene so the conversion below cannot drift away from it.
@onready var panel_size: Vector2 = (screen.mesh as QuadMesh).size
@onready var view_size := Vector2(browser.view_size)

var external_material: ShaderMaterial

## The WebView pixel position the pointer last pointed at.
## Remembered because key events need a position too.
var last_point := Vector2.ZERO
var texture_bound := false


func _ready() -> void:
	# The node starts with autostart off, so the URL can be set first.
	browser.url = _local_page_url()
	browser.start()


## Decides which page to open. `-- --page webgl` switches it.
func _local_page_url() -> String:
	var page := "index"
	var args := OS.get_cmdline_user_args()
	var index := args.find("--page")
	if index >= 0 and index + 1 < args.size():
		page = args[index + 1]
	return WebAssets.page_url(page)


# ── Forwarding input ─────────────────────────────────────────────────────

## Converts mouse and touch input on the panel to WebView pixels and forwards it.
func _on_panel_input(
	_camera: Node, event: InputEvent, hit: Vector3, _normal: Vector3, _shape: int
) -> void:
	last_point = _world_to_view_pixels(hit)

	# Touch goes through as well; the extension drops the duplicate mouse events.
	if event is InputEventMouseMotion or event is InputEventMouseButton \
			or event is InputEventScreenTouch or event is InputEventScreenDrag:
		browser.feed_input(event, last_point)


func _on_panel_exited() -> void:
	browser.notify_pointer_left()


## Turns a hit position on the panel, in world space, into WebView pixels.
func _world_to_view_pixels(world_position: Vector3) -> Vector2:
	var local := screen.global_transform.affine_inverse() * world_position
	# A QuadMesh is centred on its origin. Flip Y to put (0, 0) at the top left.
	var u := local.x / panel_size.x + 0.5
	var v := 0.5 - local.y / panel_size.y
	return Vector2(u * view_size.x, v * view_size.y)


func _unhandled_input(event: InputEvent) -> void:
	# Key events need no position, but pass the last one to keep the API uniform.
	if event is InputEventKey:
		browser.feed_input(event, last_point)


# ── Notifications from Servo ─────────────────────────────────────────────

func _on_frame_updated() -> void:
	if texture_bound:
		return
	# The texture stays the same object for the whole run, so bind it once.
	var texture: Texture2D = browser.get_texture()
	if texture == null:
		return
	if browser.needs_external_sampler():
		# Android's shared buffer arrives as a GL_TEXTURE_EXTERNAL_OES, which a
		# `sampler2D` cannot read. Swap in a shader that uses `samplerExternalOES`.
		# It is loaded here rather than kept in the scene because desktop drivers
		# reject `samplerExternalOES` outright.
		external_material = ShaderMaterial.new()
		external_material.shader = load("res://demo/servo_external.gdshader")
		external_material.set_shader_parameter("servo_texture", texture)
		screen.material_override = external_material
		texture_bound = true
		print("godot-servo: texture path = ", browser.get_backend_name(), " (external sampler)")
		return

	material.albedo_texture = texture
	if browser.is_texture_flipped_v():
		# Only the macOS IOSurface path arrives upside down. Undo it in the material.
		material.uv1_scale = Vector3(1.0, -1.0, 1.0)
		material.uv1_offset = Vector3(0.0, 1.0, 0.0)
	texture_bound = true
	print("godot-servo: texture path = ", browser.get_backend_name())


func _on_title_changed(title: String) -> void:
	_refresh_hud(title, "")


func _on_url_changed(url: String) -> void:
	_refresh_hud("", url)


func _on_load_finished() -> void:
	print("godot-servo: page load finished")


var _title := ""
var _url := ""
var _last_event := "(nothing yet)"


## A file:// URL would show a local absolute path, so show just the file name.
func _short_url(url: String) -> String:
	if url.begins_with("file://"):
		return url.get_file()
	return url


func _refresh_hud(title: String, url: String) -> void:
	if title != "":
		_title = title
	if url != "":
		_url = url
	hud.text = "[b]%s[/b]  —  %s\npath: %s\nlast event: %s" % [
		_title,
		_short_url(_url),
		browser.get_backend_name(),
		_last_event,
	]


## Where the page's godot.emit() and <a href="godot:..."> arrive.
func _on_bridge_event(event_name: String, payload: String) -> void:
	_last_event = "%s %s" % [event_name, payload]
	_refresh_hud("", "")
	print("godot-servo: bridge_event ", event_name, " ", payload)

	match event_name:
		"buy":
			var data: Variant = JSON.parse_string(payload)
			if data is Dictionary:
				print("  bought: ", data.get("item", "?"), " / ", data.get("price", 0), "G")
		"rename":
			var data: Variant = JSON.parse_string(payload)
			if data is Dictionary:
				print("  name: ", data.get("name", ""))
		"close":
			print("  close requested (", payload, ")")


func _on_console_message(level: String, message: String) -> void:
	print("godot-servo: [", level, "] ", message)


# ── UI requests from the page ────────────────────────────────────────────
#
# alert(), confirm(), prompt() and <select> all block the page's JavaScript until
# they are answered. A real game would raise its own dialog here and call
# respond_to_dialog() or respond_to_select() when it closes. The demo shows the
# dialogs on the HUD and answers them immediately; <select> gets a real menu,
# because there is nothing else on screen to pick an option with.

func _on_crashed(reason: String) -> void:
	_last_event = "page crashed: %s" % reason
	_refresh_hud("", "")
	push_error("godot-servo: page crashed: %s" % reason)


func _on_dialog_alert(message: String) -> void:
	_last_event = "alert: %s" % message
	_refresh_hud("", "")
	browser.respond_to_dialog(true, "")


func _on_dialog_confirm(message: String) -> void:
	_last_event = "confirm: %s → OK" % message
	_refresh_hud("", "")
	browser.respond_to_dialog(true, "")


func _on_dialog_prompt(message: String, default_value: String) -> void:
	_last_event = "prompt: %s → '%s'" % [message, default_value]
	_refresh_hud("", "")
	browser.respond_to_dialog(true, default_value)


## `options` is an array of `{ id, label, disabled, group }` dictionaries.
##
## Servo draws no dropdown of its own, so the menu is a Godot PopupMenu. It goes
## where the pointer last was, which is the `<select>` the player just clicked:
## back through the panel into world space, then onto the screen.
func _on_select_element_requested(options: Array, allow_multiple: bool) -> void:
	_last_event = "select: %d options -> picker open" % options.size()
	_refresh_hud("", "")
	var at := camera.unproject_position(_view_pixels_to_world(last_point))
	select_picker.open(browser, options, allow_multiple, at)


# ── IME ───────────────────────────────────────────────────────────────────

## Called when a text field in the page takes focus.
##
## The OS places the candidate window in window coordinates, so the caret
## position on the panel has to be projected to screen coordinates and handed
## over. Without that, candidates appear at the top left of the window.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	# The caret's bottom-left corner is where the candidate window should go.
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = camera.unproject_position(_view_pixels_to_world(bottom_left))
	_last_event = "IME on (composed input works)"
	_refresh_hud("", "")


func _on_ime_dismissed() -> void:
	_last_event = "IME off"
	_refresh_hud("", "")


## The inverse of `_world_to_view_pixels()`.
func _view_pixels_to_world(point: Vector2) -> Vector3:
	var u := point.x / view_size.x
	var v := point.y / view_size.y
	var local := Vector3(
		(u - 0.5) * panel_size.x,
		(0.5 - v) * panel_size.y,
		0.0)
	return screen.global_transform * local


# ── Checking that it works ───────────────────────────────────────────────

## Started with `--screenshot <path>`, waits a moment, writes the screen out and
## quits. There to confirm by eye that the GPU sharing path really produces an image.
var _screenshot_path := ""
var _frames := 0


func _process(_delta: float) -> void:
	if _screenshot_path == "":
		return
	_frames += 1
	# Give the page time to load and produce its first frame.
	if _frames == 180:
		await RenderingServer.frame_post_draw
		var image := get_viewport().get_texture().get_image()
		image.save_png(_screenshot_path)
		print("godot-servo: wrote ", _screenshot_path)
		get_tree().quit()


func _init() -> void:
	var args := OS.get_cmdline_user_args()
	var index := args.find("--screenshot")
	if index >= 0 and index + 1 < args.size():
		_screenshot_path = args[index + 1]
