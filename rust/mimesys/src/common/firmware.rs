//! Firmware bundles from the frontend: a 4-byte magic ("SNFW", "MCDB", ...), then for
//! each file a name length (u8), the file name, a u32 little-endian length and the data.

/// Every (lower-case file name, data) in a bundle with this magic; empty if it isn't one.
pub fn files(bundle: &[u8], magic: &[u8; 4]) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    if !bundle.starts_with(magic) { return out; }
    let mut i = 4;
    while i < bundle.len() {
        let n = bundle[i] as usize;
        let Some(name) = bundle.get(i + 1..i + 1 + n) else { break };
        let name = String::from_utf8_lossy(name).to_lowercase();
        i += 1 + n;
        let Some(len) = bundle.get(i..i + 4) else { break };
        let len = u32::from_le_bytes(len.try_into().unwrap()) as usize;
        i += 4;
        let Some(data) = bundle.get(i..i + len) else { break };
        out.push((name, data.to_vec()));
        i += len;
    }
    out
}

/// Build a bundle (for tests and tools).
pub fn bundle(magic: &[u8; 4], files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut b = magic.to_vec();
    for (name, data) in files {
        b.push(name.len() as u8);
        b.extend(name.as_bytes());
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(*data);
    }
    b
}
