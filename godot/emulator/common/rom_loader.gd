class_name RomLoader

const ARCADE_DB_PATH := "user://arcade_games.json"
const SKIP_EXT := ["txt", "md", "nfo", "htm", "html", "jpg", "png", "pdf"]

# Loading a game is three steps (main.gd's execute_load does them in order):
#   1. read_rom(path, inner)       the bytes (from a file, or a zip), or "this is an arcade set"
#   2. guess_system(data, path)    which system: the Rust detector looks at the data first
#                                  (headers, code fingerprints), the extension/folder last
#   3. create_system(id, ...)      the EmuSystem for that system, with its BIOS if it has one
# When the guess isn't clear, main.gd asks the user (SystemPickDialog) between 2 and 3.


## Returns one of
##   {"kind": "cart", "data": PackedByteArray, "name": save-file name, "file": file name}
##   {"kind": "arcade"}            multi-file zip: load it with load_arcade(path)
##   {}                            couldn't read it
## file_inside_zip: a specific file from a zip (from the library or the zip picker).
static func read_rom(path: String, file_inside_zip: String = "") -> Dictionary:
	if path.get_extension().to_lower() != "zip":
		var data := FileAccess.get_file_as_bytes(path)
		if data.is_empty():
			return {}
		return _cart(data, path.get_file())

	var zip := ZIPReader.new()
	if zip.open(path) != OK:
		print("Error: can't open zip %s" % path.get_file())
		return {}
	var inner := file_inside_zip
	if inner == "":
		var roms: Array = []
		for f in zip.get_files():
			if f.ends_with("/") or f.begins_with("__MACOSX") or f.get_extension().to_lower() in SKIP_EXT:
				continue
			roms.append(f)
		if roms.size() > 1:
			zip.close()
			return {"kind": "arcade"}       # several ROMs = an arcade set, matched by the zip's name
		if roms.is_empty():
			zip.close()
			print("Error: zip file is empty: %s" % path.get_file())
			return {}
		inner = roms[0]
	var data := zip.read_file(inner)
	zip.close()
	print("Load file from zip: %s" % inner)
	return _cart(data, inner.get_file()) if not data.is_empty() else {}


static func _cart(data: PackedByteArray, file: String) -> Dictionary:
	return {"kind": "cart", "data": data, "file": file,
		"name": file.get_basename().to_lower().validate_filename()}

## The Rust detector's guesses, best first: {"clear": bool, "guesses": [{"system", "score", "why"}]}.
## `path` is the full path (folder names are a hint) with the file name at the end.
static func guess_system(data: PackedByteArray, path: String) -> Dictionary:
	var lines: PackedStringArray = EmuSystem.detect_rom(data, path)
	var guesses: Array = []
	for i in range(1, lines.size()):
		var parts := lines[i].split("|", true, 2)
		guesses.append({"system": parts[0], "score": int(parts[1]), "why": parts[2]})
	return {"clear": lines.size() > 0 and lines[0] == "clear", "guesses": guesses}


## Build the system. `system_id`: nes gb gbc sms gg coleco pce a2600 a7800
static func create_system(system_id: String, data: PackedByteArray, name: String) -> Variant:
	var bios := PackedByteArray()
	match system_id:
		"coleco":
			bios = load_bios(AppSettings.bios_path("coleco", "user://BIOS/colecovision.rom"))
			if bios.is_empty():
				print("Warning: no ColecoVision BIOS; most games won't start.")
		"gb", "gbc":
			bios = load_bios(AppSettings.bios_path("gb", "user://BIOS/gbc_bios.bin"))
	var sys := EmuSystem.new()
	if not sys.load_cartridge(system_id, data, name, bios):
		return null
	return sys


## The controls/options profile for a system id (Game Boy Color shares the Game Boy's).
static func profile_id(system_id: String) -> String:
	return "gb" if system_id == "gbc" else system_id


static func load_bios(p_path: String) -> PackedByteArray:
	if p_path == "" or not FileAccess.file_exists(p_path):
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
	if not FileAccess.file_exists(ARCADE_DB_PATH):
		return {}
	var parsed = JSON.parse_string(FileAccess.get_file_as_string(ARCADE_DB_PATH))
	return parsed if parsed is Dictionary else {}
