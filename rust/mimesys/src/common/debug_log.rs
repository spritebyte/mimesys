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
