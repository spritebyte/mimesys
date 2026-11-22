extends Node
## Autoload "AppSettings": app settings + per-system controller mappings.
## Saved to user://settings.cfg (plain text, safe to hand-edit or delete).
##
## How remapping works: the emulator thread reads the InputMap actions (c1_up, c1_a, ...).
## Each system keeps its own binding table; apply_input_profile(system_id) rewrites those
## actions when a game loads. Every action has one keyboard and one controller slot.
## Defaults are whatever the project's Input Map has at startup; controller defaults are
## tied to pad 1 for player 1 actions, pad 2 for player 2 (c2_*), pads 3 / 4 for c3_* / c4_*.
## Controller bindings remember which pad they came from ("dev"), so two pads can drive
## two players; bindings saved before that ("dev" missing) still match any pad.

const PATH := "user://settings.cfg"

## Default controller (0-based) for each player's actions.
const PAD_OF_PREFIX := {"c2_": 1, "c3_": 2, "c4_": 3}

## Systems whose controllers have a real analog stick (sent on its own, main.gd
## _get_sticks): there the d-pad actions default to the controller's d-pad buttons, so
## the stick and the d-pad are separate controls (Skies of Arcadia: stick walks, d-pad
## turns the camera). Everywhere else the left stick is the d-pad.
const STICK_SYSTEMS := ["ps1", "ps2", "dc"]
const DPAD_BUTTON := {"up": JOY_BUTTON_DPAD_UP, "down": JOY_BUTTON_DPAD_DOWN,
	"left": JOY_BUTTON_DPAD_LEFT, "right": JOY_BUTTON_DPAD_RIGHT}
## Per-system controller defaults that differ from the project's (action -> axis):
## the Dreamcast's L / R are triggers, so they sit on the controller's triggers.
const PAD_AXIS_DEFAULTS := {"dc": {"c1_l1": JOY_AXIS_TRIGGER_LEFT, "c1_r1": JOY_AXIS_TRIGGER_RIGHT,
	"c2_l1": JOY_AXIS_TRIGGER_LEFT, "c2_r1": JOY_AXIS_TRIGGER_RIGHT}}

const ALL_ACTIONS := [
	"c1_up", "c1_down", "c1_left", "c1_right", "c1_b", "c1_a", "c1_select", "c1_start",
	"coin", "c2_start", "c1_x", "c2_x", "c1_l1", "c1_r1",
	"c1_y", "c1_z", "c1_rs_up", "c1_rs_down", "c1_rs_left", "c1_rs_right",
	"c1_r2", "c1_l3", "c1_r3", "c1_analog",
	"c2_up", "c2_down", "c2_left", "c2_right", "c2_b", "c2_a", "c2_select",
	"c1_0", "c1_1", "c1_2", "c1_3", "c1_4", "c1_5", "c1_6", "c1_7", "c1_8", "c1_9",
	"c1_star", "c1_hash",
	"c2_y", "c2_z", "c2_l1", "c2_r1", "c2_r2", "c2_l3", "c2_r3", "c2_analog",
	"c3_up", "c3_down", "c3_left", "c3_right", "c3_b", "c3_a", "c3_x", "c3_y", "c3_start", "c3_coin",
	"c4_up", "c4_down", "c4_left", "c4_right", "c4_b", "c4_a", "c4_x", "c4_y", "c4_start", "c4_coin",
]

## File extension -> system (controls profile) id, for extensions that only one system uses.
const SYSTEM_BY_EXT := {
	"nes": "nes", "fds": "nes", "gb": "gb", "gbc": "gb", "gba": "gba", "agb": "gba", "col": "coleco", "cv": "coleco",
	"sms": "sms", "gg": "gg", "pce": "pce", "a26": "a2600", "a78": "a7800", "a52": "a5200", "lnx": "lynx", "lyx": "lynx", "sfc": "snes", "smc": "snes",
	"md": "md", "gen": "md", "smd": "md", "nds": "nds",
	"cue": "md", "iso": "md",      # Sega CD discs (the Genesis pad profile)
	"32x": "md",                   # 32X cartridges
	"gdi": "dc", "chd": "dc", "cdi": "dc",   # Dreamcast discs
	"psx": "ps1", "psexe": "ps1",            # PlayStation executables
}
## Extensions several systems use: the ROM detector looks inside these files.
const AMBIGUOUS_EXT := ["bin", "rom"]

## Per system: display name and the buttons it uses, as [action, label].
## The [x] in each label is the emulator's generic button, so you can tell which is which.
var SYSTEMS := {}

var fullscreen := false
var state_slot := 0
var rom_folders: Array = []      # folders the game library scans
var recent: Array = []           # newest first: {"path", "inner", "title", "system"}
const MAX_RECENT := 10
var link_a := ""                 # last games picked for the link cable
var link_b := ""
var link_system := "gb"          # system of the last link cable pair ("gb" / "gba")
var link_trace := false          # record a GBA link log to user://link_trace.txt (debugging)
var net_ip := ""                 # last address joined / port used for network link
var net_port := 47100
var rom_systems := {}            # "path" or "path|file in zip" -> system id the user picked

## Per-system options (frontend side). Missing keys fall back to these defaults.
const OPTION_DEFAULTS := {
	"aspect": "console",     # "console" = the system's TV shape, "square" = square pixels
	"smooth": false,         # (old setting) bilinear filtering; "filter" replaces it
	"filter": "",            # "sharp", "smooth", "sharp_smooth", "scanlines", "crt", "lcd" ("" = from "smooth")
	"filter_strength": 60,   # scanlines / CRT / LCD effect strength, 0-100
	"flicker": 0,            # flicker blend: 0 off, 1 blend (50/50 with the last frame), 2 phosphor fade
	"crop_top": 0, "crop_bottom": 0, "crop_left": 0, "crop_right": 0,
	"bios": "",              # "" = the default path below
	"skip_bios": false,      # boot without the BIOS / boot ROM (where the system allows it)
	"zapper": false,         # NES: Zapper light gun in port 2, aimed with the mouse
	"vs_ppu": -1,            # NES VS System: -1 = from the cartridge, else the NES 2.0 PPU number (0-12)
	"vs_swap": false,        # NES VS System: player 1 on the left stick
	"vs_dip": 0,             # NES VS System: DIP switches 1-8 (bit 0 = switch 1)
	"gb_model": "auto",      # Game Boy: "auto" (colour games on a GBC), "dmg", "cgb", "sgb"
	"bios_dmg": "",          # Game Boy: original Game Boy boot ROM ("" = default path)
	"bios_sgb": "",          # Game Boy: Super Game Boy boot ROM ("" = default path)
	"md_region": "auto",     # Genesis: "auto" (from the cartridge), "usa", "japan", "europe" (50 Hz)
	"md_pad": "6",           # Genesis: "6" six-button pad, "3" three-button pad
	"nds_debug": false,      # DS: print CPU / video state every 10 s (for bug reports)
	"nds_layout": 0,         # DS: 0 stacked, 1 side by side, 2 top screen only, 3 touch screen only
	"nds_swap": false,       # DS: swap the screens' places
	"lynx_debug": false,     # Lynx: print the sound registers every 10 s (for bug reports)
	"snes_debug": false,     # SNES: print where the CPU and sound CPU are (for bug reports)
	"a7800_debug": false,    # Atari 7800: trace stack jumps, DLIs and console switches to user://debug_a7800.log
	"nes_debug": false,      # NES: write the mapper trace to user://debug_nes.log (for bug reports)
	"arcade_debug": false,   # Arcade: write a video RAM / register dump to user://debug_arcade.log (TMNT; for bug reports)
	"md_debug": false,       # Genesis / Sega CD: print where the CPUs and the CD drive are (for bug reports)
	"lynx_rotate": 0,        # Lynx: 0 from the cartridge, 1 none, 2 left, 3 right
	"a5200_trackball": false, # 5200: controller 1 is a Trak-Ball rolled with the mouse
	"dc_cable": "composite", # Dreamcast: video cable the BIOS sees: "composite", "rgb", "vga" (next load)
	"dc_broadcast": "flash", # Dreamcast: TV standard: "flash" (as dc_flash.bin says), "ntsc", "pal" (next load)
	"dc_debug": false,       # Dreamcast: print where the CPU is and what the chips do (for bug reports)
	"dc_speed_log": false,   # Dreamcast: print a speed line every 5 seconds (light, unlike dc_debug)
	"dc_cpu_ratio": "1",     # Dreamcast: SH-4 cycles each instruction counts ("1.5" a little faster, Evolution stalls; "2" more stall)
	"dc_idle_skip": true,    # Dreamcast: skip loops that only wait for memory to change
	"dc_render_thread": true,  # Dreamcast: draw 3D on other threads while the CPU runs
	"dc_jit": false,         # Dreamcast: run the SH-4 through the recompiler (faster; new)
	"dc_fpu_exact": false,   # Dreamcast: SH-4 round-to-zero / denormal modes done exactly
	"dc_record_sound": false,  # Dreamcast: write the first minute of sound to dc_sound.wav
	"dc_card": "Card 1",     # Dreamcast: the memory card in controller 1's slot (user://GD_EMU/DC/VMU/<name>.bin)
	"dc_dump_frame": false,  # Dreamcast: save the next 3D frame to dc_frame.bin (bug reports)
	"ps1_pad": "dualshock",  # PlayStation: "dualshock" (starts digital), "analog" (DualShock in analog mode), "digital"
	"ps1_rumble": true,      # PlayStation: DualShock rumble on the controller
	"ps1_debug": false,      # PlayStation: debug log (CD commands, chip state every 5 s, trace before a crash)
	"saturn_debug": false,   # Saturn: debug log (SMPC / CD commands, the CPUs every 5 s)
	"ps2_debug": false,      # PlayStation 2: debug log (chip state every 5 s, unmodelled registers, trace before a crash)
	"ps1_card": "Card 1",    # PlayStation: the memory card in slot 1 (user://GD_EMU/PS1/MemoryCards/<name>.mcd)
}
## Game Boy boot ROMs: "bios" (above) is the Game Boy Color one, this is the original's.
const GB_DMG_BIOS_DEFAULT := "user://BIOS/dmg_boot.bin"
const GB_SGB_BIOS_DEFAULT := "user://BIOS/sgb_boot.bin"
## Systems that load a BIOS, and where it's looked for by default.
const BIOS_DEFAULTS := {
	"nes": "user://BIOS/disksys.rom",    # Famicom Disk System only (cartridges don't need it)
	"coleco": "user://BIOS/colecovision.rom",
	"gb": "user://BIOS/gbc_bios.bin",
	"gba": "user://BIOS/gba_bios.bin",
	"a5200": "user://BIOS/5200.rom",
	"lynx": "user://BIOS/lynxboot.img",
	"saturn": "user://BIOS/saturn_bios.bin",   # else the first sega_1*, mpr-17933* or *saturn* 512 KB file
	"ps2": "user://BIOS/ps2_bios.bin",         # else the first scph* or *ps2* 4 MB file (OpenPS2BIOS too)
}
const BIOS_OPTIONAL := ["gb", "gba"]    # these can boot without it
var _options := {}               # system id -> {option: value} (only changed ones)

var _defaults := {}   # action -> {"key": binding or null, "pad": binding or null}
var _bindings := {}   # system id -> action -> {"key": ..., "pad": ...}  (only systems you changed)


func _init() -> void:
	var dpad := [["c1_up", "Up"], ["c1_down", "Down"], ["c1_left", "Left"], ["c1_right", "Right"]]
	var dpad2 := [["c2_up", "P2 Up"], ["c2_down", "P2 Down"], ["c2_left", "P2 Left"], ["c2_right", "P2 Right"]]
	var keypad := []
	for i in 10:
		keypad.append(["c1_%d" % i, "Keypad %d" % i])
	keypad.append(["c1_star", "Keypad *"])
	keypad.append(["c1_hash", "Keypad #"])
	SYSTEMS = {
		"nes": {"name": "NES", "actions": dpad + [
			["c1_b", "B"], ["c1_a", "A"], ["c1_select", "Select"], ["c1_start", "Start"]] + dpad2 + [
			["c2_b", "P2 B"], ["c2_a", "P2 A"], ["c2_select", "P2 Select"], ["c2_start", "P2 Start"]]},
		"gb": {"name": "Game Boy / Color", "actions": dpad + [
			["c1_b", "B"], ["c1_a", "A"], ["c1_select", "Select"], ["c1_start", "Start"]]},
		"gba": {"name": "Game Boy Advance", "actions": dpad + [
			["c1_b", "B"], ["c1_a", "A"], ["c1_l1", "L"], ["c1_r1", "R"],
			["c1_select", "Select"], ["c1_start", "Start"]]},
		"sms": {"name": "Master System", "actions": dpad + [
			["c1_b", "Button 1 [B]"], ["c1_a", "Button 2 [A]"], ["c1_start", "Pause [Start]"]] + dpad2 + [
			["c2_b", "P2 Button 1 [B]"], ["c2_a", "P2 Button 2 [A]"]]},
		"md": {"name": "Genesis / Mega Drive", "actions": dpad + [
			["c1_x", "A [X]"], ["c1_b", "B [B]"], ["c1_a", "C [A]"], ["c1_start", "Start"],
			["c1_y", "X [Y]"], ["c1_l1", "Y [L]"], ["c1_r1", "Z [R]"], ["c1_select", "Mode [Select]"]] + dpad2 + [
			["c2_select", "P2 A [P2 Select]"], ["c2_b", "P2 B [B]"], ["c2_a", "P2 C [A]"], ["c2_start", "P2 Start"],
			["c2_y", "P2 X [Y]"], ["c2_l1", "P2 Y [L]"], ["c2_r1", "P2 Z [R]"], ["c2_z", "P2 Mode [Z]"]]},
		"ps1": {"name": "PlayStation", "actions": dpad + [
			["c1_b", "Cross [B]"], ["c1_a", "Circle [A]"], ["c1_x", "Square [X]"], ["c1_y", "Triangle [Y]"],
			["c1_l1", "L1 [L]"], ["c1_r1", "R1 [R]"], ["c1_z", "L2 [Z]"], ["c1_r2", "R2"], ["c1_start", "Start"], ["c1_select", "Select"],
			["c1_l3", "L3 (left stick press)"], ["c1_r3", "R3 (right stick press)"], ["c1_analog", "Analog button (DualShock mode)"]] + dpad2 + [
			["c2_b", "P2 Cross [B]"], ["c2_a", "P2 Circle [A]"], ["c2_x", "P2 Square [X]"], ["c2_y", "P2 Triangle [Y]"],
			["c2_l1", "P2 L1 [L]"], ["c2_r1", "P2 R1 [R]"], ["c2_z", "P2 L2 [Z]"], ["c2_r2", "P2 R2"], ["c2_start", "P2 Start"], ["c2_select", "P2 Select"],
			["c2_l3", "P2 L3 (left stick press)"], ["c2_r3", "P2 R3 (right stick press)"], ["c2_analog", "P2 Analog button"]]},
		"ps2": {"name": "PlayStation 2", "actions": dpad + [
			["c1_b", "Cross [B]"], ["c1_a", "Circle [A]"], ["c1_x", "Square [X]"], ["c1_y", "Triangle [Y]"],
			["c1_l1", "L1 [L]"], ["c1_r1", "R1 [R]"], ["c1_z", "L2 [Z]"], ["c1_r2", "R2"], ["c1_start", "Start"], ["c1_select", "Select"],
			["c1_l3", "L3 (left stick press)"], ["c1_r3", "R3 (right stick press)"], ["c1_analog", "Analog button"]] + dpad2 + [
			["c2_b", "P2 Cross [B]"], ["c2_a", "P2 Circle [A]"], ["c2_x", "P2 Square [X]"], ["c2_y", "P2 Triangle [Y]"],
			["c2_l1", "P2 L1 [L]"], ["c2_r1", "P2 R1 [R]"], ["c2_z", "P2 L2 [Z]"], ["c2_r2", "P2 R2"], ["c2_start", "P2 Start"], ["c2_select", "P2 Select"],
			["c2_l3", "P2 L3 (left stick press)"], ["c2_r3", "P2 R3 (right stick press)"], ["c2_analog", "P2 Analog button"]]},
		"saturn": {"name": "Sega Saturn", "actions": dpad + [
			["c1_b", "A [B]"], ["c1_a", "B [A]"], ["c1_r1", "C [R]"], ["c1_x", "X"], ["c1_y", "Y"], ["c1_l1", "Z [L]"],
			["c1_z", "L trigger [Z]"], ["c1_r2", "R trigger [R2]"], ["c1_start", "Start"]] + dpad2 + [
			["c2_b", "P2 A [B]"], ["c2_a", "P2 B [A]"], ["c2_r1", "P2 C [R]"], ["c2_x", "P2 X"], ["c2_y", "P2 Y"], ["c2_l1", "P2 Z [L]"],
			["c2_z", "P2 L trigger [Z]"], ["c2_r2", "P2 R trigger [R2]"], ["c2_start", "P2 Start"]]},
		"dc": {"name": "Sega Dreamcast", "actions": dpad + [
			["c1_b", "A [B]"], ["c1_a", "B [A]"], ["c1_x", "X"], ["c1_y", "Y"], ["c1_start", "Start"],
			["c1_l1", "L trigger [L2]"], ["c1_r1", "R trigger [R2]"]] + dpad2 + [
			["c2_b", "P2 A [B]"], ["c2_a", "P2 B [A]"], ["c2_x", "P2 X"], ["c2_y", "P2 Y"], ["c2_start", "P2 Start"],
			["c2_l1", "P2 L trigger [L2]"], ["c2_r1", "P2 R trigger [R2]"]] + _arcade_pad(3) + _arcade_pad(4)},
		"nds": {"name": "Nintendo DS (stylus = mouse on the bottom screen)", "actions": dpad + [
			["c1_a", "A"], ["c1_b", "B"], ["c1_x", "X"], ["c1_y", "Y"], ["c1_l1", "L"], ["c1_r1", "R"],
			["c1_select", "Select"], ["c1_start", "Start"], ["c1_z", "Microphone (blow) [Z]"]]},
		"gg": {"name": "Game Gear", "actions": dpad + [
			["c1_b", "Button 1 [B]"], ["c1_a", "Button 2 [A]"], ["c1_start", "Start"]]},
		"coleco": {"name": "ColecoVision", "actions": dpad + [
			["c1_b", "Fire L [B]"], ["c1_a", "Fire R [A]"], ["c1_start", "Start"]] + keypad + dpad2 + [
			["c2_b", "P2 Fire L [B]"], ["c2_a", "P2 Fire R [A]"]]},
		"pce": {"name": "TurboGrafx-16", "actions": dpad + [
			["c1_a", "I [A]"], ["c1_b", "II [B]"], ["c1_select", "Select"], ["c1_start", "Run [Start]"]]},
		"a2600": {"name": "Atari 2600", "actions": dpad + [
			["c1_a", "Fire [A]"], ["c1_select", "Game Select [Select]"], ["c1_start", "Game Reset [Start]"]] + dpad2 + [
			["c2_a", "P2 Fire [A]"]]},
		"snes": {"name": "Super Nintendo", "actions": dpad + [
			["c1_a", "A"], ["c1_b", "B"], ["c1_x", "X"], ["c1_y", "Y"], ["c1_l1", "L"], ["c1_r1", "R"],
			["c1_select", "Select"], ["c1_start", "Start"]] + dpad2 + [
			["c2_a", "P2 A"], ["c2_b", "P2 B"], ["c2_x", "P2 X"], ["c2_y", "P2 Y"], ["c2_l1", "P2 L"], ["c2_r1", "P2 R"],
			["c2_select", "P2 Select"], ["c2_start", "P2 Start"]]},
		"lynx": {"name": "Atari Lynx", "actions": dpad + [
			["c1_a", "A (outer) [A]"], ["c1_b", "B (inner) [B]"], ["c1_l1", "Option 1 [L]"],
			["c1_r1", "Option 2 [R]"], ["c1_start", "Pause [Start]"]]},
		"a5200": {"name": "Atari 5200", "actions": dpad + [
			["c1_a", "Bottom fire [A]"], ["c1_b", "Top fire [B]"], ["c1_start", "Start"],
			["c1_select", "Pause [Select]"], ["c2_start", "Reset [P2 Start]"]] + keypad + dpad2 + [
			["c2_a", "P2 Bottom fire [A]"], ["c2_b", "P2 Top fire [B]"]]},
		"a7800": {"name": "Atari 7800", "actions": dpad + [
			["c1_a", "Button 1 [A]"], ["c1_b", "Button 2 [B]"], ["c1_start", "Pause [Start]"],
			["c1_select", "Select"], ["c2_start", "Reset [P2 Start]"]] + dpad2 + [
			["c2_a", "P2 Button 1 [A]"], ["c2_b", "P2 Button 2 [B]"]]},
		"arcade": {"name": "Arcade", "actions": dpad + [
			["c1_a", "Button 1 [A]"], ["c1_b", "Button 2 [B]"], ["c1_x", "Button 3 [X]"],
			["c1_y", "Button 4 [Y]"], ["c1_z", "Button 5 [Z]"],
			["c1_rs_up", "Right Stick Up"], ["c1_rs_down", "Right Stick Down"],
			["c1_rs_left", "Right Stick Left"], ["c1_rs_right", "Right Stick Right"],
			["c1_start", "P1 Start"], ["c2_start", "P2 Start"], ["coin", "Coin"],
			["c1_l1", "Service / Test [L]"], ["c1_r1", "Switch Player [R]"]] + dpad2 + [
			["c2_a", "P2 Button 1 [A]"], ["c2_b", "P2 Button 2 [B]"], ["c2_x", "P2 Button 3 [X]"],
			["c2_y", "P2 Button 4 [Y]"], ["c2_z", "P2 Button 5 [Z]"],
			["c2_l1", "P2 Shoulder L [L]"], ["c2_r1", "P2 Shoulder R [R]"],
			["c2_select", "P2 Coin [P2 Select]"]] + _arcade_pad(3) + _arcade_pad(4)},
	}


## Arcade player 3 / 4 actions (4-player games: Captain Commando, TMNT, The Simpsons, ...).
## Controller only by default, on pads 3 / 4.
func _arcade_pad(n: int) -> Array:
	var c := "c%d_" % n
	var p := "P%d " % n
	return [[c + "up", p + "Up"], [c + "down", p + "Down"], [c + "left", p + "Left"], [c + "right", p + "Right"],
		[c + "a", p + "Button 1 [A]"], [c + "b", p + "Button 2 [B]"], [c + "x", p + "Button 3 [X]"],
		[c + "y", p + "Button 4 [Y]"], [c + "start", p + "Start"], [c + "coin", p + "Coin"]]


func _ready() -> void:
	for action in ALL_ACTIONS:
		if not InputMap.has_action(action):
			InputMap.add_action(action)
		_defaults[action] = _split(InputMap.action_get_events(action))
		var pad = _defaults[action]["pad"]
		if pad != null and int(pad.get("dev", -1)) < 0:
			pad["dev"] = PAD_OF_PREFIX.get(action.substr(0, 3), 0)
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
	link_a = cfg.get_value("link", "a", "")
	link_b = cfg.get_value("link", "b", "")
	link_system = cfg.get_value("link", "system", "gb")
	link_trace = cfg.get_value("link", "trace", false)
	net_ip = cfg.get_value("network", "ip", "")
	net_port = cfg.get_value("network", "port", 47100)
	rom_systems = cfg.get_value("library", "rom_systems", {})
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
	cfg.set_value("link", "a", link_a)
	cfg.set_value("link", "b", link_b)
	cfg.set_value("link", "system", link_system)
	cfg.set_value("link", "trace", link_trace)
	cfg.set_value("network", "ip", net_ip)
	cfg.set_value("network", "port", net_port)
	cfg.set_value("library", "rom_systems", rom_systems)
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


## The picture filter for a system ("filter", or the old "smooth" switch if it was never set).
func filter_of(system_id: String) -> String:
	var f: String = get_option(system_id, "filter")
	if f == "":
		f = "smooth" if get_option(system_id, "smooth") else "sharp"
	return f


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


## A system the user picked for a file (see SystemPickDialog), or "".
func remembered_system(path: String, inner: String) -> String:
	return rom_systems.get(_rom_key(path, inner), "")


func remember_system(path: String, inner: String, system_id: String) -> void:
	rom_systems[_rom_key(path, inner)] = system_id
	save_settings()


func _rom_key(path: String, inner: String) -> String:
	return path if inner == "" else path + "|" + inner


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
	var d: Dictionary = _defaults.get(action, {"key": null, "pad": null})
	# analog-stick systems: the d-pad actions sit on the controller's d-pad
	var dir := action.substr(3)
	if system_id in STICK_SYSTEMS and action.substr(0, 1) == "c" and action.substr(2, 1) == "_" and DPAD_BUTTON.has(dir):
		d = d.duplicate()
		d["pad"] = {"t": "btn", "i": DPAD_BUTTON[dir], "dev": PAD_OF_PREFIX.get(action.substr(0, 3), 0)}
	var axes: Dictionary = PAD_AXIS_DEFAULTS.get(system_id, {})
	if axes.has(action):
		d = d.duplicate()
		d["pad"] = {"t": "axis", "a": axes[action], "d": 1, "dev": PAD_OF_PREFIX.get(action.substr(0, 3), 0)}
	return d


# ---------------- binding <-> InputEvent ----------------
# A binding is a small Dictionary so the settings file stays readable:
#   {"t": "key", "code": 87, "phys": true}   {"t": "btn", "i": 0}   {"t": "axis", "a": 1, "d": -1}

func event_to_binding(ev: InputEvent) -> Variant:
	if ev is InputEventKey:
		var phys: bool = ev.physical_keycode != 0
		return {"t": "key", "code": int(ev.physical_keycode if phys else ev.keycode), "phys": phys}
	if ev is InputEventJoypadButton:
		return {"t": "btn", "i": int(ev.button_index), "dev": int(ev.device)}
	if ev is InputEventJoypadMotion:
		return {"t": "axis", "a": int(ev.axis), "d": 1 if ev.axis_value > 0 else -1, "dev": int(ev.device)}
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
			j.device = int(b.get("dev", -1))    # -1 = any pad
			return j
		_:
			var m := InputEventJoypadMotion.new()
			m.axis = int(b["a"])
			m.axis_value = float(b["d"])
			m.device = int(b.get("dev", -1))
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
			return (PAD_BUTTONS[i] if i < PAD_BUTTONS.size() else "Pad Button %d" % i) + _pad_suffix(b)
		_:
			var a := int(b["a"])
			var name: String = PAD_AXES[a] if a < PAD_AXES.size() else "Axis %d" % a
			if a == 0 or a == 2:
				name = name.replace(" X", " Left" if b["d"] < 0 else " Right")
			elif a == 1 or a == 3:
				name = name.replace(" Y", " Up" if b["d"] < 0 else " Down")
			return name + _pad_suffix(b)


## " (Pad 2)" for a binding tied to one controller ("" if it matches any).
func _pad_suffix(b: Dictionary) -> String:
	var dev := int(b.get("dev", -1))
	return "" if dev < 0 else " (Pad %d)" % (dev + 1)


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
		"btn": return "btn:%d:%d" % [int(b["i"]), int(b.get("dev", -1))]
		_: return "axis:%d:%d:%d" % [int(b["a"]), int(b["d"]), int(b.get("dev", -1))]
