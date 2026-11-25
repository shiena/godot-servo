extends Node
## Exercise HTML media through Servo's GStreamer backend.
## Audio is routed to the OS, so Godot bus meters cannot measure its level.

const WebAssets = preload("res://demo/web_assets.gd")
const STARTUP_SECONDS := 60.0

var browser: ServoWebView
var failures := 0


func _ready() -> void:
	browser = ServoWebView.new()
	browser.view_size = Vector2i(640, 360)
	browser.url = WebAssets.page_url("audio")
	add_child(browser)
	_run()


func _run() -> void:
	await _wait_for_load()
	var bytes := FileAccess.get_file_as_bytes("res://demo/web/media/tone.wav")
	await _eval("loadElement('%s', 'audio/wav')" % Marshalls.raw_to_base64(bytes))
	var metadata := await _wait_for_media_event("loadedmetadata", 10.0)
	_check("<audio> reports its duration", metadata.ends_with(" 2.00"), metadata)
	await _eval("element.play()")
	var ended := await _wait_for_media_event("ended", 5.0)
	_check("<audio> plays to the end", ended.begins_with("ended 2.00"), ended)
	await _check_video()
	print("audiotest: %s" % ("PASS" if failures == 0 else "FAIL (%d)" % failures))
	get_tree().quit(1 if failures > 0 else 0)


func _wait_for_load() -> void:
	var deadline := Time.get_ticks_msec() + int(STARTUP_SECONDS * 1000)
	while Time.get_ticks_msec() < deadline:
		var status: String = await _eval("typeof audioStatus === 'function' ? 'ready' : 'no'")
		if status == "ready":
			return
		await get_tree().create_timer(0.25).timeout
	_check("page loaded", false, "timed out")
	get_tree().quit(1)


func _check_video() -> void:
	var url := OS.get_environment("GODOT_SERVO_TEST_MEDIA")
	if url.is_empty():
		var bytes := FileAccess.get_file_as_bytes("res://demo/web/media/platform-test.mp4")
		url = "data:video/mp4;base64," + Marshalls.raw_to_base64(bytes)
	await _eval("window.testVideo = document.body.appendChild(document.createElement('video'))")
	await _eval("testVideo.onended = () => { window.videoEnded = true }")
	await _eval("testVideo.src = %s" % JSON.stringify(url))
	var style := "position:fixed;inset:0;width:100vw;height:100vh;object-fit:fill;z-index:999"
	await _eval("testVideo.style.cssText = '%s'" % style)
	var deadline := Time.get_ticks_msec() + 10000
	while Time.get_ticks_msec() < deadline:
		if await _eval("String(testVideo.readyState >= 2 || !!testVideo.error)") == "true":
			break
		await get_tree().create_timer(0.1).timeout
	var size := await _eval("testVideo.videoWidth + 'x' + testVideo.videoHeight")
	_check("<video> loads the MP4 video track", size == "320x180", size)
	await _eval("testVideo.play()")
	await get_tree().create_timer(0.75).timeout
	var image := browser.get_texture().get_image()
	var pixel := image.get_pixel(image.get_width() / 2, image.get_height() / 2)
	var red := pixel.r > 0.7 and pixel.b < 0.2 and pixel.g < 0.2
	_check("<video> renders red in the page texture", red, str(pixel))
	var position := await _eval("String(testVideo.currentTime)")
	_check("<video> advances its clock", float(position) > 0.0, position)
	if url.begins_with("http://") and float(position) > 0.0:
		await _eval("void fetch(new URL('/playing', testVideo.src))")
	await _eval("testVideo.pause()")
	await _eval("testVideo.currentTime = testVideo.duration - 1")
	await get_tree().create_timer(0.25).timeout
	var seek := await _eval(
		"String(!testVideo.seeking && Math.abs(testVideo.duration - testVideo.currentTime - 1) < 0.1)")
	_check("<video> seeks while paused", seek == "true", seek)
	await _eval("testVideo.play()")
	var ended := false
	deadline = Time.get_ticks_msec() + 18000
	while Time.get_ticks_msec() < deadline:
		ended = await _eval("String(window.videoEnded === true)") == "true"
		if ended:
			break
		await get_tree().create_timer(0.1).timeout
	_check("<video> ends after the seek", ended, str(ended))


## The newest `event` in the element's log, waiting up to `timeout` seconds for
## it. Empty when it never came.
func _wait_for_media_event(event: String, timeout: float) -> String:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000)
	while Time.get_ticks_msec() < deadline:
		var found := await _eval(
			"mediaLog.filter((e) => e.startsWith('%s ')).pop() || ''" % event)
		if found != "":
			return found
		await get_tree().create_timer(0.05).timeout
	return ""


func _eval(code: String) -> String:
	browser.evaluate_javascript(code)
	var result: Array = await browser.script_result
	return str(result[1])


func _check(label: String, ok: bool, detail: String) -> void:
	print("%s %s (%s)" % ["PASS" if ok else "FAIL", label, detail])
	if not ok:
		failures += 1
