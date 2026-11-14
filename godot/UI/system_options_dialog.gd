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

	# Picture filter
	var filters := [["sharp", "Sharp pixels"], ["smooth", "Smooth (bilinear)"],
		["sharp_smooth", "Sharp-smooth (even pixels at any size)"], ["scanlines", "Scanlines"],
		["crt", "CRT (curve, scanlines, shadow mask)"], ["lcd", "LCD grid (handhelds)"]]
	var filter := OptionButton.new()
	for f in filters:
		filter.add_item(f[1])
	var cur := AppSettings.filter_of(sys)
	for i in filters.size():
		if filters[i][0] == cur:
			filter.select(i)
	_row("Filter", filter)
	var strength := HSlider.new()
	strength.min_value = 0
	strength.max_value = 100
	strength.step = 5
	strength.value = _opt("filter_strength")
	strength.custom_minimum_size = Vector2(200, 0)
	strength.tooltip_text = "How strong the scanlines, CRT mask or LCD grid are"
	strength.editable = cur in ["scanlines", "crt", "lcd"]
	strength.value_changed.connect(func(v): _put("filter_strength", int(v)))
	_row("Filter strength", strength)
	filter.item_selected.connect(func(i):
		_put("filter", filters[i][0])
		strength.editable = filters[i][0] in ["scanlines", "crt", "lcd"]
	)

	# Flicker blend (sprites shown on every other frame look solid and see-through, as on a CRT)
	var flicker := OptionButton.new()
	flicker.add_item("Off")
	flicker.add_item("Blend (best for every-other-frame flicker, e.g. Batman)")
	flicker.add_item("Phosphor (CRT glow: bright pixels fade out)")
	flicker.select(int(_opt("flicker")))
	flicker.item_selected.connect(func(i): _put("flicker", i))
	_row("Flicker blend", flicker)

	# Light gun (NES Zapper in port 2, played with the mouse)
	if sys == "nes":
		var nbg := CheckBox.new()
		nbg.text = "Write a debug log of the mapper (user://debug_nes.log; for bug reports; applies when a game loads)"
		nbg.button_pressed = _opt("nes_debug")
		nbg.toggled.connect(func(on): _put("nes_debug", on))
		_row("Debug", nbg)
		var zap := CheckBox.new()
		zap.text = "Zapper in port 2 (aim with the mouse, left click fires, right click fires off screen)"
		zap.button_pressed = _opt("zapper")
		zap.toggled.connect(func(on): _put("zapper", on))
		_row("Light gun", zap)

		# VS System (arcade) cartridges
		var ppu := OptionButton.new()
		ppu.add_item("From the cartridge (NES 2.0 header or the game's name)")
		for n in ["RP2C03B", "RP2C03G", "RP2C04-0001", "RP2C04-0002", "RP2C04-0003", "RP2C04-0004",
				"RC2C03B", "RC2C03C", "RC2C05-01", "RC2C05-02", "RC2C05-03", "RC2C05-04", "RC2C05-05"]:
			ppu.add_item(n)
		ppu.select(int(_opt("vs_ppu")) + 1)
		ppu.tooltip_text = "VS games were made for one PPU. Wrong colours or a black screen: try another"
		ppu.item_selected.connect(func(i): _put("vs_ppu", i - 1))
		_row("VS System PPU", ppu)
		var swap := CheckBox.new()
		swap.text = "Player 1 on the left stick (some VS games expect it)"
		swap.button_pressed = _opt("vs_swap")
		swap.toggled.connect(func(on): _put("vs_swap", on))
		_row("VS sticks", swap)
		var dips := HBoxContainer.new()
		for k in 8:
			var d := CheckBox.new()
			d.text = str(k + 1)
			d.button_pressed = int(_opt("vs_dip")) & (1 << k) != 0
			d.toggled.connect(func(on):
				var v := int(_opt("vs_dip"))
				_put("vs_dip", v | (1 << k) if on else v & ~(1 << k))
			)
			dips.add_child(d)
		dips.tooltip_text = "Coins per credit, lives, difficulty... depends on the game (all off is the usual default)"
		_row("VS DIP switches", dips)

	# Lynx: screen rotation (for dumps whose header doesn't say)
	if sys == "lynx":
		var rot := OptionButton.new()
		for item in ["From the cartridge", "None", "Left (top of the picture on the left)", "Right (top on the right)"]:
			rot.add_item(item)
		rot.select(int(_opt("lynx_rotate")))
		rot.item_selected.connect(func(i): _put("lynx_rotate", i))
		_row("Screen rotation", rot)
		var ldbg := CheckBox.new()
		ldbg.text = "Print the sound registers every 10 seconds (for bug reports)"
		ldbg.button_pressed = _opt("lynx_debug")
		ldbg.toggled.connect(func(on): _put("lynx_debug", on))
		_row("Debug", ldbg)

	# Arcade: debug dump
	if sys == "arcade":
		var cdbg := CheckBox.new()
		cdbg.text = "Write a video debug dump (user://debug_arcade.log; TMNT only; for bug reports; applies when a game loads)"
		cdbg.button_pressed = _opt("arcade_debug")
		cdbg.toggled.connect(func(on): _put("arcade_debug", on))
		_row("Debug", cdbg)

	# 7800: debug trace
	if sys == "a7800":
		var adbg := CheckBox.new()
		adbg.text = "Write a debug trace (user://debug_a7800.log; for bug reports; applies when a game loads)"
		adbg.button_pressed = _opt("a7800_debug")
		adbg.toggled.connect(func(on): _put("a7800_debug", on))
		_row("Debug", adbg)

	# SNES: debug report
	if sys == "snes":
		var sdbg := CheckBox.new()
		sdbg.text = "Print where the CPU and sound CPU are (each second for 10 s, then every 10 s; for bug reports)"
		sdbg.button_pressed = _opt("snes_debug")
		sdbg.toggled.connect(func(on): _put("snes_debug", on))
		_row("Debug", sdbg)

	# 5200: Trak-Ball on controller 1, rolled with the mouse
	if sys == "a5200":
		var tb := CheckBox.new()
		tb.text = "Trak-Ball on controller 1 (roll with the mouse over the picture, left click bottom fire, right click top fire)"
		tb.button_pressed = _opt("a5200_trackball")
		tb.toggled.connect(func(on): _put("a5200_trackball", on))
		_row("Controller", tb)

	# Genesis: console region (PAL = 50 Hz) and pad type; both apply when a game loads
	if sys == "md":
		var mdbg := CheckBox.new()
		mdbg.text = "Print where the CPUs and the Sega CD drive are (each second for 10 s, then every 10 s; for bug reports)"
		mdbg.button_pressed = _opt("md_debug")
		mdbg.toggled.connect(func(on): _put("md_debug", on))
		_row("Debug", mdbg)
		var region := OptionButton.new()
		var regions := [["auto", "Auto (from the cartridge)"], ["usa", "USA (60 Hz)"],
			["japan", "Japan (60 Hz)"], ["europe", "Europe (50 Hz)"]]
		for i in regions.size():
			region.add_item(regions[i][1])
			if _opt("md_region") == regions[i][0]:
				region.select(i)
		region.item_selected.connect(func(i): _put("md_region", regions[i][0]))
		_row("Console region (next load)", region)
		var pad := OptionButton.new()
		pad.add_item("6-button pad")
		pad.add_item("3-button pad (for the odd game that dislikes the 6-button one)")
		pad.select(1 if _opt("md_pad") == "3" else 0)
		pad.item_selected.connect(func(i): _put("md_pad", "3" if i == 1 else "6"))
		_row("Controller (next load)", pad)

	# Dreamcast: the video cable the BIOS sees, and the debug log
	if sys == "dc":
		var cable := OptionButton.new()
		var cables := [["composite", "Composite (most compatible)"], ["rgb", "RGB (SCART)"], ["vga", "VGA (progressive; some games refuse it)"]]
		for c in cables:
			cable.add_item(c[1])
		cable.select(max(0, cables.map(func(c): return c[0]).find(_opt("dc_cable"))))
		cable.item_selected.connect(func(i): _put("dc_cable", cables[i][0]))
		_row("Video cable (next load)", cable)
		var bcast := OptionButton.new()
		var bcasts := [["flash", "As dc_flash.bin says"], ["ntsc", "NTSC (60 Hz)"], ["pal", "PAL (50 Hz)"]]
		for b in bcasts:
			bcast.add_item(b[1])
		bcast.select(max(0, bcasts.map(func(b): return b[0]).find(_opt("dc_broadcast"))))
		bcast.item_selected.connect(func(i): _put("dc_broadcast", bcasts[i][0]))
		_row("TV standard (next load)", bcast)
		var ratio := OptionButton.new()
		var ratios := [["1", "1 cycle an instruction (most CPU time, slowest)"], ["1.5", "1.5 cycles an instruction (default)"], ["2", "2 cycles an instruction (faster; Sonic Adventure, Crazy Taxi stall)"], ["3", "3 cycles an instruction (fastest)"]]
		for r in ratios:
			ratio.add_item(r[1])
		ratio.select(max(0, ratios.map(func(r): return r[0]).find(str(_opt("dc_cpu_ratio")))))
		ratio.item_selected.connect(func(i): _put("dc_cpu_ratio", ratios[i][0]))
		_row("CPU timing", ratio)
		var idle := CheckBox.new()
		idle.text = "Skip waiting loops (faster)"
		idle.button_pressed = _opt("dc_idle_skip")
		idle.toggled.connect(func(on): _put("dc_idle_skip", on))
		_row("Speed", idle)
		var rthr := CheckBox.new()
		rthr.text = "Draw 3D on other threads while the CPU runs (faster)"
		rthr.button_pressed = _opt("dc_render_thread")
		rthr.toggled.connect(func(on): _put("dc_render_thread", on))
		_row("", rthr)
		var fpx := CheckBox.new()
		fpx.text = "Exact FPU rounding (slower; try if physics or geometry look off)"
		fpx.button_pressed = _opt("dc_fpu_exact")
		fpx.toggled.connect(func(on): _put("dc_fpu_exact", on))
		_row("", fpx)
		var rec := CheckBox.new()
		rec.text = "Record the first minute of sound to dc_sound.wav (needs the speed log)"
		rec.button_pressed = _opt("dc_record_sound")
		rec.toggled.connect(func(on): _put("dc_record_sound", on))
		_row("Debug", rec)
		var dump := CheckBox.new()
		dump.text = "Save a 3D frame (dc_frame_1.bin, _2...) each time this is ticked on"
		dump.button_pressed = _opt("dc_dump_frame")
		dump.toggled.connect(func(on): _put("dc_dump_frame", on))
		_row("Debug", dump)
		var slog := CheckBox.new()
		slog.text = "Print a speed line every 5 seconds to debug_dc.log"
		slog.button_pressed = _opt("dc_speed_log")
		slog.toggled.connect(func(on): _put("dc_speed_log", on))
		_row("Speed log", slog)
		var ddbg := CheckBox.new()
		ddbg.text = "Print a debug log every 10 seconds (for bug reports; slows the game)"
		ddbg.button_pressed = _opt("dc_debug")
		ddbg.toggled.connect(func(on): _put("dc_debug", on))
		_row("Debug", ddbg)

	# DS: screen layout, and the 10-second debug log in the Output panel
	if sys == "nds":
		var layout := OptionButton.new()
		for item in ["Stacked (top above touch screen)", "Side by side", "Top screen only", "Touch screen only"]:
			layout.add_item(item)
		layout.select(int(_opt("nds_layout")))
		layout.item_selected.connect(func(i): _put("nds_layout", i))
		_row("Screens", layout)
		var swap := CheckBox.new()
		swap.text = "Swap the two screens"
		swap.button_pressed = _opt("nds_swap")
		swap.toggled.connect(func(on): _put("nds_swap", on))
		_row("", swap)
		var dbg := CheckBox.new()
		dbg.text = "Print a debug log every 10 seconds (for bug reports)"
		dbg.button_pressed = _opt("nds_debug")
		dbg.toggled.connect(func(on): _put("nds_debug", on))
		_row("Debug", dbg)

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

	# Game Boy: which model, and the original Game Boy's boot ROM
	if sys == "gb":
		var model := OptionButton.new()
		var models := [["auto", "Auto (Game Boy Color for colour games)"], ["dmg", "Game Boy"],
			["cgb", "Game Boy Color (colours DMG games too)"],
			["sgb", "Super Game Boy (border and colours for SGB games)"]]
		for i in models.size():
			model.add_item(models[i][1])
			if _opt("gb_model") == models[i][0]:
				model.select(i)
		model.item_selected.connect(func(i): _put("gb_model", models[i][0]))
		_row("Model", model)
		_row("Game Boy boot ROM", _path_field("bios_dmg", AppSettings.GB_DMG_BIOS_DEFAULT))
		_row("Super Game Boy boot ROM", _path_field("bios_sgb", AppSettings.GB_SGB_BIOS_DEFAULT))

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
		_row("Game Boy Color boot ROM" if sys == "gb" else "BIOS file", bios)
		if sys in AppSettings.BIOS_OPTIONAL:
			var skip := CheckBox.new()
			skip.text = "Skip the boot animation" if sys == "gb" else "Skip boot ROM (start the game directly)"
			skip.button_pressed = _opt("skip_bios")
			skip.toggled.connect(func(on): _put("skip_bios", on))
			_row("", skip)
		var note := Label.new()
		note.text = "BIOS changes apply the next time a game loads."
		if sys == "gb":
			note.text = "Model and boot ROM changes apply the next time a game loads.\nNo boot ROM file? An open-source one (SameBoy) is used instead."
		note.modulate = Color(1, 1, 1, 0.6)
		_row("", note)


## A path box with a Browse button that saves into option `key`.
func _path_field(key: String, placeholder: String) -> Control:
	var box := HBoxContainer.new()
	var edit := LineEdit.new()
	edit.text = _opt(key)
	edit.placeholder_text = placeholder
	edit.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	edit.custom_minimum_size.x = 260
	edit.text_submitted.connect(func(t): _put(key, t))
	edit.focus_exited.connect(func(): _put(key, edit.text))
	box.add_child(edit)
	var browse := Button.new()
	browse.text = "Browse…"
	var dlg := FileDialog.new()
	dlg.file_mode = FileDialog.FILE_MODE_OPEN_FILE
	dlg.access = FileDialog.ACCESS_FILESYSTEM
	dlg.title = "Choose boot ROM file"
	dlg.file_selected.connect(func(p):
		edit.text = p
		_put(key, p))
	box.add_child(dlg)
	browse.pressed.connect(func(): dlg.popup_centered_ratio(0.6))
	box.add_child(browse)
	return box


func _row(label: String, control: Control) -> void:
	var l := Label.new()
	l.text = label
	_form.add_child(l)
	_form.add_child(control)


func _finish() -> void:
	if _bios_edit:
		_put("bios", _bios_edit.text)
	AppSettings.save_settings()
