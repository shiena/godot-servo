extends Control
## The 2D scene, for checking things.
##
## With no 3D panel in the way the coordinate maths is simple, so this is the
## place to start when isolating whether an image appears at all or whether input
## gets through.
##
## The TextureRect carries the addon's `servo_texture_rect.gd`, which owns the
## texture, the input forwarding, the resize and the IME anchor. What is left
## here is what the addon cannot decide for a game: which page to open, and what
## to do with what the page says back.

const WebAssets = preload("res://demo/web_assets.gd")

@onready var browser: ServoWebView = $Browser
@onready var view: TextureRect = $Layout/View
@onready var status: Label = $Layout/Status
@onready var select_picker: PopupMenu = $SelectPicker


func _ready() -> void:
	# The node starts with autostart off, so the URL can be set first.
	browser.url = WebAssets.page_url("index")
	browser.start()


func _on_load_finished() -> void:
	status.text = "path: %s" % browser.get_backend_name()


func _on_title_changed(title: String) -> void:
	status.text = title


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


## The menu opens where the pointer last was, which is the `<select>` the player
## just clicked. The TextureRect is what remembers that.
func _on_select_element_requested(options: Array, allow_multiple: bool) -> void:
	_note("select: %d options" % options.size())
	select_picker.open(browser, options, allow_multiple, view.global_position + view.last_point)


func _on_crashed(reason: String) -> void:
	push_error("godot-servo: page crashed: %s" % reason)


## Shows a notification from the page on the status line.
func _note(text: String) -> void:
	status.text = text
	print("godot-servo: ", text)
