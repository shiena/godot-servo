extends Node
## Self check for audio: a page's Web Audio has to reach Godot's Master bus.
##
## Plays a tone from `demo/web/audio.html`, reads the bus peak while it plays,
## and again after the page suspends its AudioContext. While the tone plays, the
## samples themselves are captured and checked for gaps and jumps, which is what
## an underrun or a misjoined buffer sounds like.
##
## Then decodes the same tone from each file format with decodeAudioData, checks
## what came back, and plays one of them. Last, plays it through an <audio>
## element: metadata, level, gaps, the clock, pause, seeking and the end.
##
##     godot --path . res://demo/audiotest.tscn
##
## Results go to standard output. Exit code 0 when everything passed.

const WebAssets = preload("res://demo/web_assets.gd")

## File, channels in it. Every file is two seconds of the tone.
const DECODABLE := {
	"tone.wav": 1,
	"tone.flac": 2,
	"tone.ogg": 2,
	"tone.webm": 1,
	"tone.mp3": 1,
}
## Encoders pad the end of lossy formats, and the decoder may not trim it all.
const DURATION_TOLERANCE := 0.06

const STARTUP_SECONDS := 60.0
const LISTEN_SECONDS := 2.0
## A 0.5 gain sine peaks at about -6 dB; anything this loud is the tone.
const LOUD_DB := -20.0
## The meter falls to -200 dB in silence.
const QUIET_DB := -60.0
## The largest step between neighbouring samples a 440 Hz sine at 0.5 can take
## at 44.1 kHz is about 0.031. Twice that leaves room for resampling.
const MAX_STEP := 0.07
## Consecutive near-zero samples that mean the tone dropped out. A sine passes
## through zero in one or two.
const GAP_SAMPLES := 8

var browser: ServoWebView
var failures := 0
var capture := AudioEffectCapture.new()


func _ready() -> void:
	browser = ServoWebView.new()
	browser.view_size = Vector2i(640, 360)
	browser.url = WebAssets.page_url("audio")
	add_child(browser)
	capture.buffer_length = 2.0
	AudioServer.add_bus_effect(0, capture)
	_run()


func _run() -> void:
	await _wait_for_load()

	var before := await _peak_db(0.5)
	_check("silent before the page plays", before < QUIET_DB, "%.1f dB" % before)

	await _eval("document.getElementById('start').click()")
	var playing := await _peak_db(LISTEN_SECONDS)
	_check("the tone reaches the Master bus", playing > LOUD_DB, "%.1f dB" % playing)

	# A second's worth, starting once the tone is surely running.
	capture.clear_buffer()
	await get_tree().create_timer(1.0).timeout
	var samples := capture.get_buffer(capture.get_frames_available())
	_check_continuity(samples)
	print("  page: %s" % await _eval("audioStatus()"))

	await _eval("document.getElementById('stop').click()")
	await get_tree().create_timer(0.5).timeout
	var stopped := await _peak_db(1.0)
	_check("silent after suspend()", stopped < QUIET_DB, "%.1f dB" % stopped)
	print("  page: %s" % await _eval("audioStatus()"))

	var rate := (await _eval("audioStatus()")).split(" ")[2]
	for file: String in DECODABLE:
		var result := await _decode(file)
		var parts := result.split(" ")
		var ok: bool = (
			parts.size() == 4
			and parts[0] == "ok"
			and parts[1] == rate
			and int(parts[2]) == DECODABLE[file]
			and absf(float(parts[3]) - 2.0) <= DURATION_TOLERANCE
		)
		_check("decodeAudioData %s" % file, ok, result)
	var aac := await _decode("tone.m4a")
	_check("decodeAudioData tone.m4a is refused without the aac feature",
		aac.begins_with("error"), aac)

	# 440 Hz fits a whole number of cycles in two seconds, so the loop is seamless.
	await _eval("playDecoded('tone.wav')")
	var decoded := await _peak_db(LISTEN_SECONDS)
	_check("a decoded file reaches the Master bus", decoded > LOUD_DB, "%.1f dB" % decoded)
	capture.clear_buffer()
	await get_tree().create_timer(1.0).timeout
	_check_continuity(capture.get_buffer(capture.get_frames_available()))
	await _eval("document.getElementById('stop').click()")

	await _check_element()

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


## The loudest the Master bus got over `seconds`.
func _peak_db(seconds: float) -> float:
	var loudest := -200.0
	var deadline := Time.get_ticks_msec() + int(seconds * 1000)
	while Time.get_ticks_msec() < deadline:
		await get_tree().process_frame
		loudest = maxf(loudest, AudioServer.get_bus_peak_volume_left_db(0, 0))
	return loudest


func _check_element() -> void:
	for type in ['audio/ogg; codecs="vorbis"', "audio/mpeg", "audio/wav"]:
		var answer := await _eval("element.canPlayType('%s')" % type.replace("'", "\'"))
		_check("canPlayType(%s)" % type, answer == "probably", answer)
	for type in ['audio/ogg; codecs="opus"', "video/mp4"]:
		var answer := await _eval("element.canPlayType('%s')" % type.replace("'", "\'"))
		_check("canPlayType(%s) is refused" % type, answer == "", answer)

	var bytes := FileAccess.get_file_as_bytes("res://demo/web/media/tone.wav")
	await _eval("loadElement('%s', 'audio/wav')" % Marshalls.raw_to_base64(bytes))
	var metadata := await _wait_for_media_event("loadedmetadata", 10.0)
	_check("<audio> loads metadata", metadata.ends_with(" 2.00"), metadata)

	await _eval("element.play()")
	var playing := await _peak_db(0.6)
	_check("<audio> reaches the Master bus", playing > LOUD_DB, "%.1f dB" % playing)
	capture.clear_buffer()
	await get_tree().create_timer(0.5).timeout
	_check_continuity(capture.get_buffer(capture.get_frames_available()))

	await _eval("element.pause()")
	await get_tree().create_timer(0.3).timeout
	var paused := await _peak_db(0.3)
	_check("silent while paused", paused < QUIET_DB, "%.1f dB" % paused)
	var held := float((await _eval("mediaStatus()")).split(" ")[2])
	await get_tree().create_timer(0.3).timeout
	var status := await _eval("mediaStatus()")
	var time := float(status.split(" ")[2])
	_check("currentTime holds while paused", time == held and time > 0.6 and time < 1.4,
		"%.2f then %s" % [held, status])

	# Back near the start, then play to the end. Servo 0.5.0 cannot play an
	# element again once it has ended, so the end comes last.
	await _eval("element.currentTime = 0.5")
	var seeked := await _wait_for_media_event("seeked", 3.0)
	_check("<audio> seeks", seeked.begins_with("seeked 0.5"), seeked)
	await _eval("element.play()")
	await get_tree().create_timer(0.8).timeout
	status = await _eval("mediaStatus()")
	time = float(status.split(" ")[2])
	_check("currentTime follows playback after the seek", time > 0.9 and time < 1.5, status)
	var ended := await _wait_for_media_event("ended", 3.0)
	_check("<audio> ends at its duration", ended.begins_with("ended 2.00"), ended)
	var after := await _peak_db(0.5)
	_check("silent after the end", after < QUIET_DB, "%.1f dB" % after)
	print("  events: %s" % await _eval("mediaLog.join(', ')"))

	# The other formats only need to open: playing is the same code from here.
	for file: String in ["tone.flac", "tone.ogg", "tone.webm", "tone.mp3"]:
		var data := FileAccess.get_file_as_bytes("res://demo/web/media/" + file)
		var type: String = {"flac": "audio/flac", "ogg": "audio/ogg", "webm": "audio/webm",
			"mp3": "audio/mpeg"}[file.get_extension()]
		await _eval("loadElement('%s', '%s')" % [Marshalls.raw_to_base64(data), type])
		var loaded := await _wait_for_media_event("loadedmetadata", 10.0)
		# Without a Content-Length an Ogg file's duration comes once all of it
		# has arrived, as a durationchange after loadedmetadata.
		var duration := 0.0
		if loaded != "":
			duration = await _wait_for_duration(2.0)
		_check("<audio> opens %s" % file, absf(duration - 2.0) <= DURATION_TOLERANCE,
			"%s, duration %.2f" % [loaded, duration])


## The element's duration once it is finite, waiting up to `timeout` seconds.
## 0 when it stayed unknown.
func _wait_for_duration(timeout: float) -> float:
	var deadline := Time.get_ticks_msec() + int(timeout * 1000)
	while Time.get_ticks_msec() < deadline:
		var duration := await _eval("String(document.getElementById('element').duration)")
		if duration.is_valid_float():
			return float(duration)
		await get_tree().create_timer(0.05).timeout
	return 0.0


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


## What decodeAudioData made of `file`: "ok <rate> <channels> <seconds>" or
## "error <message>".
func _decode(file: String) -> String:
	var bytes := FileAccess.get_file_as_bytes("res://demo/web/media/" + file)
	await _eval("decodeBase64('%s', '%s')" % [file, Marshalls.raw_to_base64(bytes)])
	var deadline := Time.get_ticks_msec() + 10000
	while Time.get_ticks_msec() < deadline:
		var result := await _eval("decodeResults['%s'] || ''" % file)
		if result != "":
			return result
		await get_tree().create_timer(0.1).timeout
	return "timed out"


## A clean tone has neither jumps between samples nor runs of silence.
func _check_continuity(samples: PackedVector2Array) -> void:
	var largest_step := 0.0
	var gaps := 0
	var quiet_run := 0
	for i in range(1, samples.size()):
		largest_step = maxf(largest_step, absf(samples[i].x - samples[i - 1].x))
		if absf(samples[i].x) < 0.01:
			quiet_run += 1
			if quiet_run == GAP_SAMPLES:
				gaps += 1
		else:
			quiet_run = 0
	var detail := "%d samples, largest step %.3f, %d gaps" % [samples.size(), largest_step, gaps]
	var clean := samples.size() > 0 and largest_step < MAX_STEP and gaps == 0
	_check("the tone plays without gaps or jumps", clean, detail)


func _eval(code: String) -> String:
	browser.evaluate_javascript(code)
	var result: Array = await browser.script_result
	return str(result[1])


func _check(label: String, ok: bool, detail: String) -> void:
	print("%s %s (%s)" % ["PASS" if ok else "FAIL", label, detail])
	if not ok:
		failures += 1
