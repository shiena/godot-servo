extends MeshInstance3D
## Shows a `ServoWebView` on a panel standing in 3D space, and drives it.
##
## Attach this script to a `MeshInstance3D` whose mesh is a `QuadMesh`, give it a
## `CollisionObject3D` child with a shape covering the same area, and point
## `browser` at the WebView node. The collision body is what turns a click in the
## world into a position on the page; without one nothing reaches it.
##
## No `class_name`; see `local_pages.gd` for why.
##
## What it takes over, all of which is otherwise the game's to write:
##
## - Binding the texture, and whichever of `is_texture_flipped_v()` and
##   `needs_external_sampler()` the platform calls for. Getting either wrong
##   shows up as a wrong picture rather than as an error.
## - The maths both ways between a hit in the world and a pixel on the page.
## - Which of the game and the page holds the keyboard.
## - The cursor the page asks for. A panel is not a Control, so the shape is the
##   whole window's and has to be put up and taken down by hand.
## - Where the IME candidate window goes, which needs the caret projected out of
##   the world and onto the screen.
##
## What stays with the game: the URL, `bridge_event`, and answering the page's
## dialogs and `<select>`. `select_picker.gd` covers the last of those, and
## `view_pixels_to_screen()` here says where to open it.

const Cursors = preload("res://addons/godot_servo/cursors.gd")
const EXTERNAL_SHADER := "res://addons/godot_servo/servo_external.gdshader"

## Emitted when the page takes the keyboard, and again when it gives it back.
signal focus_changed(page_has_keyboard: bool)

## The WebView to show. Without it this script does nothing.
@export var browser: ServoWebView

## The camera the panel is looked at through. Projecting a point on the page onto
## the screen needs it; the IME anchor and `view_pixels_to_screen()` are both
## inert without one.
@export var camera: Camera3D

## Whether a click on the panel hands the page the keyboard, and a click
## anywhere else takes it back. Off leaves every key event with the game.
@export var take_keyboard_on_click := true

## Whether the page holds the keyboard.
var focused := false

## Where the pointer last pointed, in WebView pixels. Key events need a position
## too, and a `<select>` menu opens here.
var last_point := Vector2.ZERO

var _bound: Texture2D
var _page_cursor: int = Input.CURSOR_ARROW
## Set by the panel's own handler when a click reaches it. See `_resolve_click()`.
var _took_click := false


func _ready() -> void:
	if browser == null:
		push_error("godot-servo: the ServoPanel3D at %s has no browser." % get_path())
		return
	if not (mesh is QuadMesh):
		push_warning(
			"godot-servo: the ServoPanel3D at %s is not a QuadMesh, so positions "
			% get_path()
			+ "on the page will be wrong."
		)

	var clickable := _collision_object()
	if clickable == null:
		push_warning(
			"godot-servo: the ServoPanel3D at %s has no CollisionObject3D child, "
			% get_path()
			+ "so no input can reach the page."
		)
	else:
		clickable.input_event.connect(_on_panel_input)
		clickable.mouse_exited.connect(_on_panel_exited)

	browser.frame_updated.connect(_on_frame_updated)
	browser.cursor_changed.connect(_on_cursor_changed)
	browser.ime_requested.connect(_on_ime_requested)


func _collision_object() -> CollisionObject3D:
	for child in get_children():
		if child is CollisionObject3D:
			return child
	return null


# ── Coordinates ──────────────────────────────────────────────────────────

## Turns a hit position on the panel, in world space, into WebView pixels.
func world_to_view_pixels(world_position: Vector3) -> Vector2:
	var local := global_transform.affine_inverse() * world_position
	var panel := _panel_size()
	# A QuadMesh is centred on its origin. Flip Y to put (0, 0) at the top left.
	var u := local.x / panel.x + 0.5
	var v := 0.5 - local.y / panel.y
	return Vector2(u, v) * Vector2(browser.view_size)


## The inverse of `world_to_view_pixels()`.
func view_pixels_to_world(point: Vector2) -> Vector3:
	var panel := _panel_size()
	var view := Vector2(browser.view_size)
	var local := Vector3(
		(point.x / view.x - 0.5) * panel.x, (0.5 - point.y / view.y) * panel.y, 0.0
	)
	return global_transform * local


## A point in WebView pixels, projected into this viewport's coordinates.
##
## Where anything drawn over the page belongs: a `<select>` menu, a tooltip, the
## IME candidate window. Needs `camera`.
func view_pixels_to_screen(point: Vector2) -> Vector2:
	if camera == null:
		return Vector2.ZERO
	return camera.unproject_position(view_pixels_to_world(point))


func _panel_size() -> Vector2:
	var quad := mesh as QuadMesh
	return quad.size if quad != null else Vector2.ONE


# ── Input ────────────────────────────────────────────────────────────────

func _on_panel_input(
	_camera: Node, event: InputEvent, hit: Vector3, _normal: Vector3, _shape: int
) -> void:
	last_point = world_to_view_pixels(hit)
	Input.set_default_cursor_shape(_page_cursor as Input.CursorShape)

	# Reaching this handler means the click landed on the panel, so this is what
	# hands the page the keyboard.
	if take_keyboard_on_click and event is InputEventMouseButton and event.is_pressed():
		_took_click = true
		_set_focused(true)

	# Touch goes through as well; the extension drops the duplicate mouse events
	# Godot's emulate_mouse_from_touch produces, so one gesture stays one gesture.
	if (
		event is InputEventMouseMotion
		or event is InputEventMouseButton
		or event is InputEventScreenTouch
		or event is InputEventScreenDrag
	):
		browser.feed_input(event, last_point)


func _on_panel_exited() -> void:
	browser.notify_pointer_left()
	Input.set_default_cursor_shape(Input.CURSOR_ARROW)


func _unhandled_input(event: InputEvent) -> void:
	if not take_keyboard_on_click:
		return

	# A click that misses the panel takes the keyboard back. Whether it missed is
	# only known once the physics picking has run, which happens either side of
	# this, so the answer is settled at the end of the frame instead.
	if event is InputEventMouseButton and event.is_pressed():
		_resolve_click.call_deferred()
		return

	# Key events need no position, but pass the last one to keep the API uniform.
	if focused and event is InputEventKey:
		browser.feed_input(event, last_point)


func _resolve_click() -> void:
	if _took_click:
		_took_click = false
		return
	_set_focused(false)


func _set_focused(value: bool) -> void:
	if focused == value:
		return
	focused = value
	focus_changed.emit(value)


## The page asked for a different cursor: a link, a text field, a resizer.
##
## The panel is not a Control, so the shape is the whole window's. It is put up
## while the pointer is on the panel and taken down on the way out. Setting it
## also injects a mouse motion, which is why the page's choice is remembered here
## rather than read back from Godot.
func _on_cursor_changed(shape: String) -> void:
	_page_cursor = Cursors.godot_shape(shape)
	Input.set_default_cursor_shape(_page_cursor as Input.CursorShape)


## The OS places the candidate window in window coordinates, and there is no
## correspondence between a position on the panel and one on screen, so the caret
## has to be projected. Without this, candidates appear at the top left.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = view_pixels_to_screen(bottom_left)


# ── The texture ──────────────────────────────────────────────────────────

func _on_frame_updated() -> void:
	var current := browser.get_texture()
	if current == null or current == _bound:
		return
	_bound = current

	if browser.needs_external_sampler():
		# Android's Compatibility renderer hands the buffer over as a
		# GL_TEXTURE_EXTERNAL_OES texture, which a `sampler2D` reads as black.
		# Loaded here rather than kept in the scene, because desktop drivers
		# reject `samplerExternalOES` outright.
		var shader_material := material_override as ShaderMaterial
		if shader_material == null:
			shader_material = ShaderMaterial.new()
			shader_material.shader = load(EXTERNAL_SHADER)
			material_override = shader_material
		shader_material.set_shader_parameter("servo_texture", current)
		return

	var standard := material_override as StandardMaterial3D
	if standard == null:
		standard = StandardMaterial3D.new()
		standard.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
		standard.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR
		material_override = standard
	standard.albedo_texture = current
	if browser.is_texture_flipped_v():
		# Only the macOS IOSurface path arrives upside down. Undo it here.
		standard.uv1_scale = Vector3(1.0, -1.0, 1.0)
		standard.uv1_offset = Vector3(0.0, 1.0, 0.0)
