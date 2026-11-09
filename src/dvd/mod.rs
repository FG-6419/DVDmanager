//! DVD-Strukturen: Quelle (Ordner, Laufwerk, ISO), IFO-Tabellen, VOB-Pakete.

pub mod ifo;
pub mod iso;
pub mod model;
pub mod source;
pub mod vob;

pub const SECTOR: usize = 2048;

pub type Res<T> = Result<T, String>;

#[inline]
pub fn be16(d: &[u8], o: usize) -> u16 {
    match d.get(o..o + 2) {
        Some(b) => u16::from_be_bytes([b[0], b[1]]),
        None => 0,
    }
}

#[inline]
pub fn be32(d: &[u8], o: usize) -> u32 {
    match d.get(o..o + 4) {
        Some(b) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

#[inline]
pub fn put16(d: &mut [u8], o: usize, v: u16) {
    if let Some(b) = d.get_mut(o..o + 2) {
        b.copy_from_slice(&v.to_be_bytes());
    }
}

#[inline]
pub fn put32(d: &mut [u8], o: usize, v: u32) {
    if let Some(b) = d.get_mut(o..o + 4) {
        b.copy_from_slice(&v.to_be_bytes());
    }
}
