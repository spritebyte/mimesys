class_name CheatsDialog
extends AcceptDialog
## Emulation > Cheats. One list per game, saved to user://Cheats/<system>/<game>.json
## and switched on automatically next time the game loads.
## Every change is sent to the core right away; a code the core can't read gets a red ✗
## with the reason in its tooltip.
##
## main.gd: call game_loaded(...) after a game starts, and open_dialog() from the menu.

const ROOT := "user://Cheats"
const HELP := {
	"nes": "Game Genie (SXIOPO, 8-letter codes too) or raw AAAA:VV / AAAA?CC:VV",
	"gb": "Game Genie (ABC-DEF-GHI) or GameShark / Action Replay (01VVAAAA)",
}

var _emu: Variant = null           # the EmuSystem
var _mutex: Mutex = null
var _system_id := ""
var _game_key := ""
var _supported := false
var _cheats: Array = []            # {"name", "code", "on"}
var _errors: PackedStringArray = []

var _list := VBoxContainer.new()
var _help := Label.new()
var _empty := Label.new()


func _init() -> void:
	title = "Cheats"
	ok_button_text = "Done"
	min_size = Vector2i(700, 460)

	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 10)
	# top row: Add button first, so it's always visible
	var top := HBoxContainer.new()
	var add := Button.new()
	add.text = "+ Add cheat"
	add.pressed.connect(_add_cheat)
	top.add_child(add)
	_help.modulate = Color(1, 1, 1, 0.7)
	_help.clip_text = true      # no autowrap: a wrapping label makes the window sized huge
	_help.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	top.add_child(_help)
	box.add_child(top)

	var scroll := ScrollContainer.new()
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	scroll.custom_minimum_size.y = 320
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_list.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	scroll.add_child(_list)
	box.add_child(scroll)

	_empty.text = "No cheats yet. Click \"Add cheat\" and paste a code."
	_empty.modulate = Color(1, 1, 1, 0.6)

	add_child(box)


func _add_cheat() -> void:
	_cheats.append({"name": "", "code": "", "on": true})
	_rebuild()
	# put the cursor in the new row's code box
	var row := _list.get_child(_list.get_child_count() - 1)
	if row and row.get_child_count() > 2:
		row.get_child(2).grab_focus()


## Call after a game starts. Loads that game's saved cheats and switches on the enabled ones.
func game_loaded(emu: Variant, mutex: Mutex, system_id: String, game_key: String) -> void:
	_emu = emu
	_mutex = mutex
	_system_id = system_id
	_game_key = game_key.validate_filename()
	_mutex.lock()
	_supported = _emu.supports_cheats()
	_mutex.unlock()
	_load()
	if _supported and not _cheats.is_empty():
		_apply()


func open_dialog() -> void:
	if _emu == null:
		return
	title = "Cheats  —  %s" % _game_key
	_help.text = HELP.get(_system_id, "") if _supported \
		else "Cheats aren't supported for %s yet." % AppSettings.system_name(_system_id)
	_rebuild()
	popup_centered(min_size)
	_help.tooltip_text = _help.text


func active_count() -> int:
	return _cheats.filter(func(c): return c["on"] and c["code"].strip_edges() != "").size()


# ---------------- list ----------------

func _rebuild() -> void:
	for c in _list.get_children():
		_list.remove_child(c)
		c.queue_free()
	if _cheats.is_empty():
		_list.add_child(_empty.duplicate())
	for i in _cheats.size():
		_list.add_child(_make_row(i))


func _make_row(i: int) -> HBoxContainer:
	var c: Dictionary = _cheats[i]
	var row := HBoxContainer.new()

	var on := CheckBox.new()
	on.button_pressed = c["on"]
	on.disabled = not _supported
	on.toggled.connect(func(v): c["on"] = v; _changed())
	row.add_child(on)

	var name_edit := LineEdit.new()
	name_edit.placeholder_text = "Name (e.g. Infinite lives)"
	name_edit.text = c["name"]
	name_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	name_edit.text_changed.connect(func(t): c["name"] = t; _save())
	row.add_child(name_edit)

	var code_edit := LineEdit.new()
	code_edit.placeholder_text = "Code"
	code_edit.text = c["code"]
	code_edit.custom_minimum_size.x = 230
	code_edit.text_submitted.connect(func(t): c["code"] = t.strip_edges(); _changed())
	code_edit.focus_exited.connect(func():
		if code_edit.text.strip_edges() != c["code"]:
			c["code"] = code_edit.text.strip_edges()
			_changed())
	row.add_child(code_edit)

	var status := Label.new()
	status.custom_minimum_size.x = 24
	var err := _error_for(i)
	if c["code"] == "" or not _supported:
		status.text = ""
	elif err == "":
		status.text = "✓"
		status.modulate = Color(0.5, 1.0, 0.5)
	else:
		status.text = "✗"
		status.modulate = Color(1.0, 0.45, 0.45)
		status.tooltip_text = err
		status.mouse_filter = Control.MOUSE_FILTER_STOP
		code_edit.tooltip_text = err
	row.add_child(status)

	var del := Button.new()
	del.text = "Delete"
	del.pressed.connect(func(): _cheats.remove_at(i); _changed())
	row.add_child(del)
	return row


func _error_for(i: int) -> String:
	# _errors lines up with the codes sent in _apply(): only "on" cheats with a code
	var sent := -1
	for j in i + 1:
		if _cheats[j]["on"] and _cheats[j]["code"] != "":
			sent += 1
	if not (_cheats[i]["on"] and _cheats[i]["code"] != ""):
		return _check_only(_cheats[i]["code"])
	return _errors[sent] if sent >= 0 and sent < _errors.size() else ""


## Disabled cheats aren't sent to the core, so check their syntax on their own.
func _check_only(code: String) -> String:
	if code == "" or not _supported:
		return ""
	_mutex.lock()
	var errs: PackedStringArray = _emu.set_cheats(PackedStringArray([code]))
	_mutex.unlock()
	_apply()   # put the real set back
	return errs[0] if errs.size() > 0 else ""


func _changed() -> void:
	_apply()
	_save()
	_rebuild()


# ---------------- core + disk ----------------

func _apply() -> void:
	if _emu == null or not _supported:
		return
	var codes := PackedStringArray()
	for c in _cheats:
		if c["on"] and c["code"] != "":
			codes.append(c["code"])
	_mutex.lock()
	_errors = _emu.set_cheats(codes)
	_mutex.unlock()


func _path() -> String:
	return "%s/%s/%s.json" % [ROOT, _system_id, _game_key]


func _load() -> void:
	_cheats = []
	_errors = PackedStringArray()
	if not FileAccess.file_exists(_path()):
		return
	var parsed = JSON.parse_string(FileAccess.get_file_as_string(_path()))
	if parsed is Array:
		for c in parsed:
			if c is Dictionary:
				_cheats.append({"name": str(c.get("name", "")), "code": str(c.get("code", "")),
					"on": bool(c.get("on", true))})


func _save() -> void:
	if _game_key == "":
		return
	DirAccess.make_dir_recursive_absolute(_path().get_base_dir())
	var f := FileAccess.open(_path(), FileAccess.WRITE)
	if f:
		f.store_string(JSON.stringify(_cheats, "\t"))
