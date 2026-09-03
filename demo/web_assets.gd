extends RefCounted
## Picks which demo page to open.
##
## The part that is not demo-specific — getting a `res://` tree into a place a
## browser can open after an export — lives in the addon, as `local_pages.gd`.
## What is left here is the demo's own naming: a bare word is a page under
## `demo/web/`, and anything starting with `http` is a URL.
##
## No `class_name`. The global class table is written by the editor and may be
## missing on a checkout that has only run `--import`. Without it the script that
## refers to this one fails to parse with "Identifier not declared", so callers
## reach it through `preload()` and a path instead.

const LocalPages = preload("res://addons/godot_servo/local_pages.gd")

const ROOT := "res://demo/web"


## Returns a URL that opens `res://demo/web/<name>.html`.
## A string starting with `http` is used as a URL as it is.
static func page_url(page: String) -> String:
	if page.begins_with("http"):
		return page
	return LocalPages.url(ROOT, "%s.html" % page)
