use std::io::Cursor;

pub trait Snapshot {
    fn save(&self, out: &mut Vec<u8>);
    fn load(&mut self, cursor: &mut Cursor<&[u8]>);
}