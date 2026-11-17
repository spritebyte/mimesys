class_name MemoryCardDialog
extends AcceptDialog
## Emulation > Memory Cards (Dreamcast and PlayStation). Each card is a 128 KB file:
## - Dreamcast: user://GD_EMU/DC/VMU/<name>.bin, the raw VMU image (option "dc_card").
## - PlayStation: user://GD_EMU/PS1/MemoryCards/<name>.mcd (also .mcr / .bin / .mem: the
##   raw card image other emulators use), slot 1 (option "ps1_card").
## A dump dropped in the folder shows up. Swapping while a game runs pulls the old card out
## for a moment, as on the console.
##
## main.gd: game_loaded(...) after a game starts, save_if_dirty() on the save timer,
## game_stopped() before power off, open_dialog() from the menu.

const CARD_SIZE := 131072
## per system: folder, option, new-card extension, extensions listed, name
const SYSTEMS := {
	"dc": {"root": "user://GD_EMU/DC/VMU", "option": "dc_card", "ext": "bin", "exts": ["bin"],
		"name": "Dreamcast", "slot": "controller 1's slot"},
	"ps1": {"root": "user://GD_EMU/PS1/MemoryCards", "option": "ps1_card", "ext": "mcd",
		"exts": ["mcd", "mcr", "bin", "mem"], "name": "PlayStation", "slot": "slot 1"},
}

var _emu: Variant = null           # the EmuSystem, while a game with cards runs
var _mutex: Mutex = null
var _system := "dc"                # whose cards the dialog shows
var _names: PackedStringArray = []
var _files := {}                   # card name -> file name
var _which := OptionButton.new()

var _slot := Label.new()
var _list := ItemList.new()
var _status := Label.new()
var _new_name := LineEdit.new()
var _insert := Button.new()
var _delete := Button.new()
var _confirm := ConfirmationDialog.new()


func _init() -> void:
	title = "Memory Cards"
	ok_button_text = "Done"
	min_size = Vector2i(640, 420)

	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 10)
	for id in SYSTEMS:
		_which.add_item(SYSTEMS[id]["name"])
		_which.set_item_metadata(_which.item_count - 1, id)
	_which.item_selected.connect(func(i):
		_system = _which.get_item_metadata(i)
		_refresh())
	box.add_child(_which)
	box.add_child(_slot)

	_list.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_list.custom_minimum_size.y = 220
	_list.item_selected.connect(func(_i): _update_buttons())
	_list.item_activated.connect(func(_i): _put_in_slot())
	box.add_child(_list)

	var row := HBoxContainer.new()
	_insert.text = "Put in slot"
	_insert.pressed.connect(_put_in_slot)
	row.add_child(_insert)
	_delete.text = "Delete…"
	_delete.pressed.connect(func():
		var n := _selected()
		if n == "":
			return
		_confirm.dialog_text = "Delete memory card \"%s\" and every save on it?" % n
		_confirm.popup_centered())
	row.add_child(_delete)
	var folder := Button.new()
	folder.text = "Open folder"
	folder.tooltip_text = "Card files are raw 128 KB card images: copy one in to use it"
	folder.pressed.connect(func():
		DirAccess.make_dir_recursive_absolute(_root())
		OS.shell_open(ProjectSettings.globalize_path(_root())))
	row.add_child(folder)
	box.add_child(row)

	var make := HBoxContainer.new()
	_new_name.placeholder_text = "New card name"
	_new_name.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_new_name.text_submitted.connect(func(_t): _create())
	make.add_child(_new_name)
	var create := Button.new()
	create.text = "+ New blank card"
	create.pressed.connect(_create)
	make.add_child(create)
	box.add_child(make)

	_status.modulate = Color(1, 1, 1, 0.75)
	box.add_child(_status)
	add_child(box)

	_confirm.confirmed.connect(_delete_selected)
	add_child(_confirm)


func _root() -> String:
	return SYSTEMS[_system]["root"]


func _path(card: String) -> String:
	var f: String = _files.get(card, "%s.%s" % [card, SYSTEMS[_system]["ext"]])
	return "%s/%s" % [_root(), f]


func _list_files() -> void:
	_files = {}
	for f in DirAccess.get_files_at(_root()):
		if f.get_extension().to_lower() in SYSTEMS[_system]["exts"] and not _files.has(f.get_basename()):
			_files[f.get_basename()] = f


func _current() -> String:
	return str(AppSettings.get_option(_system, SYSTEMS[_system]["option"]))


func _selected() -> String:
	var sel := _list.get_selected_items()
	return _names[sel[0]] if sel.size() > 0 else ""


## Call after a game starts. A Dreamcast or PlayStation game gets the chosen card; the
## first time, the game's own card (a fresh one) becomes that card's file.
func game_loaded(emu: Variant, mutex: Mutex, system_id: String) -> void:
	_emu = null
	if not SYSTEMS.has(system_id):
		return
	_system = system_id
	_emu = emu
	_mutex = mutex
	DirAccess.make_dir_recursive_absolute(_root())
	_list_files()
	var path := _path(_current())
	if FileAccess.file_exists(path):
		var data := FileAccess.get_file_as_bytes(path)
		_mutex.lock()
		_emu.insert_memory_card(data, false)
		_mutex.unlock()
	else:
		if system_id == "ps1":
			# a freshly formatted card (the Dreamcast keeps the one from its save file)
			_mutex.lock()
			_emu.insert_memory_card(PackedByteArray(), false)
			_mutex.unlock()
		_write_current()


## Write the card in the slot to its file if the game changed it.
func save_if_dirty() -> void:
	if _emu == null:
		return
	_mutex.lock()
	var dirty: bool = _emu.memory_card_dirty()
	_mutex.unlock()
	if dirty:
		_write_current()


func game_stopped() -> void:
	save_if_dirty()
	_emu = null


func open_dialog() -> void:
	save_if_dirty()
	for i in _which.item_count:
		if _which.get_item_metadata(i) == _system:
			_which.select(i)
	_which.disabled = _emu != null      # a running game's cards only
	_refresh()
	popup_centered()


## The card in the slot, from the running game, to its file.
func _write_current() -> void:
	if _emu == null:
		return
	_mutex.lock()
	var data: PackedByteArray = _emu.get_memory_card()
	_mutex.unlock()
	if data.size() != CARD_SIZE:
		return
	var f := FileAccess.open(_path(_current()), FileAccess.WRITE)
	if f:
		f.store_buffer(data)
		f.close()


func _refresh() -> void:
	DirAccess.make_dir_recursive_absolute(_root())
	_list_files()
	_names = PackedStringArray(_files.keys())
	_names.sort()
	var cur := _current()
	var sysname: String = SYSTEMS[_system]["name"]
	_slot.text = "In %s: %s%s" % [SYSTEMS[_system]["slot"], cur, "" if _emu else "   (used when a %s game starts)" % sysname]
	_list.clear()
	for n in _names:
		var info: PackedStringArray = EmuSystem.card_info(_system, FileAccess.get_file_as_bytes(_path(n)))
		var text := n
		if info.is_empty():
			text += "   (not a 128 KB card image)"
		else:
			var saves := info.slice(1)
			text += "   %s blocks free" % info[0]
			if saves.size() > 0:
				text += "  ·  " + ", ".join(saves)
		if n == cur:
			text = "▶ " + text
		_list.add_item(text)
		if n == cur:
			_list.select(_list.item_count - 1)
	_update_buttons()


func _update_buttons() -> void:
	var n := _selected()
	_insert.disabled = n == "" or n == _current()
	_delete.disabled = n == "" or n == _current()


func _put_in_slot() -> void:
	var n := _selected()
	if n == "" or n == _current():
		return
	var data := FileAccess.get_file_as_bytes(_path(n))
	if data.size() != CARD_SIZE:
		_status.text = "\"%s\" isn't a 128 KB card image." % n
		return
	# the old card's latest contents first
	_write_current()
	AppSettings.set_option(_system, SYSTEMS[_system]["option"], n)
	AppSettings.save_settings()
	if _emu:
		_mutex.lock()
		_emu.insert_memory_card(data, true)
		_mutex.unlock()
		_status.text = "Swapped in \"%s\"." % n
	else:
		_status.text = "\"%s\" goes in the slot when a %s game starts." % [n, SYSTEMS[_system]["name"]]
	_refresh()


func _create() -> void:
	var n := _new_name.text.strip_edges().validate_filename()
	if n == "":
		n = "Card %d" % (_names.size() + 1)
	if FileAccess.file_exists(_path(n)):
		_status.text = "There's already a card called \"%s\"." % n
		return
	DirAccess.make_dir_recursive_absolute(_root())
	var data := PackedByteArray()
	data.resize(CARD_SIZE)   # all zero: formatted the first time it's used
	var f := FileAccess.open(_path(n), FileAccess.WRITE)
	if f == null:
		_status.text = "Couldn't create \"%s\"." % n
		return
	f.store_buffer(data)
	f.close()
	_new_name.text = ""
	_status.text = "Made \"%s\". Select it and click Put in slot." % n
	_refresh()
	var i := _names.find(n)
	if i >= 0:
		_list.select(i)
		_update_buttons()


func _delete_selected() -> void:
	var n := _selected()
	if n == "" or n == _current():
		return
	DirAccess.remove_absolute(_path(n))
	_status.text = "Deleted \"%s\"." % n
	_refresh()
