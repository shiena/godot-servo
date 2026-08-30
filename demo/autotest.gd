extends Node
## 入力転送とシグナルのセルフチェック。
##
## 実ページの中からボタンの座標を JavaScript で問い合わせ、そこへ合成した
## マウス・タッチイベントを送り、`bridge_event` が返ってくるかを見る。ホイールと
## 指のドラッグでスクロールが効いているかも確認する。
##
##     Godot --path demo --quit-after 900 -- --scene autotest
##
## 結果は標準出力に出る。すべて OK なら終了コード 0。

const VIEW_SIZE := Vector2i(1280, 720)
## 検査全体の制限時間。CI の遅いランナーでも 1 分あれば終わる。
const WATCHDOG_SECONDS := 180

var browser: ServoWebView
var results: Array[String] = []
var failures := 0

var button_point := Vector2.ZERO
var scrolled_before := -1.0
var finished := false


func _ready() -> void:
	browser = ServoWebView.new()
	browser.view_size = VIEW_SIZE
	browser.url = _local_page_url()
	add_child(browser)

	browser.bridge_event.connect(_on_bridge_event)
	browser.script_result.connect(_on_script_result)
	browser.load_finished.connect(_on_load_finished)
	browser.ime_requested.connect(_on_ime_requested)

	_watchdog()
	_run()


## 検査全体の上限。想定より遅い環境でも、黙って回り続けるより落ちたほうがよい。
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
	# 1. 起動時に飛んでくる godot.emit('ready') を待つ。
	await _expect("bridge_event (godot.emit)", "ready", 8.0)

	# 2. JavaScript の評価と script_result シグナル。
	#    ついでに最初のボタンの中心座標を取る。
	browser.evaluate_javascript("""
		(function() {
			var r = document.querySelector('button').getBoundingClientRect();
			return [r.x + r.width / 2, r.y + r.height / 2];
		})()
	""")
	await _wait_for(func() -> bool: return button_point != Vector2.ZERO, 8.0)
	_check("evaluate_javascript / script_result", button_point != Vector2.ZERO,
		"button at %s" % button_point)

	# 3. クリックを転送して、ページの onclick から bridge_event が返るか。
	_click(button_point)
	await _expect("click -> onclick -> bridge_event", "buy", 8.0)

	# 4. タップを転送して、クリックと同じように onclick が動くか。
	_touch_tap(button_point)
	await _expect("touch tap -> onclick -> bridge_event", "buy", 8.0)

	# 5. 指のドラッグでスクロールするか。慣性が乗るので落ち着くまで待つ。
	var touch_before := await _scroll_position()
	await _touch_drag(Vector2(VIEW_SIZE) * Vector2(0.5, 0.8), Vector2(VIEW_SIZE) * Vector2(0.5, 0.3))
	await _sleep(1.5)
	var touch_after := await _scroll_position()
	_check("touch drag -> scroll", touch_after > touch_before + 1.0,
		"scrollTop %.0f -> %.0f" % [touch_before, touch_after])
	await _settle_scroll_to_top()

	# 6. テキスト欄をクリックすると Servo が IME を要求してくるか。
	var input_point := await _element_center("#name")
	print("  -> click input at ", input_point)
	_click(input_point)
	var requested := await _wait_for(func() -> bool: return ime_caret != Rect2(), 8.0)
	_check("focus input -> ime_requested", requested, "caret %s" % ime_caret)

	# 7. 未確定文字列を送り込んで、確定した文字が入力欄に入るか。
	browser.feed_ime_composition("start", "に")
	await get_tree().process_frame
	browser.feed_ime_composition("update", "にほん")
	await get_tree().process_frame
	browser.feed_ime_composition("end", "日本語")
	await _sleep(0.5)
	var value := await _input_value("#name")
	_check("ime composition -> input value", value == "日本語", "value '%s'" % value)

	# 8. OS の IME と同じ順序を再現する。未確定文字列 → 空 → 確定文字のキーイベント。
	#    未確定分が残って二重に入らないことを見る。
	await _clear_input("#name")
	browser.feed_ime_preedit("にほん")
	await get_tree().process_frame
	browser.feed_ime_preedit("")
	_send_text("日本")
	await _sleep(0.5)
	var committed := await _input_value("#name")
	_check("os ime sequence -> committed once", committed == "日本",
		"value '%s'" % committed)

	# 9. ホイールを転送してスクロールが動くか。
	scrolled_before = await _scroll_position()
	for i in 8:
		_wheel(Vector2(VIEW_SIZE) * 0.5, -1)
		await get_tree().process_frame
	await _sleep(1.0)
	var after := await _scroll_position()
	_check("wheel -> scroll", after > scrolled_before + 1.0,
		"scrollTop %.0f -> %.0f" % [scrolled_before, after])

	_report()


# ── 入力の合成 ────────────────────────────────────────────────────────────

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


## 指 1 本で軽く触って離す。
func _touch_tap(point: Vector2) -> void:
	var press := InputEventScreenTouch.new()
	press.index = 0
	press.pressed = true
	browser.feed_input(press, point)

	var release := InputEventScreenTouch.new()
	release.index = 0
	release.pressed = false
	browser.feed_input(release, point)


## 指 1 本で `from` から `to` までなぞる。
##
## Servo のタッチハンドラは 10 px 動くまでパンに切り替わらないので、途中経過を
## 刻んで送る。最後に同じ座標を数回送ってから離すのは、慣性を乗せないため。
## 動いたまま離すと Servo がフリングを始め、そのあとの検査が
## 「指を離した時点のスクロール位置」に依存して不安定になる。
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

	# 速度を 0 に落としてから離す。
	for i in 3:
		var still := InputEventScreenDrag.new()
		still.index = 0
		browser.feed_input(still, to)
		await get_tree().process_frame

	var release := InputEventScreenTouch.new()
	release.index = 0
	release.pressed = false
	browser.feed_input(release, to)


## スクロール位置を先頭に戻し、本当に止まるまで待つ。
##
## 慣性が残っていると、要素の座標を取ってからクリックが届くまでの間に
## ページが動いてしまい、狙った要素に当たらない。
func _settle_scroll_to_top() -> void:
	var deadline := Time.get_ticks_msec() + 8000
	while Time.get_ticks_msec() < deadline:
		browser.evaluate_javascript("document.querySelector('main').scrollTop = 0")
		await _sleep(0.3)
		if await _scroll_position() == 0.0:
			return
	push_warning("godot-servo self check: the page kept scrolling")


## 確定文字列を Windows と同じ形 (unicode 付きのキーイベント) で送る。
func _send_text(text: String) -> void:
	for character in text:
		var key := InputEventKey.new()
		key.pressed = true
		key.unicode = character.unicode_at(0)
		browser.feed_input(key, Vector2.ZERO)


func _clear_input(selector: String) -> void:
	browser.evaluate_javascript("document.querySelector('%s').value = ''" % selector)
	await _sleep(0.3)


## direction が -1 なら下方向へスクロールする。
func _wheel(point: Vector2, direction: int) -> void:
	var wheel := InputEventMouseButton.new()
	wheel.button_index = MOUSE_BUTTON_WHEEL_DOWN if direction < 0 else MOUSE_BUTTON_WHEEL_UP
	wheel.pressed = true
	wheel.factor = 1.0
	browser.feed_input(wheel, point)


# ── 検証の足回り ──────────────────────────────────────────────────────────

var _last_event_name := ""
var _script_values: Dictionary = {}
var _scroll_id := -1
var _scroll_value := 0.0
var _element_id := -1
var _element_point := Vector2.ZERO
var _string_id := -1
var _string_value := ""


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


func _on_load_finished() -> void:
	print("  <- load finished")


var ime_caret := Rect2()


func _on_ime_requested(caret: Rect2, multiline: bool) -> void:
	ime_caret = caret
	print("  <- ime_requested ", caret, " multiline=", multiline)


## セレクタで指した要素の中心を WebView のピクセル座標で返す。
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
