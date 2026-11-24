//! Debug reports go to the console and, when the frontend gives a path, to a file too
//! (a long console scrolls the start away). `dlog!` works like `println!`.

use std::io::Write;
use std::sync::Mutex;

static LOG: Mutex<Option<(String, std::fs::File)>> = Mutex::new(None);

/// Log to this file as well ("" = console only). The file starts empty when the path
/// changes; the same path again keeps writing to it.
pub fn set_path(path: &str) {
    let Ok(mut g) = LOG.lock() else { return };
    if path.is_empty() { *g = None; return; }
    if g.as_ref().is_some_and(|(p, _)| p == path) { return; }
    match std::fs::File::create(path) {
        Ok(f) => { println!("Debug log: {path}"); *g = Some((path.to_string(), f)); }
        Err(e) => { println!("Debug log: can't write {path}: {e}"); *g = None; }
    }
}

/// The log file's path, if any.
pub fn path() -> Option<String> { LOG.lock().ok()?.as_ref().map(|(p, _)| p.clone()) }

static DATA_DIR: Mutex<String> = Mutex::new(String::new());

/// The frontend's data folder (user://, as a real path), for debug files written while
/// no log is open.
pub fn set_data_dir(dir: &str) { if let Ok(mut g) = DATA_DIR.lock() { *g = dir.to_string(); } }

/// The frontend's data folder, if it said (for files a core keeps, like the PS2's NVRAM)
pub fn data_dir() -> Option<std::path::PathBuf> {
    let d = DATA_DIR.lock().ok()?.clone();
    (!d.is_empty()).then(|| std::path::PathBuf::from(d))
}

/// Where debug files go: next to the log, else in the data folder.
pub fn dir() -> Option<std::path::PathBuf> {
    if let Some(p) = path() { return std::path::Path::new(&p).parent().map(|d| d.to_path_buf()); }
    let d = DATA_DIR.lock().ok()?.clone();
    (!d.is_empty()).then(|| std::path::PathBuf::from(d))
}

/// The frontend's audio: times its output buffer had run dry, and frames queued (for the
/// systems' speed logs).
pub static AUDIO_DRY: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
pub static AUDIO_QUEUED: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

pub fn write(line: &str) {
    println!("{line}");
    if let Ok(mut g) = LOG.lock() {
        if let Some((_, f)) = g.as_mut() { let _ = writeln!(f, "{line}"); }
    }
}

#[macro_export]
macro_rules! dlog {
    ($($t:tt)*) => { $crate::common::debug_log::write(&format!($($t)*)) };
}
