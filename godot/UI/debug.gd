extends Window

var main_ui = null
var cpu_dict: Dictionary
var ppu_dict: Dictionary

@onready var lbl_pc = %lblPCValue
@onready var lbl_sp = %lblSPValue
@onready var lbl_scanline = %lblScanlineValue
@onready var lbl_cycle = %lblCycleValue
@onready var chk_sprite0 = %chkSprite0Hit

# Called when the node enters the scene tree for the first time.
func _ready() -> void:
	pass # Replace with function body.


# Called every frame. 'delta' is the elapsed time since the previous frame.
func _process(delta: float) -> void:
	if main_ui == null:
		return
	

func refresh_values() -> void:
		cpu_dict = main_ui.emu_system.get_cpu_debug_dict()
		lbl_pc.text = str(cpu_dict["pc"])
		lbl_sp.text = str(cpu_dict["sp"])
		ppu_dict = main_ui.emu_system.get_ppu_debug_dict()
		lbl_scanline.text = str(ppu_dict["scanline"])
		lbl_cycle.text = str(ppu_dict["cycle"])
		chk_sprite0.button_pressed = ppu_dict["sprite0_hit"]

func _on_visibility_changed() -> void:
	if self.visible and main_ui:
		refresh_values()
		main_ui.emu_system.toggle_debug_trace()


func _on_btn_refresh_button_up() -> void:
	if self.visible and main_ui:
		refresh_values()
