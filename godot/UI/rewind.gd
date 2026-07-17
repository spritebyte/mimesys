extends HBoxContainer

var main_ui = null  # set externally by main.gd right after instancing
@onready var frame_label: Label = %FrameLabel

var scrubbing := false

func _process(_delta: float) -> void:
	if main_ui == null:
		return
	if not scrubbing:
		var current: int = main_ui.get_current_frame()
		var oldest: int = main_ui.get_oldest_rewindable_frame()
#		seek_bar.min_value = oldest
#		seek_bar.max_value = current
#		seek_bar.value = current
		frame_label.text = "Frame: %d" % current

func _on_seek_bar_drag_started() -> void:
	scrubbing = true
	main_ui.is_paused = true

func _on_seek_bar_value_changed(value: float) -> void:
	if scrubbing:
		main_ui.request_rewind_to_frame(int(value))

func _on_seek_bar_drag_ended(_value_changed: bool) -> void:
	scrubbing = false
	main_ui.is_paused = false


func _on_play_button_up() -> void:
#	scrubbing = true
	main_ui.is_paused = !main_ui.is_paused


func _on_btn_debug_button_up() -> void:
	main_ui.is_paused = true
	main_ui.debug_window.show()
