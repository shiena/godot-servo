extends RefCounted
## Puts the demo pages somewhere the browser can open them.
##
## No `class_name` here. The global class table is written by the editor and may
## be missing on a checkout that has only run `--import`. Without it the script
## that refers to this one fails to parse with "Identifier not declared", so
## callers reach it through `preload()` and a path instead.
##
## Running from the editor, `res://` is a real file on disk and nothing needs
## doing. After an export the pages live inside the PCK with no file behind them,
## and handing Servo a `file://` URL gives `Opening file failed`. Only then are
## they unpacked into `user://`.


## Returns a URL that opens `res://demo/web/<name>.html`.
## A string starting with `http` is used as a URL as it is.
static func page_url(page: String) -> String:
	if page.begins_with("http"):
		return page

	var source_dir := "res://demo/web"
	var absolute := ProjectSettings.globalize_path(source_dir)
	# The test has to be `OS.has_feature("editor")`. On an exported Android build
	# `globalize_path()` hands `res://...` straight back, and `FileAccess.file_exists()`
	# looks inside the PCK and returns true, so the path itself tells us nothing.
	if not OS.has_feature("editor"):
		absolute = ProjectSettings.globalize_path("user://web")
		_mirror(source_dir, "user://web")

	var path := absolute.path_join("%s.html" % page)
	return "file:///" + path.replace("\\", "/").trim_prefix("/")


## Copies everything under `from` into `to`, recursively.
static func _mirror(from: String, to: String) -> void:
	DirAccess.make_dir_recursive_absolute(to)
	var dir := DirAccess.open(from)
	if dir == null:
		push_error("godot-servo demo: cannot open %s" % from)
		return

	for name in dir.get_directories():
		_mirror(from.path_join(name), to.path_join(name))

	for name in dir.get_files():
		# Exporting can append .remap to text files. Strip it back off.
		var source := from.path_join(name)
		var destination := to.path_join(name.trim_suffix(".remap"))
		if FileAccess.file_exists(destination):
			continue
		var bytes := FileAccess.get_file_as_bytes(source)
		if bytes.is_empty():
			continue
		var out := FileAccess.open(destination, FileAccess.WRITE)
		if out == null:
			push_error("godot-servo demo: cannot write %s" % destination)
			continue
		out.store_buffer(bytes)
		out.close()
