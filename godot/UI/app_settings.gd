extends Node
## Autoload "AppSettings": app settings + per-system controller mappings.
## Saved to user://GD_EMU/settings.cfg (plain text, safe to hand-edit or delete).
##
## How remapping works: the emulator thread reads the InputMap actions (c1_up, c1_a, ...).
## Each system keeps its own binding table; apply_input_profile(system_id) rewrites those
## actions when a game loads. Every action has one keyboard and one controller slot.
## Defaults are whatever the project's Input Map has at startup.

const PATH := "user://settings.cfg"

const ALL_ACTIONS := [
	"c1_up", "c1_down", "c1_left", "c1_right", "c1_b", "c1_a", "c1_select", "c1_start",
	"coin", "c2_start",
	"c1_0", "c1_1", "c1_2", "c1_3", "c1_4", "c1_5", "c1_6", "c1_7", "c1_8", "c1_9",
	"c1_star", "c1_hash",
]

## File extension -> system id
const SYSTEM_BY_EXT := {
	"nes": "nes", "gb": "gb", "gbc": "gb", "col": "coleco", "cv": "coleco", "rom": "coleco",
	"sms": "sms", "gg": "gg", "pce": "pce", "a26": "a2600", "bin": "a2600",
}

## Per system: display name and the buttons it uses, as [action, label].
## The [x] in each label is the emulator's generic button, so you can tell which is which.
var SYSTEMS := {}

var fullscreen := false
var state_slot := 0
var rom_folders: Array = []      # folders the game library scans
var recent: Array = []           # newest first: {"path", "inner", "title", "system"}
const MAX_RECENT := 10

## Per-system options (frontend side). Missing keys fall back to these defaults.
const OPTION_DEFAULTS := {
	"aspect": "console",     # "console" = the system's TV shape, "square" = square pixels
	"smooth": false,         # bilinear filtering instead of sharp pixels
	"crop_top": 0, "crop_bottom": 0, "crop_left": 0, "crop_right": 0,
	"bios": "",              # "" = the default path below
	"skip_bios": false,      # boot without the BIOS / boot ROM (where the system allows it)
}
## Systems that load a BIOS, and where it's looked for by default.
const BIOS_DEFAULTS := {
	"coleco": "user://BIOS/colecovision.rom",
	"gb": "user://BIOS/gbc_bios.bin",
}
const BIOS_OPTIONAL := ["gb"]    # these can boot without it
var _options := {}               # system id -> {option: value} (only changed ones)

var _defaults := {}   # action -> {"key": binding or null, "pad": binding or null}
var _bindings := {}   # system id -> action -> {"key": ..., "pad": ...}  (only systems you changed)


func _init() -> void:
	var dpad := [["c1_up", "Up"], ["c1_down", "Down"], ["c1_left", "Left"], ["c1_right", "Right"]]
	var keypad := []
	for i in 10:
		keypad.append(["c1_%d" % i, "Keypad %d" % i])
	keypad.append(["c1_star", "Keypad *"])
	keypad.append(["c1_hash", "Keypad #"])
	SYSTEMS = {
		"nes": {"name": "NES", "actions": dpad + [
			["c1_b", "B"], ["c1_a", "A"], ["c1_select", "Select"], ["c1_start", "Start"]]},
		"gb": {"name": "Game Boy / Color", "actions": dpad + [
			["c1_b", "B"], ["c1_a", "A"], ["c1_select", "Select"], ["c1_start", "Start"]]},
		"sms": {"name": "Master System", "actions": dpad + [
			["c1_b", "Button 1 [B]"], ["c1_a", "Button 2 [A]"], ["c1_start", "Pause [Start]"]]},
		"gg": {"name": "Game Gear", "actions": dpad + [
			["c1_b", "Button 1 [B]"], ["c1_a", "Button 2 [A]"], ["c1_start", "Start"]]},
		"coleco": {"name": "ColecoVision", "actions": dpad + [
			["c1_b", "Fire L [B]"], ["c1_a", "Fire R [A]"], ["c1_start", "Start"]] + keypad},
		"pce": {"name": "TurboGrafx-16", "actions": dpad + [
			["c1_a", "I [A]"], ["c1_b", "II [B]"], ["c1_select", "Select"], ["c1_start", "Run [Start]"]]},
		"a2600": {"name": "Atari 2600", "actions": dpad + [
			["c1_a", "Fire [A]"], ["c1_select", "Game Select [Select]"], ["c1_start", "Game Reset [Start]"]]},
		"arcade": {"name": "Arcade", "actions": dpad + [
			["c1_a", "Button 1 [A]"], ["c1_b", "Button 2 [B]"], ["c1_start", "P1 Start"],
			["c2_start", "P2 Start"], ["coin", "Coin"]]},
	}


func _ready() -> void:
	for action in ALL_ACTIONS:
		if not InputMap.has_action(action):
			InputMap.add_action(action)
		_defaults[action] = _split(InputMap.action_get_events(action))
	load_settings()


# ---------------- save / load ----------------

func load_settings() -> void:
	var cfg := ConfigFile.new()
	if cfg.load(PATH) != OK:
		return
	fullscreen = cfg.get_value("video", "fullscreen", false)
	state_slot = cfg.get_value("states", "slot", 0)
	rom_folders = cfg.get_value("library", "folders", [])
	recent = cfg.get_value("library", "recent", [])
	for section in cfg.get_sections():
		if section.begins_with("opt_"):
			var opts := {}
			for key in cfg.get_section_keys(section):
				if OPTION_DEFAULTS.has(key):
					opts[key] = cfg.get_value(section, key)
			_options[section.substr(4)] = opts
		if not section.begins_with("input_"):
			continue
		var table := {}
		for action in cfg.get_section_keys(section):
			var entry = cfg.get_value(section, action)
			if entry is Dictionary and action in ALL_ACTIONS:
				table[action] = {"key": entry.get("key"), "pad": entry.get("pad")}
		_bindings[section.substr(6)] = table


func save_settings() -> void:
	DirAccess.make_dir_recursive_absolute(PATH.get_base_dir())
	var cfg := ConfigFile.new()
	cfg.set_value("video", "fullscreen", fullscreen)
	cfg.set_value("states", "slot", state_slot)
	cfg.set_value("library", "folders", rom_folders)
	cfg.set_value("library", "recent", recent)
	for system_id in _options:
		for key in _options[system_id]:
			cfg.set_value("opt_" + system_id, key, _options[system_id][key])
	for system_id in _bindings:
		for action in _bindings[system_id]:
			cfg.set_value("input_" + system_id, action, _bindings[system_id][action])
	cfg.save(PATH)


# ---------------- per-system options ----------------

func get_option(system_id: String, key: String) -> Variant:
	return _options.get(system_id, {}).get(key, OPTION_DEFAULTS[key])


func set_option(system_id: String, key: String, value: Variant) -> void:
	if not _options.has(system_id):
		_options[system_id] = {}
	_options[system_id][key] = value


func reset_options(system_id: String) -> void:
	_options.erase(system_id)


## BIOS file to load for a system ("" = boot without one). `fallback` is used when the
## system has no default listed in BIOS_DEFAULTS.
func bios_path(system_id: String, fallback: String = "") -> String:
	if system_id in BIOS_OPTIONAL and get_option(system_id, "skip_bios"):
		return ""
	var custom: String = get_option(system_id, "bios")
	if custom != "":
		return custom
	return BIOS_DEFAULTS.get(system_id, fallback)


# ---------------- recent ROMs ----------------

func add_recent(path: String, inner: String, title: String, system_id: String) -> void:
	for i in range(recent.size() - 1, -1, -1):
		if recent[i]["path"] == path and recent[i]["inner"] == inner:
			recent.remove_at(i)
	recent.push_front({"path": path, "inner": inner, "title": title, "system": system_id})
	recent.resize(min(recent.size(), MAX_RECENT))
	save_settings()


func system_name(system_id: String) -> String:
	return SYSTEMS.get(system_id, {"name": system_id})["name"]


# ---------------- bindings ----------------

## Binding for one action: kind = "key" or "pad". Returns a Dictionary or null.
func get_binding(system_id: String, action: String, kind: String) -> Variant:
	return _entry(system_id, action)[kind]


## Assign (or clear, with null) a binding. Any other button of this system that used the
## same key/pad button loses it, so one press never triggers two buttons.
func set_binding(system_id: String, action: String, kind: String, binding) -> void:
	if not _bindings.has(system_id):
		_bindings[system_id] = {}
	var table: Dictionary = _bindings[system_id]
	if binding != null:
		for pair in SYSTEMS.get(system_id, {"actions": []})["actions"]:
			var other: String = pair[0]
			if other != action and _same(_entry(system_id, other)[kind], binding):
				table[other] = _entry(system_id, other).duplicate()
				table[other][kind] = null
	table[action] = _entry(system_id, action).duplicate()
	table[action][kind] = binding


func reset_system(system_id: String) -> void:
	_bindings.erase(system_id)


## Load this system's table into the InputMap (call when a game starts).
func apply_input_profile(system_id: String) -> void:
	for action in ALL_ACTIONS:
		InputMap.action_erase_events(action)
		var entry := _entry(system_id, action)
		for kind in ["key", "pad"]:
			if entry[kind] != null:
				InputMap.action_add_event(action, binding_to_event(entry[kind]))


func _entry(system_id: String, action: String) -> Dictionary:
	var table: Dictionary = _bindings.get(system_id, {})
	if table.has(action):
		return table[action]
	return _defaults.get(action, {"key": null, "pad": null})


# ---------------- binding <-> InputEvent ----------------
# A binding is a small Dictionary so the settings file stays readable:
#   {"t": "key", "code": 87, "phys": true}   {"t": "btn", "i": 0}   {"t": "axis", "a": 1, "d": -1}

func event_to_binding(ev: InputEvent) -> Variant:
	if ev is InputEventKey:
		var phys: bool = ev.physical_keycode != 0
		return {"t": "key", "code": int(ev.physical_keycode if phys else ev.keycode), "phys": phys}
	if ev is InputEventJoypadButton:
		return {"t": "btn", "i": int(ev.button_index)}
	if ev is InputEventJoypadMotion:
		return {"t": "axis", "a": int(ev.axis), "d": 1 if ev.axis_value > 0 else -1}
	return null


func binding_to_event(b: Dictionary) -> InputEvent:
	match b.get("t"):
		"key":
			var k := InputEventKey.new()
			if b.get("phys", true):
				k.physical_keycode = int(b["code"])
			else:
				k.keycode = int(b["code"])
			return k
		"btn":
			var j := InputEventJoypadButton.new()
			j.button_index = int(b["i"])
			return j
		_:
			var m := InputEventJoypadMotion.new()
			m.axis = int(b["a"])
			m.axis_value = float(b["d"])
			return m


const PAD_BUTTONS := ["A / Cross", "B / Circle", "X / Square", "Y / Triangle", "Back", "Guide",
	"Start", "L Stick Click", "R Stick Click", "LB / L1", "RB / R1",
	"D-pad Up", "D-pad Down", "D-pad Left", "D-pad Right"]
const PAD_AXES := ["Left Stick X", "Left Stick Y", "Right Stick X", "Right Stick Y", "LT / L2", "RT / R2"]

func binding_text(b) -> String:
	if b == null:
		return "—"
	match b.get("t"):
		"key":
			# physical codes are named by their US-layout key, which is fine for display
			return OS.get_keycode_string(int(b["code"]))
		"btn":
			var i := int(b["i"])
			return PAD_BUTTONS[i] if i < PAD_BUTTONS.size() else "Pad Button %d" % i
		_:
			var a := int(b["a"])
			var name: String = PAD_AXES[a] if a < PAD_AXES.size() else "Axis %d" % a
			if a == 0 or a == 2:
				return name.replace(" X", " Left" if b["d"] < 0 else " Right")
			if a == 1 or a == 3:
				return name.replace(" Y", " Up" if b["d"] < 0 else " Down")
			return name


func _split(events: Array) -> Dictionary:
	var out := {"key": null, "pad": null}
	for ev in events:
		var b: Variant = event_to_binding(ev)
		if b == null:
			continue
		var kind := "key" if b["t"] == "key" else "pad"
		if out[kind] == null:
			out[kind] = b
	return out


func _same(a, b) -> bool:
	if a == null or b == null:
		return false
	return _id(a) == _id(b)


func _id(b: Dictionary) -> String:
	match b.get("t"):
		"key": return "key:%d:%s" % [int(b["code"]), str(b.get("phys", true))]
		"btn": return "btn:%d" % int(b["i"])
		_: return "axis:%d:%d" % [int(b["a"]), int(b["d"])]
