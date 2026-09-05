class_name RomLoader

# TODO: need better path for default and accept setting to override
static var bios_path: String = 'res://Tests/Roms/coleco/colecovision.rom'

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
				
				return check_and_load_arcade(path)

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

		"col", "cv", "rom":
			print("TODO: Rewriting coleco code...")
			return null
#			var bus := ColecoBus.new()
#			bus.initialize(load_coleco(data))
#			if bus.rom_data != null:
#				var _b := bus.rom_data.bios
#				print("Coleco BIOS loaded")
#				print("BIOS: %s" % ("%02X " * 16 % [b[0],b[1],b[2],b[3],b[4],b[5],b[6],b[7],b[8],b[9],b[10],b[11],b[12],b[13],b[14],b[15]]).strip_edges())
#			else:
#				print("Error: Failed to load Coleco ROM")
#			return ColecoSystem.new(bus)

		"gb", "gbc":
			var bios_path="user://BIOS/gbc_bios.bin"
			var bios: PackedByteArray = load_bios(bios_path)
			print("Loaded BIOS '%s': %d bytes" % [bios_path, bios.size()])
			if bios.is_empty():
				print("Warning: Boot ROM is empty! Emulation will fall back or skip boot ROM.")
			var gb_system = GbSystemNode.create_from_bytes(data, base_name.validate_filename(), bios)
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


# Matches a multi-file zip against known arcade game sets by the zip's base name.
# Add new arcade systems to the match block here.
static func check_and_load_arcade(zip_path: String) -> Variant:
	var zip_name: String = zip_path.get_file().get_basename().to_lower()
	print("Checking arcade zip: '%s'" % zip_name)

	match zip_name:
		"dkong", "dkongjr":
			return load_arcade_donkey_kong(zip_path)
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


static func load_arcade_donkey_kong(zip_path: String) -> Variant:
	print("\n--- [ Arcade: Donkey Kong ] ---")
	print("Zip: %s" % zip_path.get_file())
	var zip := ZIPReader.new()
	zip.open(zip_path)
	print("Files in zip:")
	for f in zip.get_files():
		print("  %s (%d bytes)" % [f, zip.read_file(f).size()])
	# Load Program ROMs into a flat 16KB buffer
	var prg_rom = PackedByteArray()
	prg_rom.resize(0x4000)
	var prg_files = ["c_5et_g.bin", "c_5ct_g.bin", "c_5bt_g.bin", "c_5at_g.bin"]
	for i in range(prg_files.size()):
		var data = zip.read_file(prg_files[i])
		# Copy data into the correct 4KB offset
		for j in range(data.size()):
			prg_rom[(i * 0x1000) + j] = data[j]

	# Load Sprite data
	var sprite_rom = PackedByteArray()
	sprite_rom.resize(0x2000)
	var spr_top1 = zip.read_file("l_4m_b.bin")  # plane 0, top half (y=0-7)
	var spr_top2 = zip.read_file("l_4r_b.bin")  # plane 1, top half
	var spr_bot1 = zip.read_file("l_4n_b.bin")  # plane 0, bottom half (y=8-15)
	var spr_bot2 = zip.read_file("l_4s_b.bin")  # plane 1, bottom half
	# Load Graphics ROMs (keeping separate for the Video Hardware)
	var tile_rom_1 = zip.read_file("v_5h_b.bin")
	var tile_rom_2 = zip.read_file("v_3pt.bin")
	
	# Load Color PROMs
	var color_prom_2j = zip.read_file("c-2j.bpr")
	var color_prom_2k = zip.read_file("c-2k.bpr")
	var color_prom_v5e = zip.read_file("v-5e.bpr")

	var sound_rom_1 = zip.read_file("s_3i_b.bin")
	var sound_rom_2 = zip.read_file("s_3j_b.bin")
	var sound_rom = sound_rom_1 + sound_rom_2
	zip.close()
	
#	var bus = DKBus.new(prg_rom, tile_rom_1, tile_rom_2, spr_top1, spr_top2, spr_bot1, spr_bot2, color_prom_2j, color_prom_2k, color_prom_v5e, sound_rom)
#	return DKSystem.new(bus)
	print("TODO: Redoing in Rust...")
	return null

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
