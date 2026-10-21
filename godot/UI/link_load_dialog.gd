class_name LinkLoadDialog
extends ConfirmationDialog
## File > Link Cable: pick a game for each Game Boy (.gb / .gbc, or a zip with one inside).
## The same game twice is fine (e.g. two copies of Red): the second one saves to <name>_p2.sav.

signal chosen(path_a: String, path_b: String)

var _edits: Array[LineEdit] = []
var _file_dialog := FileDialog.new()
var _browsing := 0


func _init() -> void:
	title = "Link Cable: 2 Game Boys"
	ok_button_text = "Start"
	min_size = Vector2i(640, 200)
	var grid := GridContainer.new()
	grid.columns = 3
	grid.add_theme_constant_override("h_separation", 10)
	grid.add_theme_constant_override("v_separation", 10)
	for i in 2:
		var l := Label.new()
		l.text = "Game Boy %d (%s):" % [i + 1, ["left", "right"][i]]
		grid.add_child(l)
		var e := LineEdit.new()
		e.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		e.custom_minimum_size.x = 380
		e.placeholder_text = "e.g. Pokemon Red.gb"
		grid.add_child(e)
		_edits.append(e)
		var b := Button.new()
		b.text = "Browse..."
		b.pressed.connect(_browse.bind(i))
		grid.add_child(b)
	add_child(grid)

	_file_dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	_file_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_file_dialog.filters = PackedStringArray(["*.gb, *.gbc, *.zip ; Game Boy games"])
	_file_dialog.file_selected.connect(func(p): _edits[_browsing].text = p)
	add_child(_file_dialog)
	confirmed.connect(_on_start)


func open() -> void:
	_edits[0].text = AppSettings.link_a
	_edits[1].text = AppSettings.link_b
	popup_centered(min_size)


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
	AppSettings.save_settings()
	chosen.emit(a, b)
