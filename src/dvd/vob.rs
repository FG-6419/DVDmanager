//! Aufbau der 2048-Byte-Pakete in VOB-Dateien, inklusive Navigationspaketen.

use super::{be16, be32, put16, put32, SECTOR};

pub const PCI_OFF: usize = 0x2D;
pub const DSI_OFF: usize = 0x407;

#[inline]
pub fn is_pack(s: &[u8]) -> bool {
    s.len() >= 14 && s[0] == 0 && s[1] == 0 && s[2] == 1 && s[3] == 0xBA
}

/// Erkennt ein Navigationspaket am System-Header und den PCI-/DSI-Paketen.
#[inline]
pub fn is_nav(s: &[u8]) -> bool {
    s.len() >= SECTOR
        && is_pack(s)
        && s[14] == 0
        && s[15] == 0
        && s[16] == 1
        && s[17] == 0xBB
        && s[0x26] == 0
        && s[0x27] == 0
        && s[0x28] == 1
        && s[0x29] == 0xBF
        && s[0x400] == 0
        && s[0x401] == 0
        && s[0x402] == 1
        && s[0x403] == 0xBF
}

pub fn get_scr(s: &[u8]) -> u64 {
    let b = &s[4..10];
    let base = (u64::from(b[0] & 0x38) << 27)
        | (u64::from(b[0] & 0x03) << 28)
        | (u64::from(b[1]) << 20)
        | (u64::from(b[2] & 0xF8) << 12)
        | (u64::from(b[2] & 0x03) << 13)
        | (u64::from(b[3]) << 5)
        | (u64::from(b[4]) >> 3);
    let ext = (u64::from(b[4] & 0x03) << 7) | (u64::from(b[5]) >> 1);
    base * 300 + ext
}

pub fn put_scr(s: &mut [u8], scr27: u64) {
    let base = scr27 / 300;
    let ext = scr27 % 300;
    s[4] = 0x44 | (((base >> 27) & 0x38) as u8) | (((base >> 28) & 0x03) as u8);
    s[5] = ((base >> 20) & 0xFF) as u8;
    s[6] = (((base >> 12) & 0xF8) as u8) | 0x04 | (((base >> 13) & 0x03) as u8);
    s[7] = ((base >> 5) & 0xFF) as u8;
    s[8] = (((base << 3) & 0xF8) as u8) | 0x04 | (((ext >> 7) & 0x03) as u8);
    s[9] = (((ext << 1) & 0xFE) as u8) | 1;
}

/// Länge des Pack-Kopfes samt Stopfbytes.
#[inline]
pub fn pack_header_len(s: &[u8]) -> usize {
    14 + usize::from(s[13] & 7)
}

/// Wozu ein Paket gehört.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stream {
    Nav,
    Video,
    /// Physische Tonspur 0..7
    Audio(u8),
    /// Physische Untertitelspur 0..31
    Sub(u8),
    Padding,
    Other,
}

/// Bestimmt den Inhalt eines Pakets.
pub fn classify(s: &[u8]) -> Stream {
    if !is_pack(s) {
        return Stream::Other;
    }
    let h = pack_header_len(s);
    if h + 9 > s.len() || s[h] != 0 || s[h + 1] != 0 || s[h + 2] != 1 {
        return Stream::Other;
    }
    match s[h + 3] {
        0xBB => Stream::Nav,
        0xE0 => Stream::Video,
        0xBE => Stream::Padding,
        0xC0..=0xC7 => Stream::Audio(s[h + 3] & 7),
        0xBD => {
            let hdr = usize::from(s[h + 8]);
            let sub = *s.get(h + 9 + hdr).unwrap_or(&0);
            match sub {
                0x20..=0x3F => Stream::Sub(sub & 0x1F),
                0x80..=0x87 | 0x88..=0x8F | 0xA0..=0xA7 => Stream::Audio(sub & 7),
                _ => Stream::Other,
            }
        }
        _ => Stream::Other,
    }
}

/// Ist das Paket verschlüsselt (Kopfdaten zeigen CSS-Kennung)?
pub fn is_scrambled(s: &[u8]) -> bool {
    if !is_pack(s) {
        return false;
    }
    let h = pack_header_len(s);
    if h + 7 > s.len() {
        return false;
    }
    matches!(s[h + 3], 0xE0 | 0xBD | 0xC0..=0xC7) && (s[h + 6] & 0x30) != 0
}

/// Videodaten eines Pakets samt Zeitstempeln.
pub struct VideoPes<'a> {
    pub payload: &'a [u8],
    pub pts: Option<u64>,
    pub dts: Option<u64>,
    /// Sonstige Kopfdaten (PES-Erweiterung, z. B. P-STD-Puffer)
    pub ext: &'a [u8],
}

pub fn read_ts(d: &[u8]) -> u64 {
    (u64::from(d[0] & 0x0E) << 29)
        | (u64::from(d[1]) << 22)
        | (u64::from(d[2] & 0xFE) << 14)
        | (u64::from(d[3]) << 7)
        | (u64::from(d[4]) >> 1)
}

pub fn write_ts(d: &mut [u8], prefix: u8, ts: u64) {
    d[0] = (prefix << 4) | (((ts >> 29) & 0x0E) as u8) | 1;
    d[1] = ((ts >> 22) & 0xFF) as u8;
    d[2] = (((ts >> 14) & 0xFE) as u8) | 1;
    d[3] = ((ts >> 7) & 0xFF) as u8;
    d[4] = (((ts << 1) & 0xFE) as u8) | 1;
}

pub fn parse_video(s: &[u8]) -> Option<VideoPes<'_>> {
    let h = pack_header_len(s);
    if h + 9 > s.len() || s[h + 3] != 0xE0 {
        return None;
    }
    let len = usize::from(be16(s, h + 4));
    let end = (h + 6 + len).min(s.len());
    let flags = s[h + 7];
    let hdr = usize::from(s[h + 8]);
    let data = h + 9 + hdr;
    if data > end {
        return None;
    }
    let mut o = h + 9;
    let (mut pts, mut dts) = (None, None);
    match flags >> 6 {
        2 => {
            pts = Some(read_ts(&s[o..]));
            o += 5;
        }
        3 => {
            pts = Some(read_ts(&s[o..]));
            dts = Some(read_ts(&s[o + 5..]));
            o += 10;
        }
        _ => {}
    }
    let mut ext: &[u8] = &[];
    if flags & 0x01 != 0 && o < data {
        // Nur die Erweiterung ist interessant; ESCR/ES-Rate kommen auf DVDs nicht vor.
        let e0 = s[o];
        let mut n = 1;
        if e0 & 0x10 != 0 {
            n += 2;
        }
        if e0 & 0x80 == 0 && e0 & 0x40 == 0 && e0 & 0x20 == 0 && e0 & 0x01 == 0 && o + n <= data {
            ext = &s[o..o + n];
        }
    }
    Some(VideoPes { payload: &s[data..end], pts, dts, ext })
}

/// Baut ein Videopaket. `pack_hdr` sind die ersten 14 Bytes eines Packs.
/// Liefert den 2048-Byte-Sektor und nimmt so viel von `es` wie hineinpasst.
pub fn build_video_pack(
    pack_hdr: &[u8],
    ts: Option<(u64, Option<u64>)>,
    ext: &[u8],
    es: &[u8],
) -> ([u8; SECTOR], usize) {
    let mut s = [0u8; SECTOR];
    s[..14].copy_from_slice(&pack_hdr[..14]);
    s[13] = (s[13] & 0xF8) | 0;
    let ts_len = match ts {
        Some((_, Some(_))) => 10,
        Some((_, None)) => 5,
        None => 0,
    };
    let hdr_data = ts_len + ext.len();
    let room = SECTOR - 14 - 9 - hdr_data;
    let take = es.len().min(room);
    let free = room - take;
    // Bis 6 freie Bytes als Stopfung im PES-Kopf, sonst eigenes Füllpaket
    let stuffing = if free <= 6 { free } else { 0 };
    let pes_len = 3 + hdr_data + stuffing + take;
    s[14] = 0;
    s[15] = 0;
    s[16] = 1;
    s[17] = 0xE0;
    put16(&mut s, 18, pes_len as u16);
    s[20] = 0x81;
    let pts_flags = match ts {
        Some((_, Some(_))) => 0xC0,
        Some((_, None)) => 0x80,
        None => 0,
    };
    s[21] = pts_flags | u8::from(!ext.is_empty());
    s[22] = (hdr_data + stuffing) as u8;
    let mut o = 23;
    if let Some((pts, dts)) = ts {
        match dts {
            Some(d) => {
                write_ts(&mut s[o..], 3, pts);
                write_ts(&mut s[o + 5..], 1, d);
                o += 10;
            }
            None => {
                write_ts(&mut s[o..], 2, pts);
                o += 5;
            }
        }
    }
    s[o..o + ext.len()].copy_from_slice(ext);
    o += ext.len();
    for b in &mut s[o..o + stuffing] {
        *b = 0xFF;
    }
    o += stuffing;
    s[o..o + take].copy_from_slice(&es[..take]);
    o += take;
    if free > 6 {
        let pad = free;
        s[o] = 0;
        s[o + 1] = 0;
        s[o + 2] = 1;
        s[o + 3] = 0xBE;
        put16(&mut s, o + 4, (pad - 6) as u16);
        for b in &mut s[o + 6..o + pad] {
            *b = 0xFF;
        }
    }
    (s, take)
}

/// Zugriff auf die Felder eines Navigationspakets.
pub struct Nav;

impl Nav {
    pub fn vob_id(s: &[u8]) -> u16 {
        be16(s, DSI_OFF + 24)
    }
    pub fn cell_id(s: &[u8]) -> u8 {
        s[DSI_OFF + 27]
    }
    pub fn set_ids(s: &mut [u8], vob: u16, cell: u8) {
        put16(s, DSI_OFF + 24, vob);
        s[DSI_OFF + 27] = cell;
    }
    pub fn lbn(s: &[u8]) -> u32 {
        be32(s, DSI_OFF + 4)
    }
    pub fn vobu_ea(s: &[u8]) -> u32 {
        be32(s, DSI_OFF + 8)
    }
    /// Verstrichene Zeit in der Zelle (Millisekunden).
    pub fn cell_elapsed_ms(s: &[u8]) -> u64 {
        super::ifo::parse_time(s, DSI_OFF + 28).ms
    }
    pub fn set_scr(s: &mut [u8], scr27: u64) {
        put_scr(s, scr27);
        put32(s, DSI_OFF, (scr27 / 300) as u32);
    }
}

pub const SRI_END: u32 = 0x3FFF_FFFF;
/// Offsets im DSI (relativ zum Datenbeginn des DSI).
pub const DSI_SML_PBI: usize = 32;
pub const DSI_SML_AGLI: usize = 180;
pub const DSI_SRI: usize = 234;
pub const DSI_SYNCI: usize = 402;
pub const PCI_NSML_AGLI: usize = 60;
pub const PCI_HLI: usize = 96;
