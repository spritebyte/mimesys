class_name NetLinkDialog
extends AcceptDialog
## File > Network Link (Game Boy). Only the buttons live here; the networking itself is Rust
## (gb_netlink.rs), reached through EmuSystem.net_host / net_join / net_disconnect / net_status.

signal host_requested(port: int)
signal join_requested(address: String, port: int)
signal disconnect_requested

var _host_port := SpinBox.new()
var _join_ip := LineEdit.new()
var _join_port := SpinBox.new()
var _status := Label.new()


func _init() -> void:
	title = "Network Link (Game Boy)"
	ok_button_text = "Close"
	min_size = Vector2i(560, 300)

	var box := VBoxContainer.new()
	box.add_theme_constant_override("separation", 12)

	var host_row := HBoxContainer.new()
	var hl := Label.new()
	hl.text = "Host a game on port"
	host_row.add_child(hl)
	_setup_port(_host_port)
	host_row.add_child(_host_port)
	var host_btn := Button.new()
	host_btn.text = "Host"
	host_btn.pressed.connect(func(): host_requested.emit(int(_host_port.value)))
	host_row.add_child(host_btn)
	box.add_child(host_row)

	var join_row := HBoxContainer.new()
	var jl := Label.new()
	jl.text = "Join"
	join_row.add_child(jl)
	_join_ip.placeholder_text = "host's IP, e.g. 192.168.1.20"
	_join_ip.custom_minimum_size.x = 220
	_join_ip.text_submitted.connect(func(_t): _join())
	join_row.add_child(_join_ip)
	var colon := Label.new()
	colon.text = "port"
	join_row.add_child(colon)
	_setup_port(_join_port)
	join_row.add_child(_join_port)
	var join_btn := Button.new()
	join_btn.text = "Join"
	join_btn.pressed.connect(_join)
	join_row.add_child(join_btn)
	box.add_child(join_row)

	var disc := Button.new()
	disc.text = "Disconnect"
	disc.pressed.connect(func(): disconnect_requested.emit())
	box.add_child(disc)

	box.add_child(HSeparator.new())
	_status.text = "Not connected"
	box.add_child(_status)
	var hint := Label.new()
	hint.text = "Both players load their own game first. One hosts, the other joins\nusing the IP shown here. Then go to the Cable Club as usual."
	hint.modulate = Color(1, 1, 1, 0.6)
	box.add_child(hint)
	add_child(box)


func _setup_port(s: SpinBox) -> void:
	s.min_value = 1024
	s.max_value = 65535
	s.value = 47100
	s.custom_minimum_size.x = 100


func _join() -> void:
	if _join_ip.text.strip_edges() != "":
		join_requested.emit(_join_ip.text.strip_edges(), int(_join_port.value))


func open() -> void:
	_join_ip.text = AppSettings.net_ip
	_host_port.value = AppSettings.net_port
	_join_port.value = AppSettings.net_port
	popup_centered(min_size)


func set_status(text: String) -> void:
	_status.text = text if text != "" else "Not connected"
