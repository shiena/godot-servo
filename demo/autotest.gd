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
## Deadline for the whole run. A CI runner takes about a minute for the lot, so
## this leaves room for a step to go slow without letting the run hang.
const WATCHDOG_SECONDS := 180
## Deadline for one check against a page that is already up.
const STEP_SECONDS := 8.0
## Deadline for the first sign of life. Starting Servo and laying the page out is
## the slow part: a CI runner without a GPU rasterises on the CPU, where what
## takes a moment on a desktop took most of a minute.
const STARTUP_SECONDS := 60.0
## How long an evaluation may go unanswered before something is badly wrong.
## Servo answers every call it is handed, in well under a tenth of a second even
## on a page that has not loaded, so this is not a deadline anything waits out.
const ANSWER_SECONDS := 5.0

var browser: ServoWebView
var results: Array[String] = []
var failures := 0
var _started_msec := 0

var button_point := Vector2.ZERO
var scrolled_before := -1.0
var finished := false

var dialog_message := ""
var dialog_default := ""
var select_options: Array = []
var select_multiple := false


func _ready() -> void:
	_started_msec = Time.get_ticks_msec()
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
	# 1. Wait for the godot.emit('ready') the page sends on load. Asking a page
	#    that has not come up yet answers with an error instead of a value, so
	#    there is nothing worth checking until this arrives.
	await _expect("bridge_event (godot.emit)", "ready", STARTUP_SECONDS)

	# 2. JavaScript evaluation and the script_result signal.
	#    Picks up the first button's centre on the way.
	button_point = await _element_center("button", STARTUP_SECONDS)
	_check("evaluate_javascript / script_result", button_point != Vector2.ZERO,
		"button at %s" % button_point)

	# 3. Forward a click and see the page's onclick send back a bridge_event.
	_click(button_point)
	await _expect("click -> onclick -> bridge_event", "buy", STEP_SECONDS)

	# 4. Forward a tap and see onclick fire the same way a click does.
	_touch_tap(button_point)
	await _expect("touch tap -> onclick -> bridge_event", "buy", STEP_SECONDS)

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
	var requested := await _wait_for(func() -> bool: return ime_caret != Rect2(), STEP_SECONDS)
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
	#    The three dialog calls are the one place an evaluation is fired and not
	#    awaited: the page sits inside alert() until respond_to_dialog() lets it
	#    go, so the answer cannot arrive until further down.
	dialog_message = ""
	browser.evaluate_javascript("alert('hello from the page')")
	var alerted := await _wait_for(func() -> bool: return dialog_message != "", STEP_SECONDS)
	_check("alert -> dialog_alert", alerted and browser.has_pending_dialog(),
		"message '%s'" % dialog_message)
	browser.respond_to_dialog(true, "")
	await _sleep(0.3)
	_check("respond_to_dialog releases the page", not browser.has_pending_dialog(), "no pending")

	# 10. The answer to confirm() should become the page's value.
	dialog_message = ""
	browser.evaluate_javascript("window.__confirmed = confirm('really?')")
	await _wait_for(func() -> bool: return dialog_message != "", STEP_SECONDS)
	browser.respond_to_dialog(true, "")
	await _sleep(0.5)
	var confirmed: Variant = await _evaluate("window.__confirmed")
	_check("confirm -> respond_to_dialog(true)", confirmed == true, "value %s" % confirmed)

	# 11. The text given to prompt() should land in the page.
	dialog_message = ""
	browser.evaluate_javascript("window.__prompted = prompt('name?', 'hero')")
	await _wait_for(func() -> bool: return dialog_message != "", STEP_SECONDS)
	_check("prompt -> dialog_prompt", dialog_default == "hero",
		"default '%s'" % dialog_default)
	browser.respond_to_dialog(true, "godot")
	await _sleep(0.5)
	var prompted: Variant = await _evaluate("window.__prompted")
	_check("prompt -> respond_to_dialog(text)", prompted == "godot", "value '%s'" % prompted)

	# 12. The <select> options should arrive and the answer should set the value.
	#     optgroups come through flattened into a single array.
	select_options = []
	var select_point := await _element_center("#job")
	_click(select_point)
	var asked := await _wait_for(func() -> bool: return not select_options.is_empty(), STEP_SECONDS)
	_check("select -> select_element_requested", asked and select_options.size() == 4,
		"%d options, last group '%s'" % [
			select_options.size(),
			select_options[-1].get("group", "") if asked else "",
		])
	if asked:
		browser.respond_to_select([select_options[-1]["id"]])
		await _sleep(0.5)
		var job: Variant = await _evaluate("document.querySelector('#job').value")
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

	# 14. Ctrl+A then Ctrl+C should reach the page and put the field on the
	#     clipboard. Holding Ctrl makes the OS deliver a control character, which
	#     Godot reports as unicode 0, so the shortcut only survives if the
	#     extension recovers the letter from the keycode.
	await _settle_scroll_to_top()
	await _clear_input("#name")
	_click(await _element_center("#name"))
	await _sleep(0.3)
	_send_text("clip")
	await _sleep(0.3)
	DisplayServer.clipboard_set("(not copied)")
	_send_shortcut(KEY_A)
	await get_tree().process_frame
	_send_shortcut(KEY_C)
	await _sleep(0.5)
	var copied := DisplayServer.clipboard_get()
	_check("ctrl+C copies the selection", copied == "clip", "clipboard '%s'" % copied)

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
		await _evaluate("document.querySelector('main').scrollTop = 0")
		# Not for the assignment, which has landed by now, but for whatever
		# momentum is left to move the page again.
		await _sleep(0.3)
		if await _scroll_position() == 0.0:
			return
	push_warning("godot-servo self check: the page kept scrolling")


## Sends a Ctrl shortcut the way Windows does: no usable unicode, only a keycode.
func _send_shortcut(keycode: Key) -> void:
	for pressed in [true, false]:
		var key := InputEventKey.new()
		key.keycode = keycode
		key.physical_keycode = keycode
		key.ctrl_pressed = true
		key.pressed = pressed
		browser.feed_input(key, Vector2.ZERO)


## Sends committed text the way Windows does: key events carrying a unicode value.
func _send_text(text: String) -> void:
	for character in text:
		var key := InputEventKey.new()
		key.pressed = true
		key.unicode = character.unicode_at(0)
		browser.feed_input(key, Vector2.ZERO)


func _clear_input(selector: String) -> void:
	await _evaluate("document.querySelector('%s').value = ''" % selector)


## A direction of -1 scrolls downwards.
func _wheel(point: Vector2, direction: int) -> void:
	var wheel := InputEventMouseButton.new()
	wheel.button_index = MOUSE_BUTTON_WHEEL_DOWN if direction < 0 else MOUSE_BUTTON_WHEEL_UP
	wheel.pressed = true
	wheel.factor = 1.0
	browser.feed_input(wheel, point)


# ── Plumbing for the checks ──────────────────────────────────────────────

var _last_event_name := ""
## Outcomes of evaluations, keyed by the id `evaluate_javascript()` returned.
var _answers: Dictionary = {}


func _on_bridge_event(event_name: String, payload: String) -> void:
	_last_event_name = event_name
	print("  <- bridge_event ", event_name, " ", payload)


func _on_script_result(id: int, value: Variant, error: String) -> void:
	_answers[id] = {"value": value, "error": error}


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


## Evaluates an expression and returns what it came to, or null if it never ran.
##
## Waits on the answer the call is promised rather than on a value of the shape
## the caller wanted. The two are not the same: an evaluation that failed and a
## script that returned null both arrive as null, which is what `error` is for.
##
## An error means the page had not settled, so the call goes again until the
## deadline. `ANSWER_SECONDS` is separate and much shorter: it only catches an
## answer that never comes, which needs Servo itself to be wedged.
func _evaluate(script: String, timeout: float = STEP_SECONDS) -> Variant:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000.0)
	var trouble := "never attempted"
	while Time.get_ticks_msec() < deadline:
		var id: int = browser.evaluate_javascript(script)
		if not await _wait_for(func() -> bool: return _answers.has(id), ANSWER_SECONDS):
			push_error("godot-servo self check: evaluation %d was never answered" % id)
			return null
		var answer: Dictionary = _answers[id]
		_answers.erase(id)
		if answer["error"] == "":
			return answer["value"]
		trouble = answer["error"]
		await _sleep(0.1)
	push_warning("godot-servo self check: %s" % trouble)
	return null


## Returns the centre of the element the selector names, in WebView pixels.
func _element_center(selector: String, timeout: float = STEP_SECONDS) -> Vector2:
	var value: Variant = await _evaluate(
		"(function(){var r=document.querySelector('%s').getBoundingClientRect();"
		% selector + "return [r.x+r.width/2, r.y+r.height/2];})()", timeout)
	if value is Array and value.size() == 2:
		return Vector2(value[0], value[1])
	return Vector2.ZERO


func _input_value(selector: String) -> String:
	var value: Variant = await _evaluate("document.querySelector('%s').value" % selector)
	return value if value is String else ""


func _scroll_position() -> float:
	var value: Variant = await _evaluate("document.querySelector('main').scrollTop")
	return value if value is float else -1.0


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


## Each line carries the second it was reached. CI prints the whole run at once,
## so this is the only way to tell a check that failed outright from one that
## merely waited past its deadline.
func _check(label: String, ok: bool, detail: String) -> void:
	var at := float(Time.get_ticks_msec() - _started_msec) / 1000.0
	if ok:
		results.append("  OK   [%5.1fs] %s  (%s)" % [at, label, detail])
	else:
		results.append("  FAIL [%5.1fs] %s  (%s)" % [at, label, detail])
		failures += 1


func _report() -> void:
	print("\n--- godot-servo self check ---")
	print("  path: ", browser.get_backend_name())
	for line in results:
		print(line)
	print("--- %d failed ---\n" % failures)
	get_tree().quit(1 if failures > 0 else 0)
