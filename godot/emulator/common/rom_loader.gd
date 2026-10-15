class_name RomLoader

const ARCADE_DB_PATH := "user://arcade_games.json"

# Loads a ROM from a file path.
# - Non-zip files: detected directly by extension.
# - Single-file zips: the contained file is extracted and detected by its extension.
# - Multi-file zips: treated as an arcade ROM set; matched by the zip's base name.
# - file_inside_zip: optionally force-load a specific file from a zip (e.g. from a picker dialog).
static func load_rom(path: String, file_inside_zip: String = "") -> Variant:
	var data: PackedByteArray
	var actual_ext: String
	var base_name: String
	var ext: String = path.get_extension().to_lower()

	if ext == "zip":
		print("load_rom: zip file loaded")
		# Caller explicitly selected a file from inside the zip.
		if file_inside_zip != "":
			print("Load file from zip: %s" % file_inside_zip)
			var zip := ZIPReader.new()
			zip.open(path)
			data = zip.read_file(file_inside_zip)
			zip.close()
			actual_ext = file_inside_zip.get_extension().to_lower()
			base_name = file_inside_zip.get_basename().to_lower()
		else:
			# Inspect the zip contents to decide how to handle it.
			var zip := ZIPReader.new()
			zip.open(path)
			var all_files: PackedStringArray = zip.get_files()
			zip.close()
			# FILTER: Filter out text files, system metadata, or directories
			var valid_rom_files: Array = []
			for f in all_files:
				var f_ext = f.get_extension().to_lower()
				# Ignore standard text files, mac OS metadata, or directory entries
				if f_ext in ["txt", "md", "nfo", "htm", "html"] or f.begins_with("__MACOSX") or f.ends_with("/"):
					continue
				valid_rom_files.append(f)
			if valid_rom_files.size() > 1:
				# Multi-file zip = arcade ROM set. Match by zip name.
				print("More than 1 file in zip")
				
				return load_arcade(path)
#				return check_and_load_arcade(path)

			elif valid_rom_files.size() == 1:
				# Single-file zip = console ROM. Extract and detect by extension.
				print("Just one zip file, load immediately")
				var the_file: String = valid_rom_files[0]
				print("Load file from zip: %s" % the_file)
				var zip2 := ZIPReader.new()
				zip2.open(path)
				data = zip2.read_file(the_file)
				zip2.close()
				actual_ext = the_file.get_extension().to_lower()
				base_name = the_file.get_file().get_basename().to_lower()

			else:
				print("Error: zip file is empty: %s" % path.get_file())
				return null
	else:
		data = FileAccess.get_file_as_bytes(path)
		actual_ext = ext
		base_name = path.get_file().get_basename().to_lower()

	# Extension-based console detection.
	print("Checking extension for %s" % base_name)

	match actual_ext:
		"nes":
			var nes_system = EmuSystem.new()
			var bios: PackedByteArray = PackedByteArray()
			nes_system.load_cartridge("nes", data, base_name.validate_filename(), bios)
			return nes_system

#			var nes_system = NesSystem.create_from_bytes(data, base_name.validate_filename())
#			return nes_system

		# "A26" = Atari 2600
		# "A52" = Atari 5200
		# "BIN" = Atari 7800?
		# TODO: should rely on file structure rather than extension
		"col", "cv", "rom":
			var bios_path="user://BIOS/colecovision.rom"
			var bios: PackedByteArray = load_bios(bios_path)
			print("Loaded BIOS '%s': %d bytes" % [bios_path, bios.size()])
			if bios.is_empty():
				print("Warning: Boot ROM is empty! Emulation will fall back or skip boot ROM.")
			var coleco_system = EmuSystem.new()
			coleco_system.load_cartridge("coleco", data, base_name.validate_filename(), bios)
			return coleco_system

		"gb", "gbc":
			# TODO: let user select if BIOS should be skipped
			#       and if specific system should be forced.
			var bios_path="user://BIOS/gbc_bios.bin"
			var bios: PackedByteArray = load_bios(bios_path)
			print("Loaded BIOS '%s': %d bytes" % [bios_path, bios.size()])
			if bios.is_empty():
				print("Warning: Boot ROM is empty! Emulation will fall back or skip boot ROM.")
#			var gb_system = GbSystemNode.create_from_bytes(data, base_name.validate_filename(), bios)
			var gb_system = EmuSystem.new()
			if actual_ext == "gb":
				gb_system.load_cartridge("gb", data, base_name.validate_filename(), bios)
			else:
				gb_system.load_cartridge("gbc", data, base_name.validate_filename(), bios)				
			return gb_system

#			print("TODO: Rewriting in rust...")
#			return null
#			var gbrom := GBRom.new(data)
#			gbrom.base_filename = base_name
#			var bus := GBBus.new(gbrom)
#			return GBSystem.new(bus)

		"sms", "gg":
			print("TODO: Will support SMS and Gamegear in future")
			return null
#			var sms_rom = SMSRom.new(data)
#			sms_rom.base_filename = base_name
#			var bus = SMSBus.new(sms_rom)
#			return SMSSystem.new(bus)
		_:
			print("Error: Unrecognized file extension '.%s' for %s" % [actual_ext, base_name])
			return null

static func load_bios(p_path: String) -> PackedByteArray:
	if not FileAccess.file_exists(p_path):
		print("Warning: BIOS not found at '%s'" % p_path)
		return PackedByteArray()
	var file := FileAccess.open(p_path, FileAccess.READ)
	var data := file.get_buffer(file.get_length())
	file.close()
	return data

static func load_arcade(zip_path: String) -> EmuSystem:
	var set_id := zip_path.get_file().get_basename().to_lower()   # "dkong.zip" -> "dkong"
	var db := _load_arcade_db()
	if not db.has(set_id):
		push_error("Unknown arcade set '%s' (not in %s)" % [set_id, ARCADE_DB_PATH])
		return null
	var game: Dictionary = db[set_id]
	print("\n--- [ Arcade: %s ] ---" % game.get("title", set_id))

	# Open this set's zip, then its parent's, and so on.
	# (Variant zips often hold only the files that differ.)
	var zips: Array[ZIPReader] = []
	var id := set_id
	while id != "":
		var zip := ZIPReader.new()
		if zip.open(zip_path.get_base_dir().path_join(id + ".zip")) == OK:
			zips.append(zip)
		id = db.get(id, {}).get("parent", "")

	var regions := _resolve_regions(db, set_id)
	var out := {}
	var ok := true
	for region_name in regions:
		var region: Dictionary = regions[region_name]
		var buf := PackedByteArray()
		buf.resize(region["size"].hex_to_int())
		for rom in region["roms"]:
			var data := _read_from_zips(zips, rom["name"])
			if data.is_empty():
				push_error("%s: missing %s" % [set_id, rom["name"]])
				ok = false
				continue
			var file_off: int = rom.get("file_offset", "0x0").hex_to_int()
			var size: int = rom["size"].hex_to_int()
			if data.size() < file_off + size:
				push_error("%s: %s is %d bytes, need at least %d" % [set_id, rom["name"], data.size(), file_off + size])
				ok = false
				continue
			var chunk := data.slice(file_off, file_off + size)
			var nibble: String = rom.get("nibble", "")
			for off in [rom["offset"]] + rom.get("reload", []):
				_copy_into(buf, off.hex_to_int(), chunk, nibble)
		out[region_name] = buf

	for z in zips:
		z.close()
	if not ok:
		return null

	var sys := EmuSystem.new()
	if not sys.load_arcade(_driver_of(db, set_id), set_id, out):
		return null
	return sys

# A variant's regions replace its parent's, one whole region at a time
static func _resolve_regions(db: Dictionary, set_id: String) -> Dictionary:
	var game: Dictionary = db[set_id]
	var regions := {}
	if game.has("parent"):
		regions = _resolve_regions(db, game["parent"])
	regions.merge(game.get("regions", {}), true)
	return regions

# Variants inherit "driver" (and anything else) from the parent if they don't set it
static func _driver_of(db: Dictionary, set_id: String) -> String:
	var game: Dictionary = db[set_id]
	if game.has("driver"):
		return game["driver"]
	return _driver_of(db, game["parent"])

static func _read_from_zips(zips: Array[ZIPReader], file: String) -> PackedByteArray:
	for zip in zips:
		if zip.get_files().has(file):
			return zip.read_file(file)
	return PackedByteArray()

static func _copy_into(dst: PackedByteArray, offset: int, src: PackedByteArray, nibble: String = "") -> void:
	for i in src.size():
		match nibble:
			"low":  dst[offset + i] |= src[i] & 0x0F
			"high": dst[offset + i] |= (src[i] & 0x0F) << 4
			_:      dst[offset + i] = src[i]

static func _load_arcade_db() -> Dictionary:
	var parsed = JSON.parse_string(FileAccess.get_file_as_string(ARCADE_DB_PATH))
	return parsed if parsed is Dictionary else {}

static func detect_systems(path: String, data: PackedByteArray, ext: String) -> Array[String]:
	# 1. Header sniff: a confident answer wins outright
	if data.size() >= 4 and data.slice(0, 4) == PackedByteArray([0x4E, 0x45, 0x53, 0x1A]):
		return ["nes"]
	if data.size() > 0x150 and _has_gb_logo(data):
		return ["gbc"] if data[0x143] & 0x80 else ["gb"]
	if data.size() >= 2 and ((data[0] == 0xAA and data[1] == 0x55) or (data[0] == 0x55 and data[1] == 0xAA)):
		return ["coleco"]

	# 2. Extension: may return several
	var by_ext := {"nes": ["nes"], "gb": ["gb"], "gbc": ["gbc"], "col": ["coleco"],
				   "rom": ["coleco", "msx"], "bin": ["a2600", "a7800", "genesis"]}
	var candidates: Array[String] = []
	candidates.assign(by_ext.get(ext, []))

	# 3. Folder hint: narrows the list when it matches a candidate
	for part in path.get_base_dir().to_lower().split("/"):
		if part in candidates:
			return [part]
	return candidates   # 0 = unknown, 1 = load it, 2+ = prompt the user

# TODO: need to complete function
static func _has_gb_logo(data: PackedByteArray) -> bool:
	return false
