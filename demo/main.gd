extends Node3D
## The in-game browser demo.
##
## Puts what Servo rendered on a panel in 3D space and forwards clicks, scrolling
## and keystrokes straight to the WebView. Events coming back from the page
## arrive on ServoWebView's bridge_event signal.

const WebAssets = preload("res://demo/web_assets.gd")

## The panel's physical size, in metres.
const PANEL_SIZE := Vector2(1.92, 1.08)
## The WebView resolution, in pixels.
const VIEW_SIZE := Vector2i(1280, 720)
## Where to put the camera: close enough that the panel roughly fills the view.
const CAMERA_Z := 1.0

var browser: ServoWebView
var screen: MeshInstance3D
var camera: Camera3D
var material: StandardMaterial3D
var external_material: ShaderMaterial
var hud: RichTextLabel

## The WebView pixel position the pointer last pointed at.
## Remembered because key events need a position too.
var last_point := Vector2.ZERO
var texture_bound := false


func _ready() -> void:
	_build_environment()
	_build_panel()
	_build_hud()
	_build_browser()


func _build_browser() -> void:
	browser = ServoWebView.new()
	browser.name = "Browser"
	browser.view_size = VIEW_SIZE
	browser.url = _local_page_url()
	add_child(browser)

	browser.frame_updated.connect(_on_frame_updated)
	browser.title_changed.connect(_on_title_changed)
	browser.url_changed.connect(_on_url_changed)
	browser.load_finished.connect(_on_load_finished)
	browser.bridge_event.connect(_on_bridge_event)
	browser.console_message.connect(_on_console_message)
	browser.ime_requested.connect(_on_ime_requested)
	browser.ime_dismissed.connect(_on_ime_dismissed)
	browser.crashed.connect(_on_crashed)
	browser.dialog_alert.connect(_on_dialog_alert)
	browser.dialog_confirm.connect(_on_dialog_confirm)
	browser.dialog_prompt.connect(_on_dialog_prompt)
	browser.select_element_requested.connect(_on_select_element_requested)


## Decides which page to open. `-- --page webgl` switches it.
func _local_page_url() -> String:
	var page := "index"
	var args := OS.get_cmdline_user_args()
	var index := args.find("--page")
	if index >= 0 and index + 1 < args.size():
		page = args[index + 1]
	return WebAssets.page_url(page)


# ── Building the scene ───────────────────────────────────────────────────

func _build_environment() -> void:
	var environment := Environment.new()
	environment.background_mode = Environment.BG_COLOR
	environment.background_color = Color(0.06, 0.07, 0.10)
	environment.ambient_light_source = Environment.AMBIENT_SOURCE_COLOR
	environment.ambient_light_color = Color(0.35, 0.38, 0.45)
	environment.ambient_light_energy = 0.6

	var world := WorldEnvironment.new()
	world.environment = environment
	add_child(world)

	var light := DirectionalLight3D.new()
	light.rotation_degrees = Vector3(-45.0, -35.0, 0.0)
	add_child(light)

	# Scatter boxes behind the panel so it reads as sitting inside a game.
	#
	# Anything directly behind the panel is hidden. These go outside the shadow the
	# panel's edges cast from the camera, into the bands left and right. Both that
	# shadow and the visible width grow in proportion to distance, so a factor
	# scaled by depth lands inside the band whatever the depth is.
	for i in 8:
		var depth := randf_range(-6.0, -2.0)
		var distance := CAMERA_Z - depth
		var side := 1.0 if i % 2 == 0 else -1.0
		var box := MeshInstance3D.new()
		box.mesh = BoxMesh.new()
		box.position = Vector3(
			side * randf_range(1.05, 1.22) * distance,
			randf_range(-0.5, 0.5) * distance,
			depth)
		box.rotation = Vector3(randf(), randf(), randf()) * TAU
		box.scale = Vector3.ONE * randf_range(0.25, 0.55) * (distance / 3.0)
		var box_material := StandardMaterial3D.new()
		box_material.albedo_color = Color(0.25, 0.30, 0.42)
		box.material_override = box_material
		add_child(box)

	var camera := Camera3D.new()
	camera.position = Vector3(0.0, 0.0, CAMERA_Z)
	add_child(camera)
	self.camera = camera


func _build_panel() -> void:
	var mesh := QuadMesh.new()
	mesh.size = PANEL_SIZE

	material = StandardMaterial3D.new()
	# This is UI, so leave it unlit.
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR
	material.albedo_color = Color.WHITE

	screen = MeshInstance3D.new()
	screen.name = "Screen"
	screen.mesh = mesh
	screen.material_override = material
	add_child(screen)

	# The click target: a thin box the same size as the panel.
	var body := StaticBody3D.new()
	body.name = "Clickable"
	var shape := CollisionShape3D.new()
	var box := BoxShape3D.new()
	box.size = Vector3(PANEL_SIZE.x, PANEL_SIZE.y, 0.02)
	shape.shape = box
	body.add_child(shape)
	screen.add_child(body)

	body.input_event.connect(_on_panel_input)
	body.mouse_exited.connect(_on_panel_exited)


func _build_hud() -> void:
	var layer := CanvasLayer.new()
	add_child(layer)

	hud = RichTextLabel.new()
	hud.bbcode_enabled = true
	hud.fit_content = true
	hud.scroll_active = false
	hud.set_anchors_preset(Control.PRESET_TOP_WIDE)
	hud.offset_left = 12.0
	hud.offset_top = 8.0
	hud.offset_right = -12.0
	hud.add_theme_color_override("default_color", Color(0.85, 0.90, 1.0))
	layer.add_child(hud)

	_set_hud("Starting...")


func _set_hud(text: String) -> void:
	if hud != null:
		hud.text = text


# ── Forwarding input ─────────────────────────────────────────────────────

## Converts mouse and touch input on the panel to WebView pixels and forwards it.
func _on_panel_input(_camera: Node, event: InputEvent, position: Vector3, _normal: Vector3, _shape: int) -> void:
	if browser == null:
		return

	last_point = _world_to_view_pixels(position)

	# Touch goes through as well; the extension drops the duplicate mouse events.
	if event is InputEventMouseMotion or event is InputEventMouseButton \
			or event is InputEventScreenTouch or event is InputEventScreenDrag:
		browser.feed_input(event, last_point)


func _on_panel_exited() -> void:
	if browser != null:
		browser.notify_pointer_left()


## Turns a hit position on the panel, in world space, into WebView pixels.
func _world_to_view_pixels(world_position: Vector3) -> Vector2:
	var local := screen.global_transform.affine_inverse() * world_position
	# A QuadMesh is centred on its origin. Flip Y to put (0, 0) at the top left.
	var u := local.x / PANEL_SIZE.x + 0.5
	var v := 0.5 - local.y / PANEL_SIZE.y
	return Vector2(u * float(VIEW_SIZE.x), v * float(VIEW_SIZE.y))


func _unhandled_input(event: InputEvent) -> void:
	if browser == null:
		return

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
	_set_hud("[b]%s[/b]  —  %s\npath: %s\nlast event: %s" % [
		_title,
		_short_url(_url),
		browser.get_backend_name() if browser != null else "?",
		_last_event,
	])


## Where the page's godot.emit() and <a href="godot:..."> arrive.
func _on_bridge_event(name: String, payload: String) -> void:
	_last_event = "%s %s" % [name, payload]
	_refresh_hud("", "")
	print("godot-servo: bridge_event ", name, " ", payload)

	match name:
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
# content on the HUD and answers immediately.

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
func _on_select_element_requested(options: Array, _allow_multiple: bool) -> void:
	var labels := PackedStringArray()
	for option in options:
		labels.append(option["label"])
	_last_event = "select: %s -> picking the last one" % ", ".join(labels)
	_refresh_hud("", "")
	if options.is_empty():
		browser.cancel_pending_dialog()
	else:
		browser.respond_to_select([options[-1]["id"]])


# ── IME ───────────────────────────────────────────────────────────────────

## Called when a text field in the page takes focus.
##
## The OS places the candidate window in window coordinates, so the caret
## position on the panel has to be projected to screen coordinates and handed
## over. Without that, candidates appear at the top left of the window.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	if camera == null:
		return
	# The caret's bottom-left corner is where the candidate window should go.
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	var world := _view_pixels_to_world(bottom_left)
	browser.ime_anchor = camera.unproject_position(world)
	_last_event = "IME on (composed input works)"
	_refresh_hud("", "")


func _on_ime_dismissed() -> void:
	_last_event = "IME off"
	_refresh_hud("", "")


## The inverse of `_world_to_view_pixels()`.
func _view_pixels_to_world(point: Vector2) -> Vector3:
	var u := point.x / float(VIEW_SIZE.x)
	var v := point.y / float(VIEW_SIZE.y)
	var local := Vector3(
		(u - 0.5) * PANEL_SIZE.x,
		(0.5 - v) * PANEL_SIZE.y,
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
