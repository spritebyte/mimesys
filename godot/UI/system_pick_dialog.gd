class_name SystemPickDialog
extends ConfirmationDialog
## "Which system is this game for?" Shown when the detector isn't sure (e.g. a .bin that
## could be 2600 or 7800). Likely systems come first, with the reason; every other system
## follows. "Remember" stores the answer for this file, so it's only asked once.
##
## Use:  dialog.ask(file_name, guesses)  then  var answer = await dialog.picked
##       answer = [system_id, remember]  (system_id "" = cancelled)

signal picked(system_id: String, remember: bool)

## Every system that can be loaded (ids as the Rust factory takes them)
const ALL := ["nes", "gb", "gbc", "gba", "sms", "gg", "coleco", "pce", "a2600", "a7800"]
const NAMES := {
	"nes": "NES / Famicom", "gb": "Game Boy", "gbc": "Game Boy Color", "gba": "Game Boy Advance", "sms": "Master System",
	"gg": "Game Gear", "coleco": "ColecoVision", "pce": "TurboGrafx-16 / PC Engine",
	"a2600": "Atari 2600", "a7800": "Atari 7800",
}

var _question := Label.new()
var _list := ItemList.new()
var _remember := CheckBox.new()
var _ids: Array = []
var _answered := false


func _init() -> void:
	title = "Which system?"
	ok_button_text = "Load"
	min_size = Vector2i(560, 420)
	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 8)
	box.add_child(_question)
	_list.size_flags_vertical = Control.SIZE_EXPAND_FILL
	_list.custom_minimum_size.y = 280
	_list.item_activated.connect(func(_i): _choose())
	box.add_child(_list)
	_remember.text = "Remember for this file"
	_remember.button_pressed = true
	box.add_child(_remember)
	add_child(box)
	confirmed.connect(_choose)
	canceled.connect(func(): _finish(""))


static func display_name(system_id: String) -> String:
	return NAMES.get(system_id, system_id)


func ask(file_name: String, guesses: Array) -> void:
	_answered = false
	_question.text = "Which system is \"%s\" for?" % file_name
	_list.clear()
	_ids.clear()
	for g in guesses:
		if g["system"] in ALL and not g["system"] in _ids:
			_ids.append(g["system"])
			_list.add_item("%s   — likely: %s" % [display_name(g["system"]), g["why"]])
	for id in ALL:
		if not id in _ids:
			_ids.append(id)
			_list.add_item(display_name(id))
	_list.select(0)
	popup_centered(min_size)
	_list.grab_focus()


func _choose() -> void:
	var sel := _list.get_selected_items()
	_finish(_ids[sel[0]] if not sel.is_empty() else "")


func _finish(system_id: String) -> void:
	if _answered:
		return
	_answered = true
	hide()
	picked.emit(system_id, _remember.button_pressed and system_id != "")
