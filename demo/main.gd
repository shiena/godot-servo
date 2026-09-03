extends Node3D
## The in-game browser demo.
##
## Puts what Servo rendered on a panel in 3D space. The panel carries the addon's
## `servo_panel_3d.gd`, which owns the texture, the coordinate maths both ways,
## the input forwarding, the keyboard focus, the cursor and the IME anchor.
##
## What is left here is what a scene file cannot hold and the addon cannot decide
## for a game: the URL, which is built at runtime, and what to do with what the
## page says back.

const WebAssets = preload("res://demo/web_assets.gd")

@onready var browser: ServoWebView = $Browser
@onready var screen: MeshInstance3D = $Screen
@onready var hud: RichTextLabel = $Hud/Status
@onready var select_picker: PopupMenu = $SelectPicker


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


# ── Notifications from Servo ─────────────────────────────────────────────

func _on_title_changed(title: String) -> void:
	_refresh_hud(title, "")


func _on_url_changed(url: String) -> void:
	_refresh_hud("", url)


func _on_load_finished() -> void:
	print("godot-servo: page load finished")
	print("godot-servo: texture path = ", browser.get_backend_name())
	_refresh_hud("", "")


## The panel took the keyboard, or gave it back.
func _on_screen_focus_changed(_page_has_keyboard: bool) -> void:
	_refresh_hud("", "")


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
	hud.text = (
		"[b]%s[/b]  —  %s\npath: %s\nlast event: %s"
		% [_title, _short_url(_url), browser.get_backend_name(), _last_event]
	)


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
## where the pointer last was, which is the `<select>` the player just clicked;
## the panel is what projects that back onto the screen.
func _on_select_element_requested(options: Array, allow_multiple: bool) -> void:
	_last_event = "select: %d options -> picker open" % options.size()
	_refresh_hud("", "")
	var at: Vector2 = screen.view_pixels_to_screen(screen.last_point)
	select_picker.open(browser, options, allow_multiple, at)


func _on_ime_requested(_caret: Rect2, _multiline: bool) -> void:
	_last_event = "IME on (composed input works)"
	_refresh_hud("", "")


func _on_ime_dismissed() -> void:
	_last_event = "IME off"
	_refresh_hud("", "")


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
