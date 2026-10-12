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
			var nes_system = NesSystem.create_from_bytes(data, base_name.validate_filename())
			return nes_system

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


# deprecated
# Matches a multi-file zip against known arcade game sets by the zip's base name.
# Add new arcade systems to the match block here.
static func check_and_load_arcade(zip_path: String) -> Variant:
	var zip_name: String = zip_path.get_file().get_basename().to_lower()
	print("Checking arcade zip: '%s'" % zip_name)

	match zip_name:
		"dkong", "dkongjr":
			return load_arcade_donkey_kong(zip_path, zip_name)
		"invaders":
			return load_arcade_space_invaders(zip_path)
		# Add more here as you implement them:
		"pacman", "mspacman":
			return load_arcade_pacman(zip_path)
		# "galaga":
		#     return load_arcade_galaga(zip_path)
		_:
			print("Error: Unrecognized arcade zip '%s'" % zip_path.get_file())
			return null


static func load_arcade_donkey_kong(zip_path: String, zip_name: String) -> Variant:
	print("\n--- [ Arcade: Donkey Kong ] ---")
	print("Zip: %s" % zip_path.get_file())
	var zip := ZIPReader.new()
	zip.open(zip_path)
	print("Files in zip:")
	for f in zip.get_files():
		print("  %s (%d bytes)" % [f, zip.read_file(f).size()])
	var files_dict = {}
	# Load Program ROMs into a flat 16KB buffer
	var prg_rom = PackedByteArray()
	prg_rom.resize(0x4000)
	var prg_files = ["c_5et_g.bin", "c_5ct_g.bin", "c_5bt_g.bin", "c_5at_g.bin"]
	for i in range(prg_files.size()):
		var data = zip.read_file(prg_files[i])
		# Copy data into the correct 4KB offset
		for j in range(data.size()):
			prg_rom[(i * 0x1000) + j] = data[j]

	files_dict["prg_rom"] = prg_rom
	# Load Sprite data
	var sprite_rom = PackedByteArray()
	sprite_rom.resize(0x2000)
	var spr_top1 = zip.read_file("l_4m_b.bin")  # plane 0, top half (y=0-7)
	var spr_top2 = zip.read_file("l_4r_b.bin")  # plane 1, top half
	var spr_bot1 = zip.read_file("l_4n_b.bin")  # plane 0, bottom half (y=8-15)
	var spr_bot2 = zip.read_file("l_4s_b.bin")  # plane 1, bottom half
	files_dict["sprite_rom"] = sprite_rom
	
	# Load Graphics ROMs (keeping separate for the Video Hardware)
	var tile_rom_1 = zip.read_file("v_5h_b.bin")
	var tile_rom_2 = zip.read_file("v_3pt.bin")
	
	# Load Color PROMs
	var color_prom_2j = zip.read_file("c-2j.bpr")
	var color_prom_2k = zip.read_file("c-2k.bpr")
	var color_prom_v5e = zip.read_file("v-5e.bpr")

	files_dict["tile_rom_1"] = tile_rom_1
	files_dict["tile_rom_2"] = tile_rom_2
	files_dict["color_prom_2j"] = color_prom_2j
	files_dict["color_prom_2k"] = color_prom_2k
	files_dict["color_prom_v5e"] = color_prom_v5e
	var sound_rom_1 = zip.read_file("s_3i_b.bin")
	var sound_rom_2 = zip.read_file("s_3j_b.bin")
	var sound_rom = sound_rom_1 + sound_rom_2
	files_dict["sound_rom"] = sound_rom
	zip.close()
	
	var dk_system = EmuSystem.new()
	dk_system.load_romset(5, files_dict, zip_name)
	return dk_system

static func load_arcade_pacman(zip_path: String) -> Variant:
	print("\n--- [ Arcade: Pacman ] ---")
	print("Zip: %s" % zip_path.get_file())
	var zip := ZIPReader.new()
	zip.open(zip_path)
	print("Files in zip:")
	for f in zip.get_files():
		print("  %s (%d bytes)" % [f, zip.read_file(f).size()])
	# Load Program ROMs into a flat 16KB buffer
	var prg_rom = PackedByteArray()
	prg_rom.resize(0x4000)
	var prg_files = ["pacman.6e", "pacman.6f", "pacman.6h", "pacman.6j"]
	for f in ["pacman.6e", "pacman.6f", "pacman.6h", "pacman.6j"]:
		var d = zip.read_file(f)
		print("AUDIT -> ROM Name: ", f, " | Actual Size in ZIP: ", d.size(), " bytes")
		
	for i in range(prg_files.size()):
		var data = zip.read_file(prg_files[i])
		# Copy data into the correct 4KB offset
		for j in range(data.size()):
			prg_rom[(i * 0x1000) + j] = data[j]

	var tile_rom = zip.read_file("pacman.5e")
	var sprite_rom = zip.read_file("pacman.5f")
	
	var color_rom = zip.read_file("82s126.4a")
	var palette_rom = zip.read_file("82s123.7f")

	var sound_rom_1 = zip.read_file("82s126.1m")
	var sound_rom_2 = zip.read_file("82s126.3m")
	var sound_rom = sound_rom_1 + sound_rom_2
	zip.close()
	
#	var bus = PacBus.new(prg_rom, tile_rom, sprite_rom, color_rom, palette_rom, sound_rom)
#	return PacSystem.new(bus)
	print("TODO: Pacman support in future")
	return null

static func load_arcade_space_invaders(zip_path: String) -> Variant:
	print("\n--- [ Arcade: Space Invaders ] ---")
	print("Zip: %s" % zip_path.get_file())
	print("STATUS: Intel 8080 CPU + hardware not yet implemented.")
	print("----------------------------------\n")
	return null

#static func load_coleco(data: PackedByteArray) -> GameROM:
#	var rom := GameROM.new()
#	rom.bios = load_bios(bios_path)
#	if data != null and data.size() % 1024 == 512:
#		print("Detected 512-byte header. Skipping...")
#		data = data.slice(512)
#	rom.cartridge = data
#	return rom

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
			var expected: int = rom["size"].hex_to_int()
			if data.is_empty():
				push_error("%s: missing %s" % [set_id, rom["name"]])
				ok = false
				continue
			if data.size() != expected:
				push_error("%s: %s is %d bytes, expected %d" % [set_id, rom["name"], data.size(), expected])
				ok = false
				continue
			for off in [rom["offset"]] + rom.get("reload", []):
				_copy_into(buf, off.hex_to_int(), data)
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

static func _copy_into(dst: PackedByteArray, offset: int, src: PackedByteArray) -> void:
	for i in src.size():
		dst[offset + i] = src[i]

static func _load_arcade_db() -> Dictionary:
	var parsed = JSON.parse_string(FileAccess.get_file_as_string(ARCADE_DB_PATH))
	return parsed if parsed is Dictionary else {}
