// Arcade save states: a small header (magic, driver version, set name) followed by the
// driver's state encoded with bincode. ROMs, decoded graphics and audio buffers are not
// stored; drivers mark them #[serde(skip)] and take them back from the running machine
// after decoding ("keep_static").

use serde::{Serialize, de::DeserializeOwned};

const MAGIC: &[u8; 4] = b"MSAS";

fn config() -> impl bincode::config::Config {
    bincode::config::standard().with_fixed_int_encoding()
}

pub fn encode<T: Serialize>(set: &str, version: u32, value: &T) -> Vec<u8> {
    let mut out = Vec::with_capacity(64 * 1024);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&version.to_le_bytes());
    let name = set.as_bytes();
    out.extend_from_slice(&(name.len() as u16).to_le_bytes());
    out.extend_from_slice(name);
    match bincode::serde::encode_to_vec(value, config()) {
        Ok(body) => { out.extend_from_slice(&body); out }
        Err(e) => { println!("save state encode failed: {e:?}"); Vec::new() }
    }
}

pub fn decode<T: DeserializeOwned>(set: &str, version: u32, data: &[u8]) -> Result<T, String> {
    if data.len() < 10 || &data[0..4] != MAGIC {
        return Err("not an arcade save state".into());
    }
    let v = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
    if v != version {
        return Err(format!("save state version {v}, this build uses {version}"));
    }
    let n = u16::from_le_bytes([data[8], data[9]]) as usize;
    let name = data.get(10..10 + n).ok_or("truncated save state")?;
    if name != set.as_bytes() {
        return Err(format!("save state is for '{}', not '{set}'", String::from_utf8_lossy(name)));
    }
    let (value, _) = bincode::serde::decode_from_slice(&data[10 + n..], config())
        .map_err(|e| format!("couldn't decode save state: {e:?}"))?;
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn header_checks() {
        let d = encode("pacman", 3, &(1u8, vec![2u16, 3]));
        let back: (u8, Vec<u16>) = decode("pacman", 3, &d).unwrap();
        assert_eq!(back, (1, vec![2, 3]));
        assert!(decode::<(u8, Vec<u16>)>("mspacman", 3, &d).is_err());
        assert!(decode::<(u8, Vec<u16>)>("pacman", 4, &d).is_err());
    }
}

/// Test helper: run, save, run on; load, run again; the two runs must match exactly.
#[cfg(test)]
pub(crate) fn check_round_trip(sys: &mut dyn crate::common::system::System, frames_before: usize, frames_after: usize) {
    for _ in 0..frames_before { sys.run_frame(0); }
    let _ = sys.drain_audio();
    let saved = sys.save_state();
    assert!(!saved.is_empty(), "save_state returned nothing");
    let mut fb1 = Vec::new();
    let mut au1 = Vec::new();
    for i in 0..frames_after {
        sys.run_frame(if i % 7 == 0 { crate::common::input::A } else { 0 });
        fb1.push(sys.framebuffer().to_vec());
        au1.extend(sys.drain_audio());
    }
    sys.load_state(&saved).expect("load_state");
    let _ = sys.drain_audio();
    for i in 0..frames_after {
        sys.run_frame(if i % 7 == 0 { crate::common::input::A } else { 0 });
        assert!(fb1[i] == sys.framebuffer(), "frame {i} differs after loading the state");
    }
    let au2 = sys.drain_audio();
    assert_eq!(au1.len(), au2.len(), "audio length");
    assert!(au1 == au2, "audio differs after loading the state");
}
