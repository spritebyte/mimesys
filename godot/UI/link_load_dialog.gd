class_name LinkLoadDialog
extends ConfirmationDialog
## File > Link Cable: two Game Boys or two GBAs side by side. Pick the system, then a game
## for each (or a zip with one inside). The same game twice is fine (e.g. two copies of Red):
## the second one saves to <name>_p2.sav. For the GBA, player 1 (left) is the multiplayer parent.

signal chosen(system_id: String, path_a: String, path_b: String)

const SYSTEMS := [
	["gb", "Game Boy / Color", "*.gb, *.gbc, *.zip ; Game Boy games", "e.g. Pokemon Red.gb"],
	["gba", "Game Boy Advance", "*.gba, *.agb, *.zip ; GBA games", "e.g. Pokemon Emerald.gba"],
]

var _system := OptionButton.new()
var _trace_box: CheckBox
var _labels: Array[Label] = []
var _edits: Array[LineEdit] = []
var _file_dialog := FileDialog.new()
var _browsing := 0


func _init() -> void:
	title = "Link Cable: 2 players"
	ok_button_text = "Start"
	min_size = Vector2i(640, 230)
	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 12)
	var top := HBoxContainer.new()
	var sl := Label.new()
	sl.text = "System:"
	top.add_child(sl)
	for sysdef in SYSTEMS:
		_system.add_item(sysdef[1])
	_system.item_selected.connect(func(_i): _apply_system())
	top.add_child(_system)
	box.add_child(top)
	var grid := GridContainer.new()
	grid.columns = 3
	grid.add_theme_constant_override("h_separation", 10)
	grid.add_theme_constant_override("v_separation", 10)
	for i in 2:
		var l := Label.new()
		grid.add_child(l)
		_labels.append(l)
		var e := LineEdit.new()
		e.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		e.custom_minimum_size.x = 380
		grid.add_child(e)
		_edits.append(e)
		var b := Button.new()
		b.text = "Browse..."
		b.pressed.connect(_browse.bind(i))
		grid.add_child(b)
	box.add_child(grid)
	var trace := CheckBox.new()
	trace.text = "Record a link log (for debugging, GBA): user://link_trace.txt"
	trace.button_pressed = AppSettings.link_trace
	trace.toggled.connect(func(on): AppSettings.link_trace = on; AppSettings.save_settings())
	_trace_box = trace
	box.add_child(trace)
	add_child(box)

	_file_dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	_file_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_file_dialog.file_selected.connect(_on_file_picked)
	add_child(_file_dialog)
	confirmed.connect(_on_start)


func open() -> void:
	_edits[0].text = AppSettings.link_a
	_edits[1].text = AppSettings.link_b
	var idx := 0
	for i in SYSTEMS.size():
		if SYSTEMS[i][0] == AppSettings.link_system:
			idx = i
	_system.select(idx)
	_apply_system()
	_trace_box.button_pressed = AppSettings.link_trace
	popup_centered(min_size)


func _system_id() -> String:
	return SYSTEMS[_system.selected][0]


## Labels, placeholder and file filter for the chosen system.
func _apply_system() -> void:
	var sysdef: Array = SYSTEMS[_system.selected]
	var unit := "GBA" if sysdef[0] == "gba" else "Game Boy"
	for i in 2:
		_labels[i].text = "%s %d (%s):" % [unit, i + 1, ["left", "right"][i]]
		_edits[i].placeholder_text = sysdef[3]
	_file_dialog.filters = PackedStringArray([sysdef[2]])


## A picked .gba switches the system to GBA (and .gb/.gbc to Game Boy).
func _on_file_picked(p: String) -> void:
	_edits[_browsing].text = p
	var ext := p.get_extension().to_lower()
	for i in SYSTEMS.size():
		if (ext in ["gba", "agb"] and SYSTEMS[i][0] == "gba") or (ext in ["gb", "gbc"] and SYSTEMS[i][0] == "gb"):
			if _system.selected != i:
				_system.select(i)
				_apply_system()


func _browse(i: int) -> void:
	_browsing = i
	if _edits[i].text != "":
		_file_dialog.current_dir = _edits[i].text.get_base_dir()
	_file_dialog.popup_centered_ratio(0.6)


func _on_start() -> void:
	var a := _edits[0].text.strip_edges()
	var b := _edits[1].text.strip_edges()
	if a == "" or b == "":
		return
	# remember the pair for next time
	AppSettings.link_a = a
	AppSettings.link_b = b
	AppSettings.link_system = _system_id()
	AppSettings.save_settings()
	chosen.emit(_system_id(), a, b)
