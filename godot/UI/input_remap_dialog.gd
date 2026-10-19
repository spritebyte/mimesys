class_name InputRemapDialog
extends AcceptDialog
## Settings > Controls. Pick a system, click a box, press a key or controller button.
## Esc cancels the capture, Delete/Backspace clears the box. Built entirely in code,
## so there's no scene to set up: main.gd does InputRemapDialog.new() and add_child().

signal bindings_changed

var _system_pick := OptionButton.new()
var _grid := GridContainer.new()
var _system_ids: Array = []

var _waiting: Button = null
var _waiting_action := ""
var _waiting_kind := ""


func _init() -> void:
	title = "Controls"
	ok_button_text = "Done"
	min_size = Vector2i(620, 520)
	exclusive = true

	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 10)

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
	reset.pressed.connect(_on_reset)
	top.add_child(reset)
	box.add_child(top)

	var hint := Label.new()
	hint.text = "Click a box, then press a key or controller button.   Esc = cancel   Delete = clear"
	hint.modulate = Color(1, 1, 1, 0.7)
	box.add_child(hint)

	var scroll := ScrollContainer.new()
	scroll.size_flags_vertical = Control.SIZE_EXPAND_FILL
	scroll.custom_minimum_size.y = 380
	scroll.horizontal_scroll_mode = ScrollContainer.SCROLL_MODE_DISABLED
	_grid.columns = 3
	_grid.size_flags_horizontal = Control.SIZE_EXPAND_FILL
	_grid.add_theme_constant_override("h_separation", 12)
	scroll.add_child(_grid)
	box.add_child(scroll)
	add_child(box)

	confirmed.connect(_finish)
	canceled.connect(_finish)
	_system_pick.item_selected.connect(func(_i): _rebuild())


func _ready() -> void:
	for id in AppSettings.SYSTEMS:
		_system_ids.append(id)
		_system_pick.add_item(AppSettings.SYSTEMS[id]["name"])


## Open the dialog showing `system_id` (e.g. the system that's running).
func open_for(system_id: String) -> void:
	var i := _system_ids.find(system_id)
	_system_pick.select(max(i, 0))
	_rebuild()
	popup_centered()


func _system() -> String:
	return _system_ids[_system_pick.selected]


func _rebuild() -> void:
	_waiting = null
	for c in _grid.get_children():
		_grid.remove_child(c)
		c.queue_free()
	for header in ["Button", "Keyboard", "Controller"]:
		var h := Label.new()
		h.text = header
		h.add_theme_color_override("font_color", Color(0.6, 0.8, 1.0))
		_grid.add_child(h)
	var sys := _system()
	for pair in AppSettings.SYSTEMS[sys]["actions"]:
		var action: String = pair[0]
		var name_lbl := Label.new()
		name_lbl.text = pair[1]
		name_lbl.size_flags_horizontal = Control.SIZE_EXPAND_FILL
		_grid.add_child(name_lbl)
		for kind in ["key", "pad"]:
			var b := Button.new()
			b.custom_minimum_size.x = 170
			b.text = AppSettings.binding_text(AppSettings.get_binding(sys, action, kind))
			b.pressed.connect(_start_capture.bind(b, action, kind))
			_grid.add_child(b)


func _start_capture(b: Button, action: String, kind: String) -> void:
	if _waiting and _waiting != b:
		_waiting.text = AppSettings.binding_text(
			AppSettings.get_binding(_system(), _waiting_action, _waiting_kind))
	_waiting = b
	_waiting_action = action
	_waiting_kind = kind
	b.text = "Press a key…" if kind == "key" else "Press a button…"
	b.release_focus()   # so Space/Enter get captured instead of re-clicking the button


func _input(event: InputEvent) -> void:
	if _waiting == null or not visible:
		return
	if event is InputEventKey:
		if not event.pressed or event.echo:
			return
		get_viewport().set_input_as_handled()
		match event.keycode:
			KEY_ESCAPE:
				_rebuild()
				return
			KEY_DELETE, KEY_BACKSPACE:
				_assign(null)
				return
		if _waiting_kind == "key":
			_assign(AppSettings.event_to_binding(event))
	elif _waiting_kind == "pad":
		if event is InputEventJoypadButton and event.pressed:
			get_viewport().set_input_as_handled()
			_assign(AppSettings.event_to_binding(event))
		elif event is InputEventJoypadMotion and abs(event.axis_value) > 0.6:
			get_viewport().set_input_as_handled()
			_assign(AppSettings.event_to_binding(event))


func _assign(binding) -> void:
	AppSettings.set_binding(_system(), _waiting_action, _waiting_kind, binding)
	_rebuild()   # redraw: assigning may have cleared a duplicate elsewhere


func _on_reset() -> void:
	AppSettings.reset_system(_system())
	_rebuild()


func _finish() -> void:
	_waiting = null
	AppSettings.save_settings()
	bindings_changed.emit()
