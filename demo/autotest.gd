extends Node
## Self check for input forwarding and signals.
##
## Asks a real page for a button's coordinates through JavaScript, sends
## synthetic mouse and touch events there, and watches for `bridge_event` coming
## back. Also checks that the wheel and a finger drag scroll the page.
##
##     Godot --path demo --quit-after 900 -- --scene autotest
##
## Results go to standard output. Exit code 0 when everything passed.

const WebAssets = preload("res://demo/web_assets.gd")

const VIEW_SIZE := Vector2i(1280, 720)
## Deadline for the whole run. A minute is plenty even on a slow CI runner.
const WATCHDOG_SECONDS := 180

var browser: ServoWebView
var results: Array[String] = []
var failures := 0

var button_point := Vector2.ZERO
var scrolled_before := -1.0
var finished := false

var dialog_message := ""
var dialog_default := ""
var select_options: Array = []
var select_multiple := false


func _ready() -> void:
	browser = ServoWebView.new()
	browser.view_size = VIEW_SIZE
	browser.url = _local_page_url()
	add_child(browser)

	browser.bridge_event.connect(_on_bridge_event)
	browser.script_result.connect(_on_script_result)
	browser.load_finished.connect(_on_load_finished)
	browser.ime_requested.connect(_on_ime_requested)
	browser.dialog_alert.connect(_on_dialog_alert)
	browser.dialog_confirm.connect(_on_dialog_confirm)
	browser.dialog_prompt.connect(_on_dialog_prompt)
	browser.select_element_requested.connect(_on_select_element_requested)

	_watchdog()
	_run()


## The overall ceiling. On a slower machine than expected, failing beats spinning
## quietly forever.
func _watchdog() -> void:
	await get_tree().create_timer(WATCHDOG_SECONDS).timeout
	if finished:
		return
	push_error("godot-servo self check: timed out after %d s" % WATCHDOG_SECONDS)
	_check("the run finished within %d s" % WATCHDOG_SECONDS, false, "timed out")
	_report()


func _local_page_url() -> String:
	return WebAssets.page_url("index")


func _run() -> void:
	# 1. Wait for the godot.emit('ready') the page sends on load.
	await _expect("bridge_event (godot.emit)", "ready", 8.0)

	# 2. JavaScript evaluation and the script_result signal.
	#    Picks up the first button's centre on the way.
	browser.evaluate_javascript("""
		(function() {
			var r = document.querySelector('button').getBoundingClientRect();
			return [r.x + r.width / 2, r.y + r.height / 2];
		})()
	""")
	await _wait_for(func() -> bool: return button_point != Vector2.ZERO, 8.0)
	_check("evaluate_javascript / script_result", button_point != Vector2.ZERO,
		"button at %s" % button_point)

	# 3. Forward a click and see the page's onclick send back a bridge_event.
	_click(button_point)
	await _expect("click -> onclick -> bridge_event", "buy", 8.0)

	# 4. Forward a tap and see onclick fire the same way a click does.
	_touch_tap(button_point)
	await _expect("touch tap -> onclick -> bridge_event", "buy", 8.0)

	# 5. Check a finger drag scrolls. It carries momentum, so wait for it to settle.
	var touch_before := await _scroll_position()
	await _touch_drag(Vector2(VIEW_SIZE) * Vector2(0.5, 0.8), Vector2(VIEW_SIZE) * Vector2(0.5, 0.3))
	await _sleep(1.5)
	var touch_after := await _scroll_position()
	_check("touch drag -> scroll", touch_after > touch_before + 1.0,
		"scrollTop %.0f -> %.0f" % [touch_before, touch_after])
	await _settle_scroll_to_top()

	# 6. Clicking a text field should make Servo ask for an IME.
	var input_point := await _element_center("#name")
	print("  -> click input at ", input_point)
	_click(input_point)
	var requested := await _wait_for(func() -> bool: return ime_caret != Rect2(), 8.0)
	_check("focus input -> ime_requested", requested, "caret %s" % ime_caret)

	# 7. Push a preedit in and check the committed text lands in the field.
	browser.feed_ime_composition("start", "に")
	await get_tree().process_frame
	browser.feed_ime_composition("update", "にほん")
	await get_tree().process_frame
	browser.feed_ime_composition("end", "日本語")
	await _sleep(0.5)
	var value := await _input_value("#name")
	_check("ime composition -> input value", value == "日本語", "value '%s'" % value)

	# 8. Reproduce the order the OS IME uses: preedit, empty, then key events for
	#    the committed text. Checks the preedit does not linger and double up.
	await _clear_input("#name")
	browser.feed_ime_preedit("にほん")
	await get_tree().process_frame
	browser.feed_ime_preedit("")
	_send_text("日本")
	await _sleep(0.5)
	var committed := await _input_value("#name")
	_check("os ime sequence -> committed once", committed == "日本",
		"value '%s'" % committed)

	# 9. alert() should arrive as a signal, and the answer should release the page.
	dialog_message = ""
	browser.evaluate_javascript("alert('hello from the page')")
	var alerted := await _wait_for(func() -> bool: return dialog_message != "", 8.0)
	_check("alert -> dialog_alert", alerted and browser.has_pending_dialog(),
		"message '%s'" % dialog_message)
	browser.respond_to_dialog(true, "")
	await _sleep(0.3)
	_check("respond_to_dialog releases the page", not browser.has_pending_dialog(), "no pending")

	# 10. The answer to confirm() should become the page's value.
	dialog_message = ""
	browser.evaluate_javascript("window.__confirmed = confirm('really?')")
	await _wait_for(func() -> bool: return dialog_message != "", 8.0)
	browser.respond_to_dialog(true, "")
	await _sleep(0.5)
	var confirmed: Variant = await _script_value("window.__confirmed")
	_check("confirm -> respond_to_dialog(true)", confirmed == true, "value %s" % confirmed)

	# 11. The text given to prompt() should land in the page.
	dialog_message = ""
	browser.evaluate_javascript("window.__prompted = prompt('name?', 'hero')")
	await _wait_for(func() -> bool: return dialog_message != "", 8.0)
	_check("prompt -> dialog_prompt", dialog_default == "hero",
		"default '%s'" % dialog_default)
	browser.respond_to_dialog(true, "godot")
	await _sleep(0.5)
	var prompted: Variant = await _script_value("window.__prompted")
	_check("prompt -> respond_to_dialog(text)", prompted == "godot", "value '%s'" % prompted)

	# 12. The <select> options should arrive and the answer should set the value.
	#     optgroups come through flattened into a single array.
	select_options = []
	var select_point := await _element_center("#job")
	_click(select_point)
	var asked := await _wait_for(func() -> bool: return not select_options.is_empty(), 8.0)
	_check("select -> select_element_requested", asked and select_options.size() == 4,
		"%d options, last group '%s'" % [
			select_options.size(),
			select_options[-1].get("group", "") if asked else "",
		])
	if asked:
		browser.respond_to_select([select_options[-1]["id"]])
		await _sleep(0.5)
		var job: Variant = await _script_value("document.querySelector('#job').value")
		_check("respond_to_select sets the value", job == "sage", "value '%s'" % job)

	# 13. Forward the wheel and check the page scrolls.
	scrolled_before = await _scroll_position()
	for i in 8:
		_wheel(Vector2(VIEW_SIZE) * 0.5, -1)
		await get_tree().process_frame
	await _sleep(1.0)
	var after := await _scroll_position()
	_check("wheel -> scroll", after > scrolled_before + 1.0,
		"scrollTop %.0f -> %.0f" % [scrolled_before, after])

	_report()


# ── Synthesising input ───────────────────────────────────────────────────

func _click(point: Vector2) -> void:
	var motion := InputEventMouseMotion.new()
	browser.feed_input(motion, point)

	var press := InputEventMouseButton.new()
	press.button_index = MOUSE_BUTTON_LEFT
	press.pressed = true
	browser.feed_input(press, point)

	var release := InputEventMouseButton.new()
	release.button_index = MOUSE_BUTTON_LEFT
	release.pressed = false
	browser.feed_input(release, point)


## One finger, touch and release.
func _touch_tap(point: Vector2) -> void:
	var press := InputEventScreenTouch.new()
	press.index = 0
	press.pressed = true
	browser.feed_input(press, point)

	var release := InputEventScreenTouch.new()
	release.index = 0
	release.pressed = false
	browser.feed_input(release, point)


## One finger, dragged from `from` to `to`.
##
## Servo's touch handler does not switch to panning until the finger has moved
## 10 px, so the intermediate positions are sent step by step. The few repeats of
## the final position before release keep momentum out of it: releasing while
## still moving starts a fling, and every later check would then depend on where
## the page happened to be when the finger came up.
func _touch_drag(from: Vector2, to: Vector2, steps: int = 10) -> void:
	var press := InputEventScreenTouch.new()
	press.index = 0
	press.pressed = true
	browser.feed_input(press, from)
	await get_tree().process_frame

	for i in range(1, steps + 1):
		var point := from.lerp(to, float(i) / float(steps))
		var drag := InputEventScreenDrag.new()
		drag.index = 0
		browser.feed_input(drag, point)
		await get_tree().process_frame

	# Let the velocity fall to zero before releasing.
	for i in 3:
		var still := InputEventScreenDrag.new()
		still.index = 0
		browser.feed_input(still, to)
		await get_tree().process_frame

	var release := InputEventScreenTouch.new()
	release.index = 0
	release.pressed = false
	browser.feed_input(release, to)


## Scroll back to the top and wait until the page has really stopped.
##
## With momentum still running, the page moves between reading an element's
## coordinates and the click arriving, and the click misses.
func _settle_scroll_to_top() -> void:
	var deadline := Time.get_ticks_msec() + 8000
	while Time.get_ticks_msec() < deadline:
		browser.evaluate_javascript("document.querySelector('main').scrollTop = 0")
		await _sleep(0.3)
		if await _scroll_position() == 0.0:
			return
	push_warning("godot-servo self check: the page kept scrolling")


## Sends committed text the way Windows does: key events carrying a unicode value.
func _send_text(text: String) -> void:
	for character in text:
		var key := InputEventKey.new()
		key.pressed = true
		key.unicode = character.unicode_at(0)
		browser.feed_input(key, Vector2.ZERO)


func _clear_input(selector: String) -> void:
	browser.evaluate_javascript("document.querySelector('%s').value = ''" % selector)
	await _sleep(0.3)


## A direction of -1 scrolls downwards.
func _wheel(point: Vector2, direction: int) -> void:
	var wheel := InputEventMouseButton.new()
	wheel.button_index = MOUSE_BUTTON_WHEEL_DOWN if direction < 0 else MOUSE_BUTTON_WHEEL_UP
	wheel.pressed = true
	wheel.factor = 1.0
	browser.feed_input(wheel, point)


# ── Plumbing for the checks ──────────────────────────────────────────────

var _last_event_name := ""
var _script_values: Dictionary = {}
var _scroll_id := -1
var _scroll_value := 0.0
var _element_id := -1
var _element_point := Vector2.ZERO
var _string_id := -1
var _string_value := ""
var _any_id := -1
var _any_value: Variant = null
var _any_ready := false


func _on_bridge_event(name: String, payload: String) -> void:
	_last_event_name = name
	print("  <- bridge_event ", name, " ", payload)


func _on_script_result(id: int, value: Variant) -> void:
	_script_values[id] = value
	if value is Array and value.size() == 2 and button_point == Vector2.ZERO:
		button_point = Vector2(value[0], value[1])
	if id == _scroll_id and value is float:
		_scroll_value = value
	if id == _element_id and value is Array and value.size() == 2:
		_element_point = Vector2(value[0], value[1])
	if id == _string_id and value is String:
		_string_value = value
	if id == _any_id:
		_any_value = value
		_any_ready = true


func _on_load_finished() -> void:
	print("  <- load finished")


var ime_caret := Rect2()


func _on_ime_requested(caret: Rect2, multiline: bool) -> void:
	ime_caret = caret
	print("  <- ime_requested ", caret, " multiline=", multiline)


func _on_dialog_alert(message: String) -> void:
	dialog_message = message
	print("  <- dialog_alert ", message)


func _on_dialog_confirm(message: String) -> void:
	dialog_message = message
	print("  <- dialog_confirm ", message)


func _on_dialog_prompt(message: String, default_value: String) -> void:
	dialog_message = message
	dialog_default = default_value
	print("  <- dialog_prompt ", message, " / ", default_value)


func _on_select_element_requested(options: Array, allow_multiple: bool) -> void:
	select_options = options
	select_multiple = allow_multiple
	print("  <- select_element_requested ", options.size(), " options")


## Evaluates an arbitrary expression and returns the result.
func _script_value(expression: String) -> Variant:
	_any_id = browser.evaluate_javascript(expression)
	_any_value = null
	_any_ready = false
	await _wait_for(func() -> bool: return _any_ready, 5.0)
	return _any_value


## Returns the centre of the element the selector names, in WebView pixels.
func _element_center(selector: String) -> Vector2:
	_element_point = Vector2.ZERO
	_element_id = browser.evaluate_javascript(
		"(function(){var r=document.querySelector('%s').getBoundingClientRect();"
		% selector + "return [r.x+r.width/2, r.y+r.height/2];})()")
	await _wait_for(func() -> bool: return _element_point != Vector2.ZERO, 5.0)
	return _element_point


func _input_value(selector: String) -> String:
	_string_value = ""
	_string_id = browser.evaluate_javascript(
		"document.querySelector('%s').value" % selector)
	await _wait_for(func() -> bool: return _string_value != "", 5.0)
	return _string_value


func _scroll_position() -> float:
	_scroll_value = -1.0
	_scroll_id = browser.evaluate_javascript("document.querySelector('main').scrollTop")
	await _wait_for(func() -> bool: return _scroll_value >= 0.0, 5.0)
	return _scroll_value


func _expect(label: String, event_name: String, timeout: float) -> void:
	_last_event_name = ""
	var ok := await _wait_for(func() -> bool: return _last_event_name == event_name, timeout)
	_check(label, ok, "expected '%s'" % event_name)


func _wait_for(predicate: Callable, timeout: float) -> bool:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000.0)
	while Time.get_ticks_msec() < deadline:
		if predicate.call():
			return true
		await get_tree().process_frame
	return false


func _sleep(seconds: float) -> void:
	await get_tree().create_timer(seconds).timeout


func _check(label: String, ok: bool, detail: String) -> void:
	if ok:
		results.append("  OK   %s  (%s)" % [label, detail])
	else:
		results.append("  FAIL %s  (%s)" % [label, detail])
		failures += 1


func _report() -> void:
	print("\n--- godot-servo self check ---")
	print("  path: ", browser.get_backend_name())
	for line in results:
		print(line)
	print("--- %d failed ---\n" % failures)
	get_tree().quit(1 if failures > 0 else 0)
