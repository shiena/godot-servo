extends Node3D
## in-game ブラウザのデモ。
##
## 3D 空間に置いた板に Servo の描画結果を貼り、クリック・スクロール・キー入力を
## そのまま WebView に転送する。ページ側から飛んでくるイベントは
## ServoWebView の bridge_event シグナルで受け取る。

const WebAssets = preload("res://demo/web_assets.gd")

## 板の物理サイズ (メートル)。
const PANEL_SIZE := Vector2(1.92, 1.08)
## WebView の解像度 (ピクセル)。
const VIEW_SIZE := Vector2i(1280, 720)

var browser: ServoWebView
var screen: MeshInstance3D
var camera: Camera3D
var material: StandardMaterial3D
var external_material: ShaderMaterial
var hud: RichTextLabel

## 直近にポインタが指していた WebView 内のピクセル座標。
## キー入力にも座標が要るので覚えておく。
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


## 開くページを決める。`-- --page webgl` のように渡すと切り替えられる。
func _local_page_url() -> String:
	var page := "index"
	var args := OS.get_cmdline_user_args()
	var index := args.find("--page")
	if index >= 0 and index + 1 < args.size():
		page = args[index + 1]
	return WebAssets.page_url(page)


# ── シーンの組み立て ──────────────────────────────────────────────────────

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

	# 板が「ゲームの中に置かれている」ことが分かるように、後ろに適当な箱を並べる。
	for i in 7:
		var box := MeshInstance3D.new()
		box.mesh = BoxMesh.new()
		box.position = Vector3(randf_range(-4.0, 4.0), randf_range(-1.5, 1.5), randf_range(-6.0, -2.0))
		box.rotation = Vector3(randf(), randf(), randf()) * TAU
		box.scale = Vector3.ONE * randf_range(0.3, 0.9)
		var box_material := StandardMaterial3D.new()
		box_material.albedo_color = Color(0.25, 0.30, 0.42)
		box.material_override = box_material
		add_child(box)

	var camera := Camera3D.new()
	camera.position = Vector3(0.0, 0.0, 1.9)
	add_child(camera)
	self.camera = camera


func _build_panel() -> void:
	var mesh := QuadMesh.new()
	mesh.size = PANEL_SIZE

	material = StandardMaterial3D.new()
	# UI なのでライティングは効かせない。
	material.shading_mode = BaseMaterial3D.SHADING_MODE_UNSHADED
	material.texture_filter = BaseMaterial3D.TEXTURE_FILTER_LINEAR
	material.albedo_color = Color.WHITE

	screen = MeshInstance3D.new()
	screen.name = "Screen"
	screen.mesh = mesh
	screen.material_override = material
	add_child(screen)

	# クリック判定用のあたり。板と同じ大きさの薄い箱。
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

	_set_hud("起動中…")


func _set_hud(text: String) -> void:
	if hud != null:
		hud.text = text


# ── 入力の転送 ────────────────────────────────────────────────────────────

## 板の上でのマウス・タッチ操作を WebView のピクセル座標に直して渡す。
func _on_panel_input(_camera: Node, event: InputEvent, position: Vector3, _normal: Vector3, _shape: int) -> void:
	if browser == null:
		return

	last_point = _world_to_view_pixels(position)

	# タッチも一緒に渡す。疑似マウスイベントとの重複は拡張側で落としている。
	if event is InputEventMouseMotion or event is InputEventMouseButton \
			or event is InputEventScreenTouch or event is InputEventScreenDrag:
		browser.feed_input(event, last_point)


func _on_panel_exited() -> void:
	if browser != null:
		browser.notify_pointer_left()


## 板の当たり位置 (ワールド座標) を WebView のピクセル座標へ。
func _world_to_view_pixels(world_position: Vector3) -> Vector2:
	var local := screen.global_transform.affine_inverse() * world_position
	# QuadMesh は原点中心。左上を (0,0) にしたいので Y は反転する。
	var u := local.x / PANEL_SIZE.x + 0.5
	var v := 0.5 - local.y / PANEL_SIZE.y
	return Vector2(u * float(VIEW_SIZE.x), v * float(VIEW_SIZE.y))


func _unhandled_input(event: InputEvent) -> void:
	if browser == null:
		return

	# キー入力には座標が要らないが、API を揃えるため直近の位置を渡しておく。
	if event is InputEventKey:
		browser.feed_input(event, last_point)


# ── Servo からの通知 ──────────────────────────────────────────────────────

func _on_frame_updated() -> void:
	if texture_bound:
		return
	# テクスチャの実体は起動中ずっと同じなので、最初の 1 回だけ結びつければよい。
	var texture: Texture2D = browser.get_texture()
	if texture == null:
		return
	if browser.needs_external_sampler():
		# Android の共有バッファは GL_TEXTURE_EXTERNAL_OES で届く。`sampler2D` では
		# 読めないので、`samplerExternalOES` を使うシェーダに差し替える。
		external_material = ShaderMaterial.new()
		external_material.shader = load("res://demo/servo_external.gdshader")
		external_material.set_shader_parameter("servo_texture", texture)
		screen.material_override = external_material
		texture_bound = true
		print("godot-servo: texture path = ", browser.get_backend_name(), " (external sampler)")
		return

	material.albedo_texture = texture
	if browser.is_texture_flipped_v():
		# macOS の IOSurface 共有経路だけ上下が逆に届く。マテリアル側で戻す。
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
var _last_event := "(まだ届いていない)"


## file:// はローカルの絶対パスがそのまま出てしまうので、ファイル名だけにする。
func _short_url(url: String) -> String:
	if url.begins_with("file://"):
		return url.get_file()
	return url


func _refresh_hud(title: String, url: String) -> void:
	if title != "":
		_title = title
	if url != "":
		_url = url
	_set_hud("[b]%s[/b]  —  %s\n経路: %s\n直近のイベント: %s" % [
		_title,
		_short_url(_url),
		browser.get_backend_name() if browser != null else "?",
		_last_event,
	])


## ページ側の godot.emit() と <a href="godot:..."> がここに届く。
func _on_bridge_event(name: String, payload: String) -> void:
	_last_event = "%s %s" % [name, payload]
	_refresh_hud("", "")
	print("godot-servo: bridge_event ", name, " ", payload)

	match name:
		"buy":
			var data: Variant = JSON.parse_string(payload)
			if data is Dictionary:
				print("  買った: ", data.get("item", "?"), " / ", data.get("price", 0), "G")
		"rename":
			var data: Variant = JSON.parse_string(payload)
			if data is Dictionary:
				print("  名前: ", data.get("name", ""))
		"close":
			print("  閉じる要求 (", payload, ")")


func _on_console_message(level: String, message: String) -> void:
	print("godot-servo: [", level, "] ", message)


# ── IME ───────────────────────────────────────────────────────────────────

## ページ内のテキスト欄にフォーカスが入ると呼ばれる。
##
## 変換候補のウィンドウは OS がウィンドウ座標で出すので、板の中のキャレット位置を
## 画面座標へ射影して渡す。これをやらないと候補がウィンドウ左上に出る。
func _on_ime_requested(caret: Rect2, _multiline: bool) -> void:
	if camera == null:
		return
	# キャレットの左下 = 候補ウィンドウを出したい位置。
	var bottom_left := Vector2(caret.position.x, caret.position.y + caret.size.y)
	var world := _view_pixels_to_world(bottom_left)
	browser.ime_anchor = camera.unproject_position(world)
	_last_event = "IME 有効 (日本語入力できる)"
	_refresh_hud("", "")


func _on_ime_dismissed() -> void:
	_last_event = "IME 無効"
	_refresh_hud("", "")


## `_world_to_view_pixels()` の逆。
func _view_pixels_to_world(point: Vector2) -> Vector3:
	var u := point.x / float(VIEW_SIZE.x)
	var v := point.y / float(VIEW_SIZE.y)
	var local := Vector3(
		(u - 0.5) * PANEL_SIZE.x,
		(0.5 - v) * PANEL_SIZE.y,
		0.0)
	return screen.global_transform * local


# ── 動作確認用 ────────────────────────────────────────────────────────────

## `--screenshot <path>` を付けて起動すると、少し待ってから画面を書き出して終了する。
## GPU 共有経路で本当に絵が出ているかを目で確かめるための仕掛け。
var _screenshot_path := ""
var _frames := 0


func _process(_delta: float) -> void:
	if _screenshot_path == "":
		return
	_frames += 1
	# ページの読み込みと最初の描画が終わるまで少し待つ。
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
