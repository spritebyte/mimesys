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

		"sms", "gg":
			var sega_system = EmuSystem.new()
			sega_system.load_cartridge(actual_ext, data, base_name.validate_filename(), PackedByteArray())
			return sega_system

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
	var found := resolve_arcade(zip_path, _load_arcade_db())
	if found.is_empty():
		return null
	var sys := EmuSystem.new()
	if not sys.load_arcade(found["driver"], found["set_id"], found["regions"]):
		return null
	return sys


## Returns {"set_id", "driver", "title", "regions": {name: PackedByteArray}}, or {} on failure.
static func resolve_arcade(zip_path: String, db: Dictionary) -> Dictionary:
	var folder := zip_path.get_base_dir()
	var zip_id := zip_path.get_file().get_basename().to_lower()
	var cache := {}   # zip id -> index, so each zip is only read and hashed once
	var own := _zip_index(cache, folder, zip_id)
	if own.is_empty():
		push_error("Can't open %s" % zip_path)
		return {}

	var candidates: Array = []
	if db.has(zip_id):
		candidates.append(zip_id)
	for set_id in _sets_by_overlap(db, own):
		if set_id != zip_id:
			candidates.append(set_id)
	if candidates.is_empty():
		push_error("%s doesn't match any arcade set in the database" % zip_path.get_file())
		return {}

	var first_missing: Array = []
	for set_id in candidates:
		var pool: Array = [own]
		for id in [set_id] + _parents_of(db, set_id):
			if id == zip_id:
				continue
			var idx := _zip_index(cache, folder, id)
			if not idx.is_empty():
				pool.append(idx)
		var built := _build_regions(db, set_id, pool)
		if built["missing"].is_empty():
			if set_id != zip_id:
				print("%s identified as '%s' by its contents" % [zip_path.get_file(), set_id])
			print("\n--- [ Arcade: %s ] ---" % db[set_id].get("title", set_id))
			return {
				"set_id": set_id,
				"driver": _driver_of(db, set_id),
				"title": db[set_id].get("title", set_id),
				"regions": built["regions"],
			}
		if first_missing.is_empty():
			first_missing = built["missing"].map(func(m): return "%s: %s" % [set_id, m])

	push_error("%s: missing ROMs\n  %s" % [zip_path.get_file(), "\n  ".join(first_missing)])
	return {}


## Reads and hashes every file in <folder>/<id>.zip. Returns {} if there's no such zip.
static func _zip_index(cache: Dictionary, folder: String, id: String) -> Dictionary:
	if cache.has(id):
		return cache[id]
	var idx := {}
	var zip := ZIPReader.new()
	if zip.open(folder.path_join(id + ".zip")) == OK:
		idx = {"by_sha1": {}, "by_name": {}}
		for f in zip.get_files():
			if f.ends_with("/"):
				continue
			var data := zip.read_file(f)
			if data.is_empty():
				continue
			idx["by_name"][f.get_file().to_lower()] = data
			idx["by_sha1"][_sha1(data)] = data
		zip.close()
	cache[id] = idx
	return idx


static func _sha1(data: PackedByteArray) -> String:
	var ctx := HashingContext.new()
	ctx.start(HashingContext.HASH_SHA1)
	ctx.update(data)
	return ctx.finish().hex_encode()


## Every set that shares at least one ROM with this zip, most shared first.
static func _sets_by_overlap(db: Dictionary, idx: Dictionary) -> Array:
	var scored: Array = []
	for set_id in db:
		var hits := 0
		for region in _resolve_regions(db, set_id).values():
			for rom in region["roms"]:
				if idx["by_sha1"].has(rom.get("sha1", "")):
					hits += 1
		if hits > 0:
			scored.append([hits, set_id])
	scored.sort_custom(func(a, b): return a[0] > b[0])
	return scored.map(func(s): return s[1])


static func _parents_of(db: Dictionary, set_id: String) -> Array:
	var out: Array = []
	var id: String = db.get(set_id, {}).get("parent", "")
	while id != "" and not out.has(id):
		out.append(id)
		id = db.get(id, {}).get("parent", "")
	return out


static func _find_rom(pool: Array, rom: Dictionary) -> PackedByteArray:
	var sha: String = rom.get("sha1", "")
	for idx in pool:
		if sha != "":
			if idx["by_sha1"].has(sha):
				return idx["by_sha1"][sha]
		elif idx["by_name"].has(rom["name"].to_lower()):
			return idx["by_name"][rom["name"].to_lower()]
	return PackedByteArray()


## Returns {"regions": {...}, "missing": [descriptions]}
static func _build_regions(db: Dictionary, set_id: String, pool: Array) -> Dictionary:
	var out := {}
	var missing: Array = []
	var regions := _resolve_regions(db, set_id)
	for region_name in regions:
		var region: Dictionary = regions[region_name]
		var buf := PackedByteArray()
		buf.resize(region["size"].hex_to_int())   # zero-filled
		for rom in region["roms"]:
			var data := _find_rom(pool, rom)
			if data.is_empty():
				missing.append("%s (sha1 %s)" % [rom["name"], rom.get("sha1", "?")])
				continue
			var file_off: int = rom.get("file_offset", "0x0").hex_to_int()
			var size: int = rom["size"].hex_to_int()
			if data.size() < file_off + size:
				missing.append("%s is too small (%d bytes)" % [rom["name"], data.size()])
				continue
			var chunk := data.slice(file_off, file_off + size)
			var nibble: String = rom.get("nibble", "")
			for off in [rom["offset"]] + rom.get("reload", []):
				_copy_into(buf, off.hex_to_int(), chunk, nibble)
		out[region_name] = buf
	return {"regions": out, "missing": missing}


# A variant's regions replace its parent's, one whole region at a time
static func _resolve_regions(db: Dictionary, set_id: String) -> Dictionary:
	var game: Dictionary = db[set_id]
	var regions := {}
	if game.has("parent") and db.has(game["parent"]):
		regions = _resolve_regions(db, game["parent"])
	regions.merge(game.get("regions", {}), true)
	return regions


# Variants inherit "driver" from the parent if they don't set it
static func _driver_of(db: Dictionary, set_id: String) -> String:
	var game: Dictionary = db[set_id]
	if game.has("driver"):
		return game["driver"]
	return _driver_of(db, game["parent"])


# nibble: "" = overwrite, "low" = OR into bits 0-3, "high" = OR into bits 4-7
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
