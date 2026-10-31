extends Node

var emu_system: Variant
@onready var EmulatorScreen = %EmuDisplay
@onready var ScreenContainer = %ScreenContainer
@onready var file_dialog = $RomDialog
@onready var top_menu_bar = $MainVerticalStack/TopMenuBar
@onready var file_menu = $MainVerticalStack/TopMenuBar/FileMenu
@onready var settings_menu = $MainVerticalStack/TopMenuBar/SettingsMenu
@onready var emu_menu = $MainVerticalStack/TopMenuBar/EmulationMenu
@onready var debug_menu = $MainVerticalStack/TopMenuBar/DebugMenu
@onready var frame_control = $MainVerticalStack/FrameControl
@onready var debug_window = %DebugWindow

var _real_fps: int = 0
var _virtual_fps: int = 0
var _real_frame_count: int = 0
var _virtual_frame_count: int = 0
var _fps_timer_msec: int = 0

var input_state: Dictionary = {
	"up": false, "down": false, "left": false, "right": false,
	"a": false, "b": false, "x": false, "y": false,
	"select": false, "start": false,
	"key_0": false, "key_1": false, "key_2": false, "key_3": false,
	"key_4": false, "key_5": false, "key_6": false, "key_7": false,
	"key_8": false, "key_9": false, "key_star": false, "key_hash": false,
}
var cpu_thread: Thread
var emu_mutex: Mutex
var exit_thread: bool = false
var is_paused: bool = false
var thread_heartbeat: int = 0

#const CPU_CLOCK: float = 4000000.0
var audio_gen: AudioStreamGenerator
var audio_player: AudioStreamPlayer
var local_input: Dictionary = {}

# NEW: GUI pass
const STATE_SLOTS := 10                 # slots 0-9 (slot 0 = your existing save states)
var _zapper_on := false                 # NEW (zapper): mouse drives the NES Zapper
var _pointer := Vector3i(-1, -1, 0)     # NEW (zapper): x, y in emulator pixels (-1 = off), buttons
var current_system_id := ""             # "nes", "pce", "arcade"... picks the controls profile
var view_menu: PopupMenu
var slot_menu: PopupMenu
var remap_dialog: InputRemapDialog
var library_dialog: LibraryDialog              # NEW (pass 2)
var options_dialog: SystemOptionsDialog        # NEW (pass 2)
var recent_menu: PopupMenu                     # NEW (pass 2)
var _display_info: SystemDisplayInfo           # NEW (pass 2): kept so options apply live
var cheats_dialog: CheatsDialog                # NEW (cheats)
var link_dialog: LinkLoadDialog                # NEW (link cable)
var link_mode := false                         # NEW (link cable): two Game Boys running
var link_focus := 0                            # NEW (link cable): which one gets the controls
var system_pick: SystemPickDialog              # NEW (ROM detection)
var net_dialog: NetLinkDialog                  # NEW (network link)
var _net_status := ""
var _net_poll_msec := 0
var _trace_file: FileAccess = null              # GBA link log being written (debugging)
var _trace_msec := 0
var _paused_before_dialog := false
var _toast_panel: PanelContainer
var _toast_label: Label
var _toast_tween: Tween

enum FileMenuID { LOAD_ROM, RELOAD_ROM, SAVE_STATE, LOAD_STATE, QUIT, LIBRARY, LINK, NETWORK }
enum EmuMenuID { RESET, PAUSE, RESUME, STEP_CPU, CHEATS }
enum DebugMenuID { CPU_REGISTERS, AUDIO_REGISTERS, VDP_REGISTERS, MEMORY_VIEWER, DISASSEMBLY }
enum SettingsMenuID { NES, GB, CONTROLS, OPTIONS }
enum ViewMenuID { FULLSCREEN }

# Called when the node enters the scene tree for the first time.
func _ready() -> void:
	get_tree().set_auto_accept_quit(false)
	# battery saves / arcade high scores: write changes every 30 s, not only on exit,
	# so a crash doesn't lose them (only touches the disk when something changed)
	var sram_timer := Timer.new()
	sram_timer.wait_time = 30.0
	sram_timer.autostart = true
	sram_timer.timeout.connect(func():
		if emu_system != null and emu_system.has_method("check_and_save_sram"):
			emu_system.check_and_save_sram())
	add_child(sram_timer)
	init_menus()
	_init_toast()
	remap_dialog = InputRemapDialog.new()
	add_child(remap_dialog)
	remap_dialog.bindings_changed.connect(_on_bindings_changed)
	remap_dialog.visibility_changed.connect(_on_remap_visibility)
	library_dialog = LibraryDialog.new()
	add_child(library_dialog)
	library_dialog.game_chosen.connect(func(path, inner): execute_load(path, inner))
	options_dialog = SystemOptionsDialog.new()
	add_child(options_dialog)
	options_dialog.options_changed.connect(_apply_display_options)
	cheats_dialog = CheatsDialog.new()
	add_child(cheats_dialog)
	link_dialog = LinkLoadDialog.new()
	add_child(link_dialog)
	link_dialog.chosen.connect(execute_link_load)
	system_pick = SystemPickDialog.new()
	add_child(system_pick)
	net_dialog = NetLinkDialog.new()
	add_child(net_dialog)
	net_dialog.host_requested.connect(_net_host)
	net_dialog.join_requested.connect(_net_join)
	net_dialog.disconnect_requested.connect(_net_disconnect)
	emu_mutex = Mutex.new()
	cpu_thread = Thread.new()
	frame_control.main_ui = self
	debug_window.main_ui = self

	audio_gen = AudioStreamGenerator.new()
#	audio_gen.mix_rate = 44100
	audio_gen.buffer_length = 0.1
	audio_player = AudioStreamPlayer.new()
	audio_player.stream = audio_gen
	add_child(audio_player)
	audio_player.play()
	print("EXACT SAVE PATH: ", OS.get_user_data_dir())
	if AppSettings.fullscreen:
		_set_fullscreen.call_deferred(true)

func init_menus():
	# FILE
	file_menu.add_item("Game Library...   (Ctrl+L)", FileMenuID.LIBRARY)
	file_menu.add_item("Load ROM", FileMenuID.LOAD_ROM)
	recent_menu = PopupMenu.new()
	recent_menu.name = "RecentMenu"
	file_menu.add_child(recent_menu)
	file_menu.add_submenu_item("Recent ROMs", "RecentMenu")
	file_menu.add_item("Link Cable: 2 players...", FileMenuID.LINK)
	file_menu.add_item("Network Link...", FileMenuID.NETWORK)
	recent_menu.about_to_popup.connect(_refresh_recent_menu)
	recent_menu.id_pressed.connect(_on_recent_menu)
#	file_menu.add_item("Reload ROM", FileMenuID.RELOAD_ROM)
	file_menu.add_separator()
	file_menu.add_item("Save State   (F5)", FileMenuID.SAVE_STATE)
	file_menu.add_item("Load State   (F8)", FileMenuID.LOAD_STATE)
	slot_menu = PopupMenu.new()
	slot_menu.name = "SlotMenu"
	file_menu.add_child(slot_menu)
	file_menu.add_submenu_item("State Slot   (F6 / F7)", "SlotMenu")
	slot_menu.about_to_popup.connect(_refresh_slot_menu)
	slot_menu.id_pressed.connect(_select_slot)
	file_menu.add_separator()
	file_menu.add_item("Quit", FileMenuID.QUIT)

	file_menu.id_pressed.connect(_on_file_menu)

	# VIEW (created here, so no scene changes needed)
	view_menu = PopupMenu.new()
	view_menu.name = "View"
	top_menu_bar.add_child(view_menu)
	top_menu_bar.move_child(view_menu, settings_menu.get_index())
	view_menu.add_check_item("Fullscreen   (F11)", ViewMenuID.FULLSCREEN)
	view_menu.id_pressed.connect(_on_view_menu)

	# SETTINGS
	settings_menu.add_item("Controls...", SettingsMenuID.CONTROLS)
	settings_menu.add_item("System Options...", SettingsMenuID.OPTIONS)
	settings_menu.id_pressed.connect(_on_settings_menu)

	# EMULATION
	emu_menu.add_item("Reset", EmuMenuID.RESET)
	emu_menu.add_item("Pause", EmuMenuID.PAUSE)
	emu_menu.add_item("Resume", EmuMenuID.RESUME)
	emu_menu.set_item_disabled(emu_menu.get_item_index(EmuMenuID.RESUME), true)
	emu_menu.add_item("Step CPU", EmuMenuID.STEP_CPU)
	emu_menu.add_separator()
	emu_menu.add_item("Cheats...   (Ctrl+H)", EmuMenuID.CHEATS)

	emu_menu.id_pressed.connect(_on_emu_menu)

	# DEBUG
	debug_menu.add_item("CPU Registers", DebugMenuID.CPU_REGISTERS)
	debug_menu.add_item("Audio Registers", DebugMenuID.AUDIO_REGISTERS)

	debug_menu.id_pressed.connect(_on_debug_menu)

func _on_settings_menu(id):
	match id:
		SettingsMenuID.CONTROLS:
			remap_dialog.open_for(current_system_id if current_system_id != "" else "nes")
		SettingsMenuID.OPTIONS:
			options_dialog.open_for(current_system_id if current_system_id != "" else "nes")
		SettingsMenuID.NES:
			#TODO: Show NES Settings
			pass
		SettingsMenuID.GB:
			#TODO: Show GB/GBC Settings
			pass

func _on_view_menu(id):
	match id:
		ViewMenuID.FULLSCREEN:
			_set_fullscreen(not AppSettings.fullscreen)

func _on_file_menu(id):
	match id:
		FileMenuID.LIBRARY:
			library_dialog.open()

		FileMenuID.LOAD_ROM:
			file_dialog.popup_centered()

		FileMenuID.LINK:
			link_dialog.open()

		FileMenuID.NETWORK:
			if emu_system == null or not (current_system_id in LINK_SYSTEMS) or link_mode:
				_toast("Load a Game Boy or GBA game first")
			else:
				net_dialog.open(SystemPickDialog.display_name(current_system_id))

#		FileMenuID.RELOAD_ROM:
#			emu_system.reload_rom()
#			pass

		FileMenuID.SAVE_STATE:
			_save_state()

		FileMenuID.LOAD_STATE:
			_load_state()

		FileMenuID.QUIT:
			get_tree().root.propagate_notification(NOTIFICATION_WM_CLOSE_REQUEST)

func _on_emu_menu(id):
	match id:
		EmuMenuID.RESET:
			if emu_system:
				emu_mutex.lock()
				emu_system.reset()
				emu_mutex.unlock()

		EmuMenuID.PAUSE:
			is_paused = true
			emu_menu.set_item_disabled(emu_menu.get_item_index(EmuMenuID.PAUSE), true)
			emu_menu.set_item_disabled(emu_menu.get_item_index(EmuMenuID.RESUME), false)

		EmuMenuID.RESUME:
			is_paused = false
			emu_menu.set_item_disabled(emu_menu.get_item_index(EmuMenuID.PAUSE), false)
			emu_menu.set_item_disabled(emu_menu.get_item_index(EmuMenuID.RESUME), true)


		EmuMenuID.STEP_CPU:
			if emu_system: emu_system.cpu.step()

		EmuMenuID.CHEATS:
			cheats_dialog.open_dialog()


func _on_debug_menu(id):
	pass

# ---------------- NEW: hotkeys ----------------
# Fixed keys (not remappable): F5 save, F8 load, F6/F7 slot down/up, F11 fullscreen,
# Esc leaves fullscreen.
func _unhandled_input(event: InputEvent) -> void:
	if not (event is InputEventKey and event.pressed and not event.echo):
		return
	if event.keycode == KEY_L and event.ctrl_pressed:
		library_dialog.open()
		get_viewport().set_input_as_handled()
		return
	if event.keycode == KEY_H and event.ctrl_pressed:
		cheats_dialog.open_dialog()
		get_viewport().set_input_as_handled()
		return
	if event.keycode == KEY_TAB and link_mode:
		_switch_link_focus()
		get_viewport().set_input_as_handled()
		return
	match event.keycode:
		KEY_F5: _save_state()
		KEY_F8: _load_state()
		KEY_F6: _change_slot(-1)
		KEY_F7: _change_slot(1)
		KEY_F11: _set_fullscreen(not AppSettings.fullscreen)
		KEY_ESCAPE:
			if not AppSettings.fullscreen:
				return
			_set_fullscreen(false)
		_:
			return
	get_viewport().set_input_as_handled()

# ---------------- NEW: save state slots ----------------
func _save_state() -> void:
	if not emu_system:
		return
	emu_mutex.lock()
	var ok: bool = emu_system.save_state_to_file(AppSettings.state_slot)
	emu_mutex.unlock()
	_toast("Saved slot %d" % AppSettings.state_slot if ok else "Couldn't save state")

func _load_state() -> void:
	if not emu_system:
		return
	if _slot_path(AppSettings.state_slot) == "":
		_toast("Slot %d is empty" % AppSettings.state_slot)
		return
	emu_mutex.lock()
	var ok: bool = emu_system.load_state_from_file(AppSettings.state_slot)
	emu_mutex.unlock()
	_toast("Loaded slot %d" % AppSettings.state_slot if ok else "Couldn't load slot %d" % AppSettings.state_slot)

func _change_slot(delta: int) -> void:
	_select_slot(posmod(AppSettings.state_slot + delta, STATE_SLOTS))
	_toast("Slot %d  —  %s" % [AppSettings.state_slot, _slot_info(AppSettings.state_slot)])

func _select_slot(slot: int) -> void:
	AppSettings.state_slot = slot
	AppSettings.save_settings()

func _refresh_slot_menu() -> void:
	slot_menu.clear()
	for i in STATE_SLOTS:
		slot_menu.add_radio_check_item("Slot %d   %s" % [i, _slot_info(i)], i)
		slot_menu.set_item_checked(slot_menu.get_item_index(i), i == AppSettings.state_slot)

## Path of the state file for `slot`, or "" if there isn't one.
func _slot_path(slot: int) -> String:
	if not emu_system:
		return ""
	emu_mutex.lock()
	var path: String = emu_system.state_file_path(slot)
	emu_mutex.unlock()
	return path if path != "" and FileAccess.file_exists(path) else ""

func _slot_info(slot: int) -> String:
	var path := _slot_path(slot)
	if path == "":
		return "(empty)"
	var local: int = FileAccess.get_modified_time(path) + Time.get_time_zone_from_system()["bias"] * 60
	return Time.get_datetime_string_from_unix_time(local, true).substr(0, 16)   # YYYY-MM-DD HH:MM

# ---------------- NEW: fullscreen ----------------
func _set_fullscreen(on: bool) -> void:
	AppSettings.fullscreen = on
	AppSettings.save_settings()
	DisplayServer.window_set_mode(
		DisplayServer.WINDOW_MODE_FULLSCREEN if on else DisplayServer.WINDOW_MODE_WINDOWED)
	# full screen = just the game; F11 or Esc brings the menus back
	top_menu_bar.visible = not on
	frame_control.visible = not on
	view_menu.set_item_checked(view_menu.get_item_index(ViewMenuID.FULLSCREEN), on)
	if on:
		_toast("Fullscreen  —  F11 or Esc to exit")

# ---------------- NEW: controls ----------------
func _on_bindings_changed() -> void:
	if current_system_id != "":
		AppSettings.apply_input_profile(current_system_id)

func _on_remap_visibility() -> void:
	# pause while remapping, so pressed keys don't reach the game
	if remap_dialog.visible:
		_paused_before_dialog = is_paused
		is_paused = true
	else:
		is_paused = _paused_before_dialog

# ---------------- NEW (pass 2): recent ROMs ----------------
func _refresh_recent_menu() -> void:
	recent_menu.clear()
	for i in AppSettings.recent.size():
		var r: Dictionary = AppSettings.recent[i]
		recent_menu.add_item("%s   [%s]" % [r["title"], AppSettings.system_name(r["system"])], i)
	if AppSettings.recent.is_empty():
		recent_menu.add_item("(nothing yet)", 1000)
		recent_menu.set_item_disabled(0, true)
	else:
		recent_menu.add_separator()
		recent_menu.add_item("Clear list", 999)

func _on_recent_menu(id: int) -> void:
	if id == 999:
		AppSettings.recent.clear()
		AppSettings.save_settings()
		return
	if id >= AppSettings.recent.size():
		return
	var r: Dictionary = AppSettings.recent[id]
	if not FileAccess.file_exists(r["path"]):
		_toast("File not found: %s" % r["path"].get_file())
		AppSettings.recent.remove_at(id)
		AppSettings.save_settings()
		return
	execute_load(r["path"], r["inner"])

# ---------------- NEW (pass 2): per-system picture options ----------------
## Crop, picture shape and smoothing for the running system (called on load and on change).
func _apply_display_options() -> void:
	if _display_info == null or current_system_id == "":
		return
	var sys := current_system_id
	var info := _display_info
	var top: int = AppSettings.get_option(sys, "crop_top")
	var bottom: int = AppSettings.get_option(sys, "crop_bottom")
	var left: int = AppSettings.get_option(sys, "crop_left")
	var right: int = AppSettings.get_option(sys, "crop_right")
	var w: int = max(info.visible_width - left - right, 16)
	var h: int = max(info.visible_height - top - bottom, 16)
	var atlas := EmulatorScreen.texture as AtlasTexture
	if atlas:
		atlas.region = Rect2(info.visible_x + left, info.visible_y + top, w, h)
	if AppSettings.get_option(sys, "aspect") == "square":
		ScreenContainer.ratio = float(w) / h
	else:
		# keep the TV shape, minus whatever was cropped off
		ScreenContainer.ratio = info.target_aspect_ratio \
			* (float(w) / info.visible_width) / (float(h) / info.visible_height)
	EmulatorScreen.texture_filter = CanvasItem.TEXTURE_FILTER_LINEAR \
		if AppSettings.get_option(sys, "smooth") else CanvasItem.TEXTURE_FILTER_NEAREST
	# NEW (zapper): light gun in port 2
	_zapper_on = sys == "nes" and AppSettings.get_option(sys, "zapper")
	emu_mutex.lock()
	emu_system.set_option("zapper", 1 if _zapper_on else 0)
	# GBA cartridge clock (Pokemon etc.): local time = UTC + this many minutes
	emu_system.set_option("rtc_utc_offset", Time.get_time_zone_from_system().bias)
	emu_mutex.unlock()
	EmulatorScreen.mouse_default_cursor_shape = Control.CURSOR_CROSS \
		if _zapper_on else Control.CURSOR_ARROW

# ---------------- NEW (zapper) ----------------
## Mouse position over the game picture -> emulator framebuffer pixel, plus buttons.
## Clicks only count while the mouse is over the picture (not on menus).
func _update_pointer() -> void:
	if not _zapper_on:
		return
	var local: Vector2 = EmulatorScreen.get_local_mouse_position()
	var size: Vector2 = EmulatorScreen.size
	if size.x <= 0 or size.y <= 0 or not Rect2(Vector2.ZERO, size).has_point(local):
		_pointer = Vector3i(-1, -1, 0)
		return
	var region := Rect2(0, 0, 256, 240)
	var atlas := EmulatorScreen.texture as AtlasTexture
	if atlas:
		region = atlas.region      # the visible (cropped) part of the framebuffer
	var x := int(region.position.x + local.x / size.x * region.size.x)
	var y := int(region.position.y + local.y / size.y * region.size.y)
	var buttons := 0
	if Input.is_mouse_button_pressed(MOUSE_BUTTON_LEFT):
		buttons |= 1
	if Input.is_mouse_button_pressed(MOUSE_BUTTON_RIGHT):
		buttons |= 2
	_pointer = Vector3i(x, y, buttons)

# ---------------- NEW (network link) ----------------
# The networking is all Rust (gb_netlink.rs); these just call it and show what it says.
func _net_host(port: int) -> void:
	_net_call("net_host", [port])
	_start_trace()
	AppSettings.net_port = port
	AppSettings.save_settings()

func _net_join(address: String, port: int) -> void:
	_net_call("net_join", [address, port])
	_start_trace()
	AppSettings.net_ip = address
	AppSettings.net_port = port
	AppSettings.save_settings()

func _net_disconnect() -> void:
	_net_call("net_disconnect", [])

func _net_call(method: String, args: Array) -> void:
	if emu_system == null:
		return
	emu_mutex.lock()
	var err = emu_system.callv(method, args)
	emu_mutex.unlock()
	if err is String and err != "":
		_toast(err)
	_poll_net_status(true)

## Twice a second while a Game Boy runs: show status changes (connected / disconnected).
func _poll_net_status(force := false) -> void:
	var now := Time.get_ticks_msec()
	if not force and now - _net_poll_msec < 500:
		return
	_net_poll_msec = now
	if emu_system == null or not (current_system_id in LINK_SYSTEMS) or link_mode:
		return
	emu_mutex.lock()
	var s: String = emu_system.net_status()
	emu_mutex.unlock()
	if s == _net_status:
		return
	_net_status = s
	net_dialog.set_status(s)
	if s.begins_with("Connected to") or s.begins_with("Not connected"):
		_toast(s)

# ---------------- NEW: on-screen messages ----------------
func _init_toast() -> void:
	var layer := CanvasLayer.new()
	layer.layer = 100
	add_child(layer)
	_toast_panel = PanelContainer.new()
	_toast_panel.position = Vector2(16, 40)
	_toast_panel.modulate.a = 0.0
	_toast_panel.mouse_filter = Control.MOUSE_FILTER_IGNORE
	_toast_label = Label.new()
	_toast_label.add_theme_font_size_override("font_size", 18)
	_toast_panel.add_child(_toast_label)
	layer.add_child(_toast_panel)

func _toast(text: String) -> void:
	_toast_label.text = "  %s  " % text
	if _toast_tween:
		_toast_tween.kill()
	_toast_panel.modulate.a = 1.0
	_toast_tween = create_tween()
	_toast_tween.tween_interval(1.5)
	_toast_tween.tween_property(_toast_panel, "modulate:a", 0.0, 0.5)

func _exit_tree():
	stop_emulation()

func _process(_delta):
	_poll_net_status()   # NEW (network link)
	_poll_trace()
	_update_pointer()    # NEW (zapper)
	var now = Time.get_ticks_msec()
	_real_frame_count += 1
	if emu_system and (now - _fps_timer_msec >= 1000):
		_real_fps = _real_frame_count
		_virtual_fps = _virtual_frame_count
		_real_frame_count = 0
		_virtual_frame_count = 0
		_fps_timer_msec = now
#		print("Real FPS: %d | Virtual FPS: %d" % [_real_fps, _virtual_fps])
	if emu_system: #and emu_system.is_frame_ready():
		var tex_to_apply = emu_system.get_frame_texture()
		if tex_to_apply:
			if EmulatorScreen.texture is AtlasTexture:
				EmulatorScreen.texture.atlas = tex_to_apply
			else:
				# Fallback if a frame arrives before setup_screen executes
				EmulatorScreen.texture = tex_to_apply

const INPUT_BITS := {
	"c1_up": 0, "c1_down": 1, "c1_left": 2, "c1_right": 3,
	"c1_b": 4, "c1_a": 5, "c1_select": 6, "c1_start": 7,
	"coin": 16, "c2_start": 17, "c1_x": 18, "c2_x": 19, "c1_l1": 20, "c1_r1": 21,
	"c1_y": 22, "c1_z": 23,
	# right stick (twin-stick arcade games): bits 12-15, above the keypad nibble
	"c1_rs_up": 12, "c1_rs_down": 13, "c1_rs_left": 14, "c1_rs_right": 15,
	# player 2: bits 24-31, same order as player 1 (c2_start also sets bit 31, below)
	"c2_up": 24, "c2_down": 25, "c2_left": 26, "c2_right": 27,
	"c2_b": 28, "c2_a": 29, "c2_select": 30,
}

const KEYPAD_ACTIONS := ["c1_0", "c1_1", "c1_2", "c1_3", "c1_4", "c1_5",
	"c1_6", "c1_7", "c1_8", "c1_9", "c1_star", "c1_hash"]

func _get_serialized_input() -> int:
	var mask = 0x00
	# --- Low Byte (Bits 0-7): Joystick & Fire Buttons ---
	for action in INPUT_BITS:
		if Input.is_action_pressed(action):
			mask |= 1 << INPUT_BITS[action]
	if mask & (1 << 17):
		mask |= 1 << 31     # P2 Start in the player 2 byte too

	# --- High Byte (Bits 8-15): Keypad (Serialized as a 4-bit nibble) ---
	# Instead of giving each key its own bit, compress the pressed key
	# into a standard 4-bit number (0-15), just like real hardware does!
	var key_value = 0x0F # 0x0F means no key is pressed
	for i in KEYPAD_ACTIONS.size():
		if Input.is_action_pressed(KEYPAD_ACTIONS[i]):
			key_value = i
			break
	mask |= (key_value << 8)
	return mask

const FRAME_TIME_USEC := 16743
## Frame length for the running game: the core's own when it has one (Genesis PAL = 50 Hz)
var _frame_time_usec := FRAME_TIME_USEC

func _thread_loop():
	var next_frame_time := Time.get_ticks_usec()
	while true:
		if exit_thread:
			break
		if is_paused or not emu_system:
			OS.delay_msec(10)
			next_frame_time = Time.get_ticks_usec()
			continue

		var input_mask = _get_serialized_input()
		var pointer := _pointer
		emu_mutex.lock()
		if _zapper_on:
			emu_system.set_pointer(pointer.x, pointer.y, pointer.z)
		emu_system.run_slice(input_mask)
		emu_system.update_audio_buffer();
		emu_mutex.unlock()
		_virtual_frame_count += 1
		next_frame_time += _frame_time_usec
		var now := Time.get_ticks_usec()
		if next_frame_time > now:
			OS.delay_usec(next_frame_time - now)
		else:
			next_frame_time = now

func _to_binary_string(byte: int) -> String:
	var s = ""
	for i in range(8):
		s = str(byte & 1) + s
		byte >>= 1
	return s

#func _on_load_button_pressed() -> void:
#	file_dialog.popup_centered_ratio(0.4)

func stop_emulation():
	emu_mutex.lock()
	exit_thread = true
	emu_mutex.unlock()
	if cpu_thread.is_started():
		cpu_thread.wait_to_finish()
	exit_thread = false

func _on_rom_dialog_file_selected(path: String) -> void:
	execute_load(path, "")

func _on_rom_selection_dialog_confirmed() -> void:
	var item_list = $RomSelectionDialog/RomItemList
	var selected_indices = item_list.get_selected_items()
	if selected_indices.is_empty():
		return
	var file_name: String = item_list.get_item_text(selected_indices[0])
	var zip_path: String = $RomSelectionDialog.get_meta("zip_path")
	execute_load(zip_path, file_name)

## Load a game: read it, work out its system (asking if the detector isn't sure), start it.
func execute_load(path: String, internal_zip_file: String = "") -> void:
	print("Call load_rom: %s" % path)
	var rom: Dictionary = RomLoader.read_rom(path, internal_zip_file)
	if rom.is_empty():
		OS.alert("Could not read %s" % path.get_file())
		return

	if rom["kind"] == "arcade":
		_stop_current()
		var arcade = RomLoader.load_arcade(path)
		if not arcade:
			OS.alert("Could not load arcade set: %s" % path.get_file())
			return
		var arcade_title := path.get_file().get_basename()
		AppSettings.add_recent(path, internal_zip_file, arcade_title, "arcade")
		link_mode = false
		_start_system(arcade, "arcade", arcade_title)
		return

	# which system? a remembered answer, else the detector, else ask
	var system_id: String = AppSettings.remembered_system(path, internal_zip_file)
	if system_id == "":
		var where: String = path.get_base_dir().path_join(rom["file"])
		var guess: Dictionary = RomLoader.guess_system(rom["data"], where)
		if guess["clear"]:
			system_id = guess["guesses"][0]["system"]
			print("Detected %s (%s)" % [system_id, guess["guesses"][0]["why"]])
		else:
			system_pick.ask(rom["file"], guess["guesses"])
			var answer: Array = await system_pick.picked
			if answer[0] == "":
				return
			system_id = answer[0]
			if answer[1]:
				AppSettings.remember_system(path, internal_zip_file, system_id)

	_stop_current()
	var new_system = RomLoader.create_system(system_id, rom["data"], rom["name"])
	if not new_system:
		OS.alert("Could not load %s as %s" % [rom["file"], SystemPickDialog.display_name(system_id)])
		return
	var title: String = rom["file"].get_basename()
	var profile: String = RomLoader.profile_id(system_id)
	AppSettings.add_recent(path, internal_zip_file, title, profile)   # NEW (pass 2)
	link_mode = false
	_start_system(new_system, profile, title)

## Systems with a link cable (local pair and network).
const LINK_SYSTEMS := ["gb", "gba"]

## NEW (link cable): two Game Boys or two GBAs side by side, joined by a link cable.
## Tab switches controls.
func execute_link_load(system_id: String, path_a: String, path_b: String) -> void:
	_stop_current()
	var a := _read_rom(path_a)
	var b := _read_rom(path_b)
	if a.is_empty() or b.is_empty():
		OS.alert("Couldn't read %s" % (path_a if a.is_empty() else path_b).get_file())
		return
	var sys = EmuSystem.new()
	var ok := false
	if system_id == "gba":
		var gba_bios: PackedByteArray = RomLoader.load_bios(AppSettings.bios_path("gba", "user://BIOS/gba_bios.bin"))
		ok = sys.load_gba_link(a["data"], a["name"], b["data"], b["name"], gba_bios)
	else:
		var bios: PackedByteArray = RomLoader.gb_boot_blob()
		ok = sys.load_gb_link(a["data"], a["name"], b["data"], b["name"], bios)
	if not ok:
		OS.alert("Couldn't start the link cable (see the Output panel)")
		return
	link_mode = true
	link_focus = 0
	_start_system(sys, system_id, "%s + %s" % [a["name"], b["name"]])
	_start_trace()
	_toast("Link cable on  —  Tab switches which %s you control" % _link_unit())

## "Game Boy" / "GBA", for messages about the linked pair.
func _link_unit() -> String:
	return "GBA" if current_system_id == "gba" else "Game Boy"

func _switch_link_focus() -> void:
	link_focus = 1 - link_focus
	emu_mutex.lock()
	emu_system.set_option("link_focus", link_focus)
	emu_mutex.unlock()
	_toast("Controlling %s %d (%s side)" % [_link_unit(), link_focus + 1, ["left", "right"][link_focus]])

## Link log (File > Link Cable / Network Link: "Record a link log"): the GBA link port's
## register writes, changed reads and transfers, written to user://link_trace.txt.
func _start_trace() -> void:
	_stop_trace()
	if not AppSettings.link_trace or emu_system == null or current_system_id != "gba":
		return
	_trace_file = FileAccess.open("user://link_trace.txt", FileAccess.WRITE)
	if _trace_file == null:
		return
	emu_mutex.lock()
	emu_system.set_option("link_trace", 1)
	emu_mutex.unlock()
	_toast("Recording a link log to " + ProjectSettings.globalize_path("user://link_trace.txt"))

func _poll_trace() -> void:
	if _trace_file == null or emu_system == null:
		return
	var now := Time.get_ticks_msec()
	if now - _trace_msec < 500:
		return
	_trace_msec = now
	emu_mutex.lock()
	var lines: PackedStringArray = emu_system.take_trace()
	emu_mutex.unlock()
	for l in lines:
		_trace_file.store_line(l)
	_trace_file.flush()

func _stop_trace() -> void:
	if _trace_file:
		_poll_trace_now()
		_trace_file.close()
		_trace_file = null

func _poll_trace_now() -> void:
	_trace_msec = 0
	_poll_trace()

func _stop_current() -> void:
	_stop_trace()
	if emu_system:
		emu_system.power_off()
	if cpu_thread.is_alive():
		stop_emulation()

func _start_system(new_system, system_id: String, title: String) -> void:
	emu_system = new_system
	current_system_id = system_id
	AppSettings.apply_input_profile(current_system_id)
	emu_system.power_on()
	cheats_dialog.game_loaded(emu_system, emu_mutex, current_system_id, title)   # NEW (cheats)
	if cheats_dialog.active_count() > 0:
		_toast("%d cheat(s) on" % cheats_dialog.active_count())
	var playback = audio_player.get_stream_playback()
	emu_system.set_audio_playback(playback)
	var ft: int = emu_system.frame_time_usec()
	_frame_time_usec = ft if ft > 0 else FRAME_TIME_USEC
	cpu_thread.start(Callable(self, "_thread_loop"))
	var display_info = emu_system.display_info()
	if display_info:
		setup_screen(display_info)
	else:
		print("Could not get display info")

## A .gb/.gbc file, or the first one inside a zip: {"data": PackedByteArray, "name": String}
## A game for the link cable: the file itself, or the first Game Boy / GBA game in a zip.
func _read_rom(path: String) -> Dictionary:
	if path.get_extension().to_lower() != "zip":
		var data := FileAccess.get_file_as_bytes(path)
		return {} if data.is_empty() else {"data": data, "name": path.get_file().get_basename().to_lower().validate_filename()}
	var zip := ZIPReader.new()
	if zip.open(path) != OK:
		return {}
	for f in zip.get_files():
		if f.get_extension().to_lower() in ["gb", "gbc", "gba", "agb"]:
			var data := zip.read_file(f)
			zip.close()
			return {"data": data, "name": f.get_file().get_basename().to_lower().validate_filename()}
	zip.close()
	return {}

func _notification(what):
	if what == NOTIFICATION_WM_CLOSE_REQUEST:
		if emu_system:
			emu_system.power_off()
		stop_emulation()
		get_tree().quit()

func setup_screen(display_info: SystemDisplayInfo):
	# 1. Update the AtlasTexture cropping window using Rust's safe area
	var atlas = EmulatorScreen.texture as AtlasTexture
	if not atlas:
		atlas = AtlasTexture.new()
		# Wrap whatever texture was already there (if any)
		atlas.atlas = EmulatorScreen.texture
		EmulatorScreen.texture = atlas
	atlas.region = Rect2(
		display_info.visible_x,
		display_info.visible_y,
		display_info.visible_width,
		display_info.visible_height
	)

	# 2. Tell the container what shape the game is supposed to be
	ScreenContainer.ratio = display_info.target_aspect_ratio
	# 3. NEW (pass 2): per-system crop / shape / smoothing
	_display_info = display_info
	_apply_display_options()

func request_rewind_to_frame(target_frame: int) -> bool:
	if not emu_system:
		return false
	emu_mutex.lock()
	var result = emu_system.request_rewind_to_frame(target_frame)
	emu_mutex.unlock()
	return result

func get_current_frame() -> int:
	if not emu_system:
		return 0
	emu_mutex.lock()
	var frame = emu_system.get_current_frame()
	emu_mutex.unlock()
	return frame

func get_oldest_rewindable_frame() -> int:
	if not emu_system:
		return 0
	emu_mutex.lock()
	var frame = emu_system.get_oldest_rewindable_frame()
	emu_mutex.unlock()
	return frame


func _on_debug_window_close_requested() -> void:
	debug_window.hide()
