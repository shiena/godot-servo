extends PopupMenu
## The dropdown for a page's `<select>`.
##
## Servo does not draw the dropdown itself. It hands the option list to the
## embedder and blocks the page's JavaScript until an answer arrives, which is
## what `select_element_requested` carries. This turns that list into a Godot
## PopupMenu and answers the WebView with whatever the player picked.
##
## Every path out of the menu answers: picking an option, confirming a
## multiple-choice list, and closing it with Escape or a click outside.

## `_ids` holds one entry per menu item. Separators get this, and the confirm
## item of a multiple-choice list gets `_CONFIRM`; everything else holds the
## option `id` to hand back.
const _SEPARATOR := -1
const _CONFIRM := -2

var _browser: ServoWebView
var _ids: Array[int] = []
var _multiple := false
var _answered := true


func _init() -> void:
	# A multiple-choice list stays open until it is confirmed.
	hide_on_checkable_item_selection = false
	index_pressed.connect(_on_index_pressed)
	popup_hide.connect(_on_popup_hide)


## Opens the menu for one `select_element_requested`.
##
## `options` is the array the signal handed over: `{ id, label, disabled, group }`
## dictionaries. `at` is where to put the menu, in this viewport's coordinates.
func open(browser: ServoWebView, options: Array, allow_multiple: bool, at: Vector2) -> void:
	if visible:
		hide()

	_browser = browser
	_multiple = allow_multiple
	_answered = false
	_ids.clear()
	clear()

	if options.is_empty():
		_answered = true
		browser.cancel_pending_dialog()
		return

	# `<optgroup>`s arrive flattened, with the group's name on every option, so
	# start a section whenever the name changes.
	var group := ""
	for option in options:
		var option_group: String = option.get("group", "")
		if option_group != group:
			group = option_group
			if group != "":
				add_separator(group)
				_ids.append(_SEPARATOR)
		var index := item_count
		if allow_multiple:
			# Servo does not say which options are selected already, so the list
			# starts empty and answers with exactly what was ticked.
			add_check_item(option["label"])
		else:
			add_item(option["label"])
		set_item_disabled(index, option.get("disabled", false))
		_ids.append(option["id"])

	if allow_multiple:
		add_separator()
		_ids.append(_SEPARATOR)
		add_item("OK")
		_ids.append(_CONFIRM)

	reset_size()
	popup_on_parent(Rect2i(Vector2i(at), size))


func _on_index_pressed(index: int) -> void:
	var id := _ids[index]
	if id == _SEPARATOR:
		return

	if not _multiple:
		var picked: Array[int] = [id]
		_answer(picked)
		return

	if id == _CONFIRM:
		var chosen: Array[int] = []
		for i in item_count:
			if is_item_checkable(i) and is_item_checked(i):
				chosen.append(_ids[i])
		_answer(chosen)
		return

	set_item_checked(index, not is_item_checked(index))


## Escape, a click outside, or a multiple-choice list closed without OK.
##
## PopupMenu hides *before* it emits `index_pressed`, so at this point a pick may
## still be on its way and cancelling here would withdraw the request out from
## under it. Deferring puts the check after the signals of this frame.
func _on_popup_hide() -> void:
	_cancel_unless_answered.call_deferred()


## The page is still waiting, so withdrawing the request is what lets it carry on.
func _cancel_unless_answered() -> void:
	if _answered:
		return
	_answered = true
	if _browser != null:
		_browser.cancel_pending_dialog()


func _answer(ids: Array[int]) -> void:
	_answered = true
	if _browser != null:
		_browser.respond_to_select(ids)
