extends Node

var emu_system: Variant
@onready var EmulatorScreen = %EmuDisplay
@onready var ScreenContainer = %ScreenContainer
@onready var file_dialog = $RomDialog
@onready var file_menu = $MainVerticalStack/TopMenuBar/FileMenu
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

enum FileMenuID { LOAD_ROM, RELOAD_ROM, SAVE_STATE, LOAD_STATE, QUIT }
enum EmuMenuID { RESET, PAUSE, RESUME, STEP_CPU }
enum DebugMenuID { CPU_REGISTERS, AUDIO_REGISTERS, VDP_REGISTERS, MEMORY_VIEWER, DISASSEMBLY }

# Called when the node enters the scene tree for the first time.
func _ready() -> void:
	get_tree().set_auto_accept_quit(false)
	init_menus()
	emu_mutex = Mutex.new()
	cpu_thread = Thread.new()
	frame_control.main_ui = self
	debug_window.main_ui = self

	audio_gen = AudioStreamGenerator.new()
	audio_gen.mix_rate = 44100
	audio_gen.buffer_length = 0.05
	audio_player = AudioStreamPlayer.new()
	audio_player.stream = audio_gen
	add_child(audio_player)
	audio_player.play()
	print("EXACT SAVE PATH: ", OS.get_user_data_dir())

func init_menus():
	# FILE
	file_menu.add_item("Load ROM", FileMenuID.LOAD_ROM)
#	file_menu.add_item("Reload ROM", FileMenuID.RELOAD_ROM)
	file_menu.add_separator()
	file_menu.add_item("Save State", FileMenuID.SAVE_STATE)
	file_menu.add_item("Load State", FileMenuID.LOAD_STATE)
	file_menu.add_separator()
	file_menu.add_item("Quit", FileMenuID.QUIT)

	file_menu.id_pressed.connect(_on_file_menu)

	# EMULATION
	emu_menu.add_item("Reset", EmuMenuID.RESET)
	emu_menu.add_item("Pause", EmuMenuID.PAUSE)
	emu_menu.add_item("Resume", EmuMenuID.RESUME)
	emu_menu.set_item_disabled(emu_menu.get_item_index(12), true)
	emu_menu.add_item("Step CPU", EmuMenuID.STEP_CPU)

	emu_menu.id_pressed.connect(_on_emu_menu)

	# DEBUG
	debug_menu.add_item("CPU Registers", DebugMenuID.CPU_REGISTERS)
	debug_menu.add_item("Audio Registers", DebugMenuID.AUDIO_REGISTERS)

	debug_menu.id_pressed.connect(_on_debug_menu)

func _on_file_menu(id):
	match id:
		FileMenuID.LOAD_ROM:
			file_dialog.popup_centered()

#		FileMenuID.RELOAD_ROM:
#			emu_system.reload_rom()
#			pass

		FileMenuID.SAVE_STATE:
			if emu_system: emu_system.save_state_to_file(0)

		FileMenuID.LOAD_STATE:
			if emu_system: emu_system.load_state_from_file(0)

		FileMenuID.QUIT:
			get_tree().root.propagate_notification(NOTIFICATION_WM_CLOSE_REQUEST)

func _on_emu_menu(id):
	match id:
		EmuMenuID.RESET:
			if emu_system: emu_system.reset()

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


func _on_debug_menu(id):
	pass

func _exit_tree():
	stop_emulation()

func _process(_delta):
	var now = Time.get_ticks_msec()
	_real_frame_count += 1
	if emu_system and (now - _fps_timer_msec >= 1000):
		_real_fps = _real_frame_count
		_virtual_fps = _virtual_frame_count
		_real_frame_count = 0
		_virtual_frame_count = 0
		_fps_timer_msec = now
#		print("Real FPS: %d | Virtual FPS: %d" % [_real_fps, _virtual_fps])
	if emu_system and emu_system.is_frame_ready():
		var tex_to_apply = emu_system.get_frame_texture()
		if tex_to_apply:
			if EmulatorScreen.texture is AtlasTexture:
				EmulatorScreen.texture.atlas = tex_to_apply
			else:
				# Fallback if a frame arrives before setup_screen executes
				EmulatorScreen.texture = tex_to_apply

func _get_serialized_input() -> int:
	var mask = 0x00
	# --- Low Byte (Bits 0-7): Joystick & Fire Buttons ---
	if Input.is_action_pressed("c1_up"):     mask |= (1 << 0)
	if Input.is_action_pressed("c1_down"):   mask |= (1 << 1)
	if Input.is_action_pressed("c1_left"):   mask |= (1 << 2)
	if Input.is_action_pressed("c1_right"):  mask |= (1 << 3)
	if Input.is_action_pressed("c1_b"): mask |= (1 << 4)
	if Input.is_action_pressed("c1_a"): mask |= (1 << 5)
	if Input.is_action_pressed("c1_select"): mask |= (1 << 6)
	if Input.is_action_pressed("c1_start"): mask |= (1 << 7)

	# --- High Byte (Bits 8-15): Keypad (Serialized as a 4-bit nibble) ---
	# Instead of giving each key its own bit, compress the pressed key 
	# into a standard 4-bit number (0-15), just like real hardware does!
	var key_value = 0x0F # 0x0F means no key is pressed
	if Input.is_action_pressed("c1_0"):    key_value = 0x00
	elif Input.is_action_pressed("c1_1"):  key_value = 0x01
	elif Input.is_action_pressed("c1_2"):  key_value = 0x02
	elif Input.is_action_pressed("c1_3"):  key_value = 0x03
	elif Input.is_action_pressed("c1_4"):  key_value = 0x04
	elif Input.is_action_pressed("c1_5"):  key_value = 0x05
	elif Input.is_action_pressed("c1_6"):  key_value = 0x06
	elif Input.is_action_pressed("c1_7"):  key_value = 0x07
	elif Input.is_action_pressed("c1_8"):  key_value = 0x08
	elif Input.is_action_pressed("c1_9"):  key_value = 0x09
	elif Input.is_action_pressed("c1_star"): key_value = 0x0A
	elif Input.is_action_pressed("c1_hash"): key_value = 0x0B
	
	mask |= (key_value << 8)
#	print("Mask=%02X" % mask)
	return mask
const FRAME_TIME_USEC := 16639

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
		emu_mutex.lock()
		emu_system.run_slice(input_mask)
		emu_system.update_audio_buffer();
		emu_mutex.unlock()
		_virtual_frame_count += 1
		next_frame_time += FRAME_TIME_USEC
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

func execute_load(path: String, internal_zip_file: String = "") -> void:
	if emu_system:
		emu_system.power_off()
	if cpu_thread.is_alive():
		stop_emulation()
	print("Call load_rom: %s" % path)
	var new_system = RomLoader.load_rom(path, internal_zip_file)
	if new_system:
		emu_system = new_system
		emu_system.power_on(audio_player)
		var playback = audio_player.get_stream_playback()
		emu_system.set_audio_playback(playback)
		cpu_thread.start(Callable(self, "_thread_loop"))
		var display_info = emu_system.get_display_info()
		if display_info:
			setup_screen(display_info)
		else:
			print("Could not get display info")
	else:
		OS.alert("Could not load ROM: %s" % path.get_file())

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
