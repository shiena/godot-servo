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
	browser.ime_requested.connect(_on_ime_requested)


func _local_page_url() -> String:
	return WebAssets.page_url("index")


## TextureRect のローカル座標を WebView のピクセル座標に直して渡す。
## マウスとタッチの両方を通す。重複する疑似イベントは拡張側で落としている。
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
	status.text = "経路: %s" % browser.get_backend_name()


## 候補ウィンドウの位置。TextureRect の拡大率を戻して画面座標にする。
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	var scale := view.size / Vector2(VIEW_SIZE)
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	browser.ime_anchor = view.global_position + bottom_left * scale


func _on_bridge_event(name: String, payload: String) -> void:
	status.text = "%s %s" % [name, payload]
	print("godot-servo: bridge_event ", name, " ", payload)
