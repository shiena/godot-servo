extends RefCounted
## Turns a page bundled under `res://` into a URL Servo can open.
##
## No `class_name` here. The global class table is written by the editor and may
## be missing on a checkout that has only run `--import`; a script naming an
## absent global class then fails to parse with "Identifier not declared". Reach
## this one through `preload()` and a path instead:
##
## [codeblock]
## const LocalPages = preload("res://addons/godot_servo/local_pages.gd")
##
## browser.url = LocalPages.url("res://ui", "shop.html")
## [/codeblock]
##
## Running from the editor, `res://` is a real directory on disk and the URL
## points straight at it. After an export the files live inside the PCK with no
## file behind them, and a `file://` URL onto `res://` gives Servo
## `Opening file failed`. Only then is the tree copied out to `user://`.

## Where an exported build unpacks to, before the tree's own `res://` path.
const CACHE_ROOT := "user://godot_servo/pages/"


## A `file://` URL for `relative_path` inside `root`.
##
## `root` is the directory the page and everything it references sit under, and
## the whole of it travels together: a page's `<link>` and `<script>` resolve
## against wherever the page itself ended up, so moving the page alone would
## leave its stylesheet behind.
static func url(root: String, relative_path: String) -> String:
	var absolute := directory(root).path_join(relative_path)
	return "file:///" + absolute.replace("\\", "/").trim_prefix("/")


## The absolute directory `root`'s files can be opened from.
##
## After an export that means copying them out first, which happens once: the
## call is cheap enough to make on every page load.
static func directory(root: String) -> String:
	# The test has to be `OS.has_feature("editor")`. On an exported Android build
	# `globalize_path()` hands `res://...` straight back, and
	# `FileAccess.file_exists()` looks inside the PCK and answers true, so
	# neither the path nor the file tells us whether anything is really there.
	if OS.has_feature("editor"):
		return ProjectSettings.globalize_path(root)

	var cache := CACHE_ROOT.path_join(root.trim_prefix("res://"))
	_mirror(root, cache)
	return ProjectSettings.globalize_path(cache)


## Copies everything under `from` into `to`, recursively.
##
## A file already there is rewritten only when its length differs, so the second
## launch copies nothing while an update that changed a page still lands.
static func _mirror(from: String, to: String) -> void:
	DirAccess.make_dir_recursive_absolute(to)
	var dir := DirAccess.open(from)
	if dir == null:
		push_error("godot-servo: cannot open %s" % from)
		return

	for directory_name in dir.get_directories():
		_mirror(from.path_join(directory_name), to.path_join(directory_name))

	for file_name in dir.get_files():
		# Exporting can append .remap to text files. Strip it back off.
		var source := from.path_join(file_name)
		var destination := to.path_join(file_name.trim_suffix(".remap"))
		var bytes := FileAccess.get_file_as_bytes(source)
		if bytes.is_empty():
			continue
		if _same_length(destination, bytes.size()):
			continue
		var out := FileAccess.open(destination, FileAccess.WRITE)
		if out == null:
			push_error("godot-servo: cannot write %s" % destination)
			continue
		out.store_buffer(bytes)
		out.close()


static func _same_length(path: String, length: int) -> bool:
	var file := FileAccess.open(path, FileAccess.READ)
	if file == null:
		return false
	var existing := file.get_length()
	file.close()
	return existing == length
