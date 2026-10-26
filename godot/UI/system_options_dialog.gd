class_name SystemOptionsDialog
extends AcceptDialog
## Settings > System Options. Per system: picture shape, smoothing, overscan crop, BIOS.
## Picture changes show up live; BIOS changes apply the next time a game loads.

signal options_changed

var _system_pick := OptionButton.new()
var _form := GridContainer.new()
var _system_ids: Array = []
var _bios_dialog := FileDialog.new()
var _bios_edit: LineEdit = null


func _init() -> void:
	title = "System Options"
	ok_button_text = "Done"
	min_size = Vector2i(560, 420)

	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 12)
	var top := HBoxContainer.new()
	var lbl := Label.new()
	lbl.text = "System:"
	top.add_child(lbl)
	_system_pick.custom_minimum_size.x = 220
	top.add_child(_system_pick)
	var spacer := Control.new()
	spacer.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	top.add_child(spacer)
	var reset := Button.new()
	reset.text = "Reset this system"
	reset.pressed.connect(func():
		AppSettings.reset_options(_system())
		_changed()
		_rebuild())
	top.add_child(reset)
	box.add_child(top)

	_form.columns = 2
	_form.add_theme_constant_override("h_separation", 16)
	_form.add_theme_constant_override("v_separation", 10)
	box.add_child(_form)
	add_child(box)

	_bios_dialog.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	_bios_dialog.access = FileDialog.ACCESS_FILESYSTEM
	_bios_dialog.title = "Choose BIOS file"
	_bios_dialog.file_selected.connect(func(p):
		_put("bios", p)
		if _bios_edit: _bios_edit.text = p)
	add_child(_bios_dialog)

	_system_pick.item_selected.connect(func(_i): _rebuild())
	confirmed.connect(_finish)
	canceled.connect(_finish)


func _ready() -> void:
	for id in AppSettings.SYSTEMS:
		_system_ids.append(id)
		_system_pick.add_item(AppSettings.SYSTEMS[id]["name"])


func open_for(system_id: String) -> void:
	_system_pick.select(max(_system_ids.find(system_id), 0))
	_rebuild()
	popup_centered()


func _system() -> String:
	return _system_ids[_system_pick.selected]


func _opt(key: String) -> Variant:
	return AppSettings.get_option(_system(), key)


func _put(key: String, value: Variant) -> void:
	AppSettings.set_option(_system(), key, value)
	_changed()


func _changed() -> void:
	options_changed.emit()


func _rebuild() -> void:
	_bios_edit = null
	for c in _form.get_children():
		_form.remove_child(c)
		c.queue_free()
	var sys := _system()

	# Picture shape
	var aspect := OptionButton.new()
	aspect.add_item("Console (TV shape)")
	aspect.add_item("Square pixels")
	aspect.select(1 if _opt("aspect") == "square" else 0)
	aspect.item_selected.connect(func(i): _put("aspect", "square" if i == 1 else "console"))
	_row("Picture shape", aspect)

	# Smoothing
	var smooth := CheckBox.new()
	smooth.text = "Smooth pixels (bilinear)"
	smooth.button_pressed = _opt("smooth")
	smooth.toggled.connect(func(on): _put("smooth", on))
	_row("Filter", smooth)

	# Light gun (NES Zapper in port 2, played with the mouse)
	if sys == "nes":
		var zap := CheckBox.new()
		zap.text = "Zapper in port 2 (aim with the mouse, left click fires, right click fires off screen)"
		zap.button_pressed = _opt("zapper")
		zap.toggled.connect(func(on): _put("zapper", on))
		_row("Light gun", zap)

	# Overscan crop
	var crop := HBoxContainer.new()
	for side in [["crop_top", "Top"], ["crop_bottom", "Bottom"], ["crop_left", "Left"], ["crop_right", "Right"]]:
		var l := Label.new()
		l.text = side[1]
		crop.add_child(l)
		var spin := SpinBox.new()
		spin.min_value = 0
		spin.max_value = 32
		spin.value = _opt(side[0])
		spin.suffix = "px"
		var key: String = side[0]
		spin.value_changed.connect(func(v): _put(key, int(v)))
		crop.add_child(spin)
	_row("Crop edges", crop)

	# BIOS (only for systems that use one)
	if AppSettings.BIOS_DEFAULTS.has(sys):
		var bios := HBoxContainer.new()
		_bios_edit = LineEdit.new()
		_bios_edit.text = _opt("bios")
		_bios_edit.placeholder_text = AppSettings.BIOS_DEFAULTS[sys]
		_bios_edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		_bios_edit.custom_minimum_size.x = 260
		_bios_edit.text_submitted.connect(func(t): _put("bios", t))
		_bios_edit.focus_exited.connect(func(): if _bios_edit: _put("bios", _bios_edit.text))
		bios.add_child(_bios_edit)
		var browse := Button.new()
		browse.text = "Browse…"
		browse.pressed.connect(func(): _bios_dialog.popup_centered_ratio(0.6))
		bios.add_child(browse)
		_row("BIOS file", bios)
		if sys in AppSettings.BIOS_OPTIONAL:
			var skip := CheckBox.new()
			skip.text = "Skip boot ROM (start the game directly)"
			skip.button_pressed = _opt("skip_bios")
			skip.toggled.connect(func(on): _put("skip_bios", on))
			_row("", skip)
		var note := Label.new()
		note.text = "BIOS changes apply the next time a game loads."
		note.modulate = Color(1, 1, 1, 0.6)
		_row("", note)


func _row(label: String, control: Control) -> void:
	var l := Label.new()
	l.text = label
	_form.add_child(l)
	_form.add_child(control)


func _finish() -> void:
	if _bios_edit:
		_put("bios", _bios_edit.text)
	AppSettings.save_settings()
