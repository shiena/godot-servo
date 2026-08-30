class_name WebAssets
extends RefCounted
## デモのページをブラウザから開ける場所に用意する。
##
## エディタから実行しているときは `res://` がそのまま実ファイルなので何もしない。
## 書き出したあとは PCK の中に入っていて実体が無く、Servo に `file://` で渡しても
## `Opening file failed` になる。その場合だけ `user://` へ展開する。


## `res://demo/web/<name>.html` を開ける URL にして返す。
## `http` で始まる文字列はそのまま URL として扱う。
static func page_url(page: String) -> String:
	if page.begins_with("http"):
		return page

	var source_dir := "res://demo/web"
	var absolute := ProjectSettings.globalize_path(source_dir)
	if not FileAccess.file_exists(absolute.path_join("index.html")):
		absolute = ProjectSettings.globalize_path("user://web")
		_mirror(source_dir, "user://web")

	var path := absolute.path_join("%s.html" % page)
	return "file:///" + path.replace("\\", "/").trim_prefix("/")


## `from` の中身を `to` へ再帰的に複製する。
static func _mirror(from: String, to: String) -> void:
	DirAccess.make_dir_recursive_absolute(to)
	var dir := DirAccess.open(from)
	if dir == null:
		push_error("godot-servo demo: cannot open %s" % from)
		return

	for name in dir.get_directories():
		_mirror(from.path_join(name), to.path_join(name))

	for name in dir.get_files():
		# 書き出し時にテキストは .remap が付くことがあるので剥がす。
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
