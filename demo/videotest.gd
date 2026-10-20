extends Node
## Self check for <video>: frames have to reach the page and the screen, in
## step with the clock and the sound.
##
## Plays the clips in `demo/web/media/`. Each is two seconds that turn red,
## green, blue and white every half second, over a 440 Hz tone; one has no
## sound. For each: metadata, the first frame while paused, the colour against
## currentTime while playing, the sound, seeking, and the end.
##
##     godot --path . res://demo/videotest.tscn
##
## Results go to standard output. Exit code 0 when everything passed. A clip
## whose codecs the platform's decoders refuse in canPlayType() is skipped:
## what Linux plays depends on the GStreamer plugins installed. Platforms with
## no video decoder yet skip the whole check.

const WebAssets = preload("res://demo/web_assets.gd")

## File, MIME type with codecs, whether it has sound.
const CLIPS := [
	["clip.mp4", 'video/mp4; codecs="avc1.42E01E, mp4a.40.2"', true],
	["clip.webm", 'video/webm; codecs="vp9, opus"', true],
	["clip-silent.mp4", 'video/mp4; codecs="avc1.42E01E"', false],
]
const COLORS := ["red", "green", "blue", "white"]
const SEGMENT_SECONDS := 0.5
## How far from a colour change a sample has to be to count: the clock and the
## frame on show may each be a frame or so apart.
const MARGIN_SECONDS := 0.12
const DURATION_TOLERANCE := 0.06

const STARTUP_SECONDS := 60.0
const LOUD_DB := -20.0
const QUIET_DB := -60.0

var browser: ServoWebView
var failures := 0


func _ready() -> void:
	browser = ServoWebView.new()
	browser.view_size = Vector2i(640, 480)
	browser.url = WebAssets.page_url("video")
	add_child(browser)
	_run()


func _run() -> void:
	if not OS.get_name() in ["Windows", "Linux"]:
		print("videotest: SKIP (no video decoder on %s yet)" % OS.get_name())
		get_tree().quit(0)
		return
	await _wait_for_load()

	if OS.get_name() == "Windows":
		for type in ['video/mp4; codecs="avc1.42E01E, mp4a.40.2"']:
			var answer := await _eval("element.canPlayType('%s')" % type)
			_check("canPlayType(%s)" % type, answer == "probably", answer)
		for type in ['video/webm; codecs="vp9, opus"', "video/mp4"]:
			var answer := await _eval("element.canPlayType('%s')" % type)
			_check("canPlayType(%s)" % type, answer == "maybe", answer)
	var refused := await _eval("element.canPlayType('video/mp4; codecs=\"xyz1\"')")
	_check("canPlayType of an unknown codec is refused", refused == "", refused)

	var played := 0
	for clip: Array in CLIPS:
		var answer := await _eval("element.canPlayType('%s')" % clip[1])
		if answer == "":
			print("SKIP %s (canPlayType(%s) is refused here)" % [clip[0], clip[1]])
			continue
		played += 1
		await _check_clip(clip[0], clip[1].get_slice(";", 0), clip[2])
	_check("at least one clip plays here", played > 0, "%d of %d" % [played, CLIPS.size()])

	print("videotest: %s" % ("PASS" if failures == 0 else "FAIL (%d)" % failures))
	get_tree().quit(1 if failures > 0 else 0)


func _check_clip(file: String, type: String, sound: bool) -> void:
	print("-- %s" % file)
	var bytes := FileAccess.get_file_as_bytes("res://demo/web/media/" + file)
	await _eval("loadElement('%s', '%s')" % [Marshalls.raw_to_base64(bytes), type])
	var metadata := await _wait_for_media_event("loadedmetadata", 10.0)
	var status := await _eval("mediaStatus()")
	var size := status.split(" ")
	var duration := float(metadata.split(" ")[-1]) if metadata != "" else 0.0
	_check("%s loads metadata" % file,
		absf(duration - 2.0) <= DURATION_TOLERANCE and size[4] == "320" and size[5] == "180",
		"%s; %s" % [metadata, status])
	if metadata == "":
		return

	var first := await _wait_for_color(2.0)
	_check("%s shows its first frame while paused" % file, first == "red", first)
	var shown := await _screen_color()
	_check("%s reaches the screen" % file, shown == "red", shown)

	await _eval("element.play()")
	var result := await _follow(1.2, 4.0)
	_check("%s keeps its frames in step with currentTime" % file,
		result.mismatches == 0 and result.seen.size() >= 3,
		"%d samples, %d off, saw %s" % [result.samples, result.mismatches, ", ".join(result.seen)])
	if sound:
		_check("%s is heard" % file, result.peak > LOUD_DB, "%.1f dB" % result.peak)
	else:
		_check("%s is silent" % file, result.peak < QUIET_DB, "%.1f dB" % result.peak)

	await _eval("element.pause()")
	await _eval("element.currentTime = 0.7")
	var seeked := await _wait_for_media_event("seeked", 3.0)
	await get_tree().create_timer(0.3).timeout
	var after_seek := await _eval("frameColor()")
	_check("%s seeks to the frame at the new time" % file,
		seeked.begins_with("seeked 0.7") and after_seek == "green", "%s, %s" % [seeked, after_seek])
	shown = await _screen_color()
	_check("%s shows the sought frame on screen" % file, shown == "green", shown)

	await _eval("element.play()")
	var ended := await _wait_for_media_event("ended", 4.0)
	_check("%s ends at its duration" % file, ended.begins_with("ended 2.0"), ended)
	var last := await _eval("frameColor()")
	_check("%s ends on its last frame" % file, last == "white", last)
	print("  events: %s" % await _eval("mediaLog.join(', ')"))


## Sample the frame on show against currentTime while playing, until
## currentTime reaches `until`, or `timeout` seconds have passed. How quickly it
## gets there depends on the output's latency, which a slow machine makes long.
func _follow(until: float, timeout: float) -> Dictionary:
	var samples := 0
	var mismatches := 0
	var seen: Array[String] = []
	var peak := -200.0
	var deadline := Time.get_ticks_msec() + int(timeout * 1000)
	while Time.get_ticks_msec() < deadline:
		peak = maxf(peak, AudioServer.get_bus_peak_volume_left_db(0, 0))
		var parts := (await _eval("element.currentTime.toFixed(3) + ' ' + frameColor()")).split(" ")
		var time := float(parts[0])
		if time >= until:
			break
		var color := parts[1]
		if not seen.has(color):
			seen.append(color)
		var segment := int(time / SEGMENT_SECONDS)
		var into := time - segment * SEGMENT_SECONDS
		if into < MARGIN_SECONDS or into > SEGMENT_SECONDS - MARGIN_SECONDS:
			continue
		samples += 1
		if segment >= COLORS.size() or color != COLORS[segment]:
			mismatches += 1
			print("  at %.3f: %s" % [time, color])
	return {"samples": samples, "mismatches": mismatches, "seen": seen, "peak": peak}


## The colour in the middle of the video as Godot has it on screen.
func _screen_color() -> String:
	for i in 5:
		await get_tree().process_frame
	var rect := (await _eval("videoRect()")).split(" ")
	var image := browser.get_texture().get_image()
	var scale := float(image.get_width()) / float(browser.view_size.x)
	var x := int((float(rect[0]) + float(rect[2]) / 2.0) * scale)
	var y := int((float(rect[1]) + float(rect[3]) / 2.0) * scale)
	if x < 0 or y < 0 or x >= image.get_width() or y >= image.get_height():
		return "outside %d,%d" % [x, y]
	var pixel := image.get_pixel(x, y)
	return await _eval("colorName(%d, %d, %d, %d)" % [
		pixel.r8, pixel.g8, pixel.b8, pixel.a8])


## The frame's colour once there is one, waiting up to `timeout` seconds.
func _wait_for_color(timeout: float) -> String:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000)
	var color := "none"
	while Time.get_ticks_msec() < deadline:
		color = await _eval("frameColor()")
		if color != "none":
			return color
		await get_tree().create_timer(0.05).timeout
	return color


func _wait_for_load() -> void:
	var deadline := Time.get_ticks_msec() + int(STARTUP_SECONDS * 1000)
	while Time.get_ticks_msec() < deadline:
		var status: String = await _eval("typeof frameColor === 'function' ? 'ready' : 'no'")
		if status == "ready":
			return
		await get_tree().create_timer(0.25).timeout
	_check("page loaded", false, "timed out")
	get_tree().quit(1)


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
