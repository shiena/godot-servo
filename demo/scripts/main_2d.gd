extends Control
## 2D の確認用シーン。
##
## 3D の板を挟まないぶん座標変換が単純なので、まず絵が出るか / 入力が通るかを
## 切り分けたいときはこちらを使う。

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
	status.text = "起動中…"
	root.add_child(status)

	view = TextureRect.new()
	view.custom_minimum_size = Vector2(VIEW_SIZE)
	view.stretch_mode = TextureRect.STRETCH_SCALE
	view.expand_mode = TextureRect.EXPAND_IGNORE_SIZE
	view.size_flags_vertical = Control.SIZE_EXPAND_FILL
	# TextureRect 自身が入力を受け取れるようにする。
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


func _local_page_url() -> String:
	var absolute := ProjectSettings.globalize_path("res://web/index.html")
	return "file:///" + absolute.replace("\\", "/").trim_prefix("/")


## TextureRect のローカル座標を WebView のピクセル座標に直して渡す。
func _on_view_input(event: InputEvent) -> void:
	if browser == null:
		return
	if not (event is InputEventMouse):
		return

	var local: Vector2 = (event as InputEventMouse).position
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
	status.text = "経路: %s" % browser.get_backend_name()


func _on_bridge_event(name: String, payload: String) -> void:
	status.text = "%s %s" % [name, payload]
	print("godot-servo: bridge_event ", name, " ", payload)
