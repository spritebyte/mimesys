class_name LibraryDialog
extends AcceptDialog
## File > Game Library. Two tabs:
##   Games    filter by system, search, double-click (or Play) to start
##   Folders  which folders are scanned (subfolders included)
## The scan result is cached in user://GD_EMU/library.json; "Rescan" refreshes it.
##
## What counts as a game:
##   a file with a known console extension (.nes .gb .pce ...)
##   a zip of console ROMs: one entry per ROM inside
##   any other multi-file zip: an arcade set (named from arcade_games.json when listed)

signal game_chosen(path: String, inner: String)

const CACHE_PATH := "user://library.json"
const SKIP_EXT := ["txt", "md", "nfo", "htm", "html", "jpg", "png", "pdf"]

var _games: Array = []      # {"title", "system", "path", "inner"}
var _filter := OptionButton.new()
var _search := LineEdit.new()
var _tree := Tree.new()
var _count := Label.new()
var _folders := ItemList.new()
var _folder_dialog := FileDialog.new()
var _filter_ids: Array = []


func _init() -> void:
	title = "Game Library"
	ok_button_text = "Play"
	min_size = Vector2i(820, 560)

	var tabs := TabContainer.new()
	tabs.size_flags_vertical = Control.SIZE_EXPAND_FILL

	# --- Games tab ---
	var games := VBoxContainer.new()
	games.name = "Games"
	var bar := HBoxContainer.new()
	_filter.custom_minimum_size.x = 200
	bar.add_child(_filter)
	_search.placeholder_text = "Search…"
	_search.clear_button_enabled = true
	_search.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	bar.add_child(_search)
	var rescan := Button.new()
	rescan.text = "Rescan"
	rescan.pressed.connect(rescan_now)
	bar.add_child(rescan)
	games.add_child(bar)
	_tree.columns = 2
	_tree.set_column_titles_visible(true)
	_tree.set_column_title(0, "Title")
	_tree.set_column_title(1, "System")
	_tree.set_column_expand(1, false)
	_tree.set_column_custom_minimum_width(1, 170)
	_tree.hide_root = true
	_tree.select_mode = Tree.SELECT_ROW
	_tree.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_tree.item_activated.connect(_play_selected)
	games.add_child(_tree)
	_count.modulate = Color(1, 1, 1, 0.7)
	games.add_child(_count)
	tabs.add_child(games)

	# --- Folders tab ---
	var folders := VBoxContainer.new()
	folders.name = "Folders"
	var fbar := HBoxContainer.new()
	var add := Button.new()
	add.text = "Add folder…"
	add.pressed.connect(func(): _folder_dialog.popup_centered_ratio(0.6))
	fbar.add_child(add)
	var remove := Button.new()
	remove.text = "Remove selected"
	remove.pressed.connect(_remove_folder)
	fbar.add_child(remove)
	folders.add_child(fbar)
	_folders.size_flags_vertical = Control.SIZE_EXPAND_FILL
	folders.add_child(_folders)
	var hint := Label.new()
	hint.text = "Subfolders are scanned too. Arcade parent sets (e.g. pacman.zip for mspacman) can live in the same folder."
	hint.autowrap_mode = TextServer.AUTOWRAP_WORD_SMART
	hint.modulate = Color(1, 1, 1, 0.7)
	folders.add_child(hint)
	tabs.add_child(folders)
	add_child(tabs)

	_folder_dialog.file_mode = FileDialog.FILE_MODE_OPEN_DIR
	_folder_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_folder_dialog.title = "Choose a ROM folder"
	_folder_dialog.dir_selected.connect(_add_folder)
	add_child(_folder_dialog)

	_filter.item_selected.connect(func(_i): _refresh_list())
	_search.text_changed.connect(func(_t): _refresh_list())
	confirmed.connect(_play_selected)


func open() -> void:
	_refresh_folders()
	if _games.is_empty():
		_load_cache()
	if _games.is_empty() and not AppSettings.rom_folders.is_empty():
		rescan_now()
	_refresh_filter()
	_refresh_list()
	popup_centered()
	_search.grab_focus()


# ---------------- folders ----------------

func _refresh_folders() -> void:
	_folders.clear()
	for f in AppSettings.rom_folders:
		_folders.add_item(f)


func _add_folder(dir: String) -> void:
	if dir in AppSettings.rom_folders:
		return
	AppSettings.rom_folders.append(dir)
	AppSettings.save_settings()
	_refresh_folders()
	rescan_now()


func _remove_folder() -> void:
	for i in _folders.get_selected_items():
		AppSettings.rom_folders.erase(_folders.get_item_text(i))
	AppSettings.save_settings()
	_refresh_folders()
	rescan_now()


# ---------------- scanning ----------------

func rescan_now() -> void:
	_games.clear()
	var arcade_db: Dictionary = RomLoader._load_arcade_db()
	for folder in AppSettings.rom_folders:
		_scan_dir(folder, arcade_db)
	_games.sort_custom(func(a, b): return a["title"].naturalnocasecmp_to(b["title"]) < 0)
	_save_cache()
	_refresh_filter()
	_refresh_list()


func _scan_dir(dir: String, arcade_db: Dictionary) -> void:
	var d := DirAccess.open(dir)
	if d == null:
		return
	for sub in d.get_directories():
		if not sub.begins_with("."):
			_scan_dir(dir.path_join(sub), arcade_db)
	for f in d.get_files():
		var path := dir.path_join(f)
		var ext := f.get_extension().to_lower()
		if ext == "zip":
			_scan_zip(path, arcade_db)
		elif AppSettings.SYSTEM_BY_EXT.has(ext):
			_add(f.get_basename(), AppSettings.SYSTEM_BY_EXT[ext], path, "")


func _scan_zip(path: String, arcade_db: Dictionary) -> void:
	var zip := ZIPReader.new()
	if zip.open(path) != OK:
		return
	var files: Array = []
	for f in zip.get_files():
		if f.ends_with("/") or f.begins_with("__MACOSX") or f.get_extension().to_lower() in SKIP_EXT:
			continue
		files.append(f)
	zip.close()
	if files.is_empty():
		return
	var all_console := files.all(func(f): return AppSettings.SYSTEM_BY_EXT.has(f.get_extension().to_lower()))
	if all_console:
		for f in files:
			var ext: String = f.get_extension().to_lower()
			# one file: load the zip directly; several: point at the file inside
			_add(f.get_file().get_basename(), AppSettings.SYSTEM_BY_EXT[ext], path,
				"" if files.size() == 1 else f)
		return
	var id := path.get_file().get_basename().to_lower()
	var title: String = arcade_db.get(id, {}).get("title", path.get_file().get_basename())
	_add(title, "arcade", path, "")


func _add(title: String, system_id: String, path: String, inner: String) -> void:
	_games.append({"title": title, "system": system_id, "path": path, "inner": inner})


func _save_cache() -> void:
	DirAccess.make_dir_recursive_absolute(CACHE_PATH.get_base_dir())
	var f := FileAccess.open(CACHE_PATH, FileAccess.WRITE)
	if f:
		f.store_string(JSON.stringify(_games))


func _load_cache() -> void:
	if not FileAccess.file_exists(CACHE_PATH):
		return
	var parsed = JSON.parse_string(FileAccess.get_file_as_string(CACHE_PATH))
	_games = parsed if parsed is Array else []


# ---------------- list ----------------

func _refresh_filter() -> void:
	var keep: String = _filter_ids[_filter.selected] if _filter.selected >= 0 and _filter.selected < _filter_ids.size() else ""
	var counts := {}
	for g in _games:
		counts[g["system"]] = counts.get(g["system"], 0) + 1
	_filter.clear()
	_filter_ids = [""]
	_filter.add_item("All systems (%d)" % _games.size())
	for id in AppSettings.SYSTEMS:
		if counts.has(id):
			_filter_ids.append(id)
			_filter.add_item("%s (%d)" % [AppSettings.system_name(id), counts[id]])
	_filter.select(max(_filter_ids.find(keep), 0))


func _refresh_list() -> void:
	_tree.clear()
	var root := _tree.create_item()
	var want: String = _filter_ids[_filter.selected] if _filter.selected >= 0 and _filter.selected < _filter_ids.size() else ""
	var words := _search.text.to_lower().split(" ", false)
	var shown := 0
	for g in _games:
		if want != "" and g["system"] != want:
			continue
		var t: String = g["title"].to_lower()
		if not words.is_empty() and not Array(words).all(func(w): return t.contains(w)):
			continue
		var item := _tree.create_item(root)
		item.set_text(0, g["title"])
		item.set_text(1, AppSettings.system_name(g["system"]))
		item.set_tooltip_text(0, g["path"] + ("  ›  " + g["inner"] if g["inner"] != "" else ""))
		item.set_metadata(0, g)
		shown += 1
	if AppSettings.rom_folders.is_empty():
		_count.text = "No folders yet: add one on the Folders tab."
	else:
		_count.text = "%d games shown" % shown
	var first := root.get_first_child()
	if first:
		first.select(0)


func _play_selected() -> void:
	var item := _tree.get_selected()
	if item == null:
		return
	var g: Dictionary = item.get_metadata(0)
	hide()
	game_chosen.emit(g["path"], g["inner"])
