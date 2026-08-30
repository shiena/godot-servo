extends Control
## The 2D scene, for checking things.
##
## With no 3D panel in the way the coordinate maths is simple, so this is the
## place to start when isolating whether an image appears at all or whether input
## gets through.

const WebAssets = preload("res://demo/web_assets.gd")

const VIEW_SIZE := Vector2i(1280, 720)

var browser: ServoWebView
var view: TextureRect
var status: Label
var texture_bound := false


func _ready() -> void:
	var root := VBoxContainer.new()
	root.set_anchors_preset(Control.PRESET_FULL_RECT)
	add_child(root)

	status = Label.new()
	status.text = "Starting..."
	root.add_child(status)

	view = TextureRect.new()
	view.custom_minimum_size = Vector2(VIEW_SIZE)
	view.stretch_mode = TextureRect.STRETCH_SCALE
	view.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	view.size_flags_vertical = Control.SIZE_EXPAND_FILL
	# Let the TextureRect itself receive input.
	view.mouse_filter = Control.MOUSE_FILTER_STOP
	view.gui_input.connect(_on_view_input)
	view.mouse_exited.connect(_on_view_exited)
	root.add_child(view)

	browser = ServoWebView.new()
	browser.view_size = VIEW_SIZE
	browser.url = _local_page_url()
	add_child(browser)

	browser.frame_updated.connect(_on_frame_updated)
	browser.bridge_event.connect(_on_bridge_event)
	browser.title_changed.connect(func(title: String) -> void: status.text = title)
	browser.ime_requested.connect(_on_ime_requested)
	browser.dialog_alert.connect(func(message: String) -> void:
		_note("alert: %s" % message)
		browser.respond_to_dialog(true, ""))
	browser.dialog_confirm.connect(func(message: String) -> void:
		_note("confirm: %s" % message)
		browser.respond_to_dialog(true, ""))
	browser.dialog_prompt.connect(func(message: String, default_value: String) -> void:
		_note("prompt: %s" % message)
		browser.respond_to_dialog(true, default_value))
	browser.select_element_requested.connect(func(options: Array, _multiple: bool) -> void:
		_note("select: %d options" % options.size())
		if options.is_empty():
			browser.cancel_pending_dialog()
		else:
			browser.respond_to_select([options[-1]["id"]]))
	browser.crashed.connect(func(reason: String) -> void:
		push_error("godot-servo: page crashed: %s" % reason))


func _local_page_url() -> String:
	return WebAssets.page_url("index")


## Converts the TextureRect's local coordinates to WebView pixels and forwards them.
## Mouse and touch both go through; the extension drops the duplicate synthetic events.
func _on_view_input(event: InputEvent) -> void:
	if browser == null:
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

	var scale := Vector2(VIEW_SIZE) / view.size
	browser.feed_input(event, local * scale)


func _on_view_exited() -> void:
	if browser != null:
		browser.notify_pointer_left()


func _unhandled_input(event: InputEvent) -> void:
	if browser != null and event is InputEventKey:
		browser.feed_input(event, Vector2.ZERO)


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


## Where to put the candidate window: undo the TextureRect's scaling to reach
## screen coordinates.
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	var scale := view.size / Vector2(VIEW_SIZE)
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = view.global_position + bottom_left * scale


func _on_bridge_event(name: String, payload: String) -> void:
	status.text = "%s %s" % [name, payload]
	print("godot-servo: bridge_event ", name, " ", payload)


## Shows a notification from the page on the status line.
func _note(text: String) -> void:
	if status != null:
		status.text = text
	print("godot-servo: ", text)
