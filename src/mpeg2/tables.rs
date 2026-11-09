//! VLC-Tabellen aus ISO/IEC 13818-2 Anhang B, als Bitfolgen notiert und beim
//! ersten Gebrauch in schnelle Nachschlagetabellen übersetzt.

use std::sync::OnceLock;

pub const MB_QUANT: u8 = 1;
pub const MB_FWD: u8 = 2;
pub const MB_BWD: u8 = 4;
pub const MB_PATTERN: u8 = 8;
pub const MB_INTRA: u8 = 16;

pub const ZIGZAG: [u8; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20,
    13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59,
    52, 45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

pub const ALT_SCAN: [u8; 64] = [
    0, 8, 16, 24, 1, 9, 2, 10, 17, 25, 32, 40, 48, 56, 57, 49, 41, 33, 26, 18, 3, 11, 4, 12, 19,
    27, 34, 42, 50, 58, 35, 43, 51, 59, 20, 28, 5, 13, 6, 14, 21, 29, 36, 44, 52, 60, 37, 45, 53,
    61, 22, 30, 7, 15, 23, 31, 38, 46, 54, 62, 39, 47, 55, 63,
];

pub const DEFAULT_INTRA: [u8; 64] = [
    8, 16, 19, 22, 26, 27, 29, 34, 16, 16, 22, 24, 27, 29, 34, 37, 19, 22, 26, 27, 29, 34, 34, 38,
    22, 22, 26, 27, 29, 34, 37, 40, 22, 26, 27, 29, 32, 35, 40, 48, 26, 27, 29, 32, 35, 40, 48, 58,
    26, 27, 29, 34, 38, 46, 56, 69, 27, 29, 35, 38, 46, 56, 69, 83,
];

pub const QSCALE_NONLINEAR: [u8; 32] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 10, 12, 14, 16, 18, 20, 22, 24, 28, 32, 36, 40, 44, 48, 52, 56, 64,
    72, 80, 88, 96, 104, 112,
];

/// Tatsächlicher Quantisierer zu einem 5-Bit-Code.
#[inline]
pub fn qscale(code: u8, nonlinear: bool) -> u32 {
    if nonlinear {
        u32::from(QSCALE_NONLINEAR[(code & 31) as usize])
    } else {
        u32::from(code & 31) * 2
    }
}

/// Bitfolge als (Code, Länge).
pub const fn bin(s: &str) -> (u32, u8) {
    let b = s.as_bytes();
    let mut v = 0u32;
    let mut n = 0u8;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'0' || b[i] == b'1' {
            v = (v << 1) | (b[i] - b'0') as u32;
            n += 1;
        }
        i += 1;
    }
    (v, n)
}

/// Einfache Nachschlagetabelle mit fester Adressbreite.
pub struct Vlc {
    pub bits: u32,
    /// (Wert, Länge); Länge 0 = ungültiger Code.
    pub table: Vec<(i16, u8)>,
}

impl Vlc {
    fn build(bits: u32, codes: &[(&str, i16)]) -> Self {
        let mut table = vec![(0i16, 0u8); 1usize << bits];
        for (s, val) in codes {
            let (c, n) = bin(s);
            let n32 = u32::from(n);
            assert!(n32 <= bits);
            let shift = bits - n32;
            let start = (c << shift) as usize;
            for e in table.iter_mut().skip(start).take(1usize << shift) {
                *e = (*val, n);
            }
        }
        Self { bits, table }
    }

    #[inline]
    pub fn lookup(&self, peek: u32) -> (i16, u8) {
        self.table[peek as usize]
    }
}

pub const ADDR_ESCAPE: i16 = 34;

const ADDR_INC: &[(&str, i16)] = &[
    ("1", 1), ("011", 2), ("010", 3), ("0011", 4), ("0010", 5), ("00011", 6), ("00010", 7),
    ("0000111", 8), ("0000110", 9), ("00001011", 10), ("00001010", 11), ("00001001", 12),
    ("00001000", 13), ("00000111", 14), ("00000110", 15), ("0000010111", 16), ("0000010110", 17),
    ("0000010101", 18), ("0000010100", 19), ("0000010011", 20), ("0000010010", 21),
    ("00000100011", 22), ("00000100010", 23), ("00000100001", 24), ("00000100000", 25),
    ("00000011111", 26), ("00000011110", 27), ("00000011101", 28), ("00000011100", 29),
    ("00000011011", 30), ("00000011010", 31), ("00000011001", 32), ("00000011000", 33),
    ("00000001000", ADDR_ESCAPE),
];

const MB_TYPE_I: &[(&str, u8)] = &[("1", MB_INTRA), ("01", MB_QUANT | MB_INTRA)];
const MB_TYPE_P: &[(&str, u8)] = &[
    ("1", MB_FWD | MB_PATTERN),
    ("01", MB_PATTERN),
    ("001", MB_FWD),
    ("00011", MB_INTRA),
    ("00010", MB_QUANT | MB_FWD | MB_PATTERN),
    ("00001", MB_QUANT | MB_PATTERN),
    ("000001", MB_QUANT | MB_INTRA),
];
const MB_TYPE_B: &[(&str, u8)] = &[
    ("10", MB_FWD | MB_BWD),
    ("11", MB_FWD | MB_BWD | MB_PATTERN),
    ("010", MB_BWD),
    ("011", MB_BWD | MB_PATTERN),
    ("0010", MB_FWD),
    ("0011", MB_FWD | MB_PATTERN),
    ("00011", MB_INTRA),
    ("00010", MB_QUANT | MB_FWD | MB_BWD | MB_PATTERN),
    ("000011", MB_QUANT | MB_FWD | MB_PATTERN),
    ("000010", MB_QUANT | MB_BWD | MB_PATTERN),
    ("000001", MB_QUANT | MB_INTRA),
];

const CBP: &[(&str, i16)] = &[
    ("111", 60), ("1101", 4), ("1100", 8), ("1011", 16), ("1010", 32), ("10011", 12), ("10010", 48),
    ("10001", 20), ("10000", 40), ("01111", 28), ("01110", 44), ("01101", 52), ("01100", 56),
    ("01011", 1), ("01010", 61), ("01001", 2), ("01000", 62), ("001111", 24), ("001110", 36),
    ("001101", 3), ("001100", 63), ("0010111", 5), ("0010110", 9), ("0010101", 17), ("0010100", 33),
    ("0010011", 6), ("0010010", 10), ("0010001", 18), ("0010000", 34), ("00011111", 7),
    ("00011110", 11), ("00011101", 19), ("00011100", 35), ("00011011", 13), ("00011010", 49),
    ("00011001", 21), ("00011000", 41), ("00010111", 14), ("00010110", 50), ("00010101", 22),
    ("00010100", 42), ("00010011", 15), ("00010010", 51), ("00010001", 23), ("00010000", 43),
    ("00001111", 25), ("00001110", 37), ("00001101", 26), ("00001100", 38), ("00001011", 29),
    ("00001010", 45), ("00001001", 53), ("00001000", 57), ("00000111", 30), ("00000110", 46),
    ("00000101", 54), ("00000100", 58), ("000000111", 31), ("000000110", 47), ("000000101", 55),
    ("000000100", 59), ("000000011", 27), ("000000010", 39), ("000000001", 0),
];

const MOTION: &[(&str, i16)] = &[
    ("1", 0), ("010", 1), ("011", -1), ("0010", 2), ("0011", -2), ("00010", 3), ("00011", -3),
    ("0000110", 4), ("0000111", -4), ("00001010", 5), ("00001011", -5), ("00001000", 6),
    ("00001001", -6), ("00000110", 7), ("00000111", -7), ("0000010110", 8), ("0000010111", -8),
    ("0000010100", 9), ("0000010101", -9), ("0000010010", 10), ("0000010011", -10),
    ("00000100010", 11), ("00000100011", -11), ("00000100000", 12), ("00000100001", -12),
    ("00000011110", 13), ("00000011111", -13), ("00000011100", 14), ("00000011101", -14),
    ("00000011010", 15), ("00000011011", -15), ("00000011000", 16), ("00000011001", -16),
];

const DC_LUMA: &[(&str, i16)] = &[
    ("100", 0), ("00", 1), ("01", 2), ("101", 3), ("110", 4), ("1110", 5), ("11110", 6),
    ("111110", 7), ("1111110", 8), ("11111110", 9), ("111111110", 10), ("111111111", 11),
];
const DC_CHROMA: &[(&str, i16)] = &[
    ("00", 0), ("01", 1), ("10", 2), ("110", 3), ("1110", 4), ("11110", 5), ("111110", 6),
    ("1111110", 7), ("11111110", 8), ("111111110", 9), ("1111111110", 10), ("1111111111", 11),
];

pub const TOK_EOB: u8 = 255;
pub const TOK_ESC: u8 = 254;

type DctCode = (&'static str, u8, u8);

/// Gemeinsamer Teil beider Tabellen (lange Codes).
const DCT_COMMON: &[DctCode] = &[
    ("000000011111", 17, 1), ("000000011010", 18, 1), ("000000011001", 19, 1), ("000000010111", 20, 1),
    ("000000010110", 21, 1), ("000000011100", 3, 3), ("000000010010", 4, 3), ("000000011110", 6, 2),
    ("000000010101", 7, 2), ("000000010001", 8, 2),
    ("0000000010110", 1, 6), ("0000000010101", 1, 7), ("0000000010100", 2, 5), ("0000000010011", 3, 4),
    ("0000000010010", 5, 3), ("0000000010001", 9, 2), ("0000000010000", 10, 2), ("0000000011111", 22, 1),
    ("0000000011110", 23, 1), ("0000000011101", 24, 1), ("0000000011100", 25, 1), ("0000000011011", 26, 1),
    ("00000000011111", 0, 16), ("00000000011110", 0, 17), ("00000000011101", 0, 18), ("00000000011100", 0, 19),
    ("00000000011011", 0, 20), ("00000000011010", 0, 21), ("00000000011001", 0, 22), ("00000000011000", 0, 23),
    ("00000000010111", 0, 24), ("00000000010110", 0, 25), ("00000000010101", 0, 26), ("00000000010100", 0, 27),
    ("00000000010011", 0, 28), ("00000000010010", 0, 29), ("00000000010001", 0, 30), ("00000000010000", 0, 31),
    ("000000000011000", 0, 32), ("000000000010111", 0, 33), ("000000000010110", 0, 34), ("000000000010101", 0, 35),
    ("000000000010100", 0, 36), ("000000000010011", 0, 37), ("000000000010010", 0, 38), ("000000000010001", 0, 39),
    ("000000000010000", 0, 40), ("000000000011111", 1, 8), ("000000000011110", 1, 9), ("000000000011101", 1, 10),
    ("000000000011100", 1, 11), ("000000000011011", 1, 12), ("000000000011010", 1, 13), ("000000000011001", 1, 14),
    ("0000000000010011", 1, 15), ("0000000000010010", 1, 16), ("0000000000010001", 1, 17), ("0000000000010000", 1, 18),
    ("0000000000010100", 6, 3), ("0000000000011010", 11, 2), ("0000000000011001", 12, 2), ("0000000000011000", 13, 2),
    ("0000000000010111", 14, 2), ("0000000000010110", 15, 2), ("0000000000010101", 16, 2), ("0000000000011111", 27, 1),
    ("0000000000011110", 28, 1), ("0000000000011101", 29, 1), ("0000000000011100", 30, 1), ("0000000000011011", 31, 1),
];

/// Tabelle B-14; der Sonderfall "1s" für den ersten Koeffizienten wird im Code behandelt.
const DCT_ZERO: &[DctCode] = &[
    ("10", TOK_EOB, 0), ("000001", TOK_ESC, 0),
    ("11", 0, 1), ("011", 1, 1), ("0100", 0, 2), ("0101", 2, 1), ("00101", 0, 3), ("00111", 3, 1),
    ("00110", 4, 1), ("000110", 1, 2), ("000111", 5, 1), ("000101", 6, 1), ("000100", 7, 1),
    ("0000110", 0, 4), ("0000100", 2, 2), ("0000111", 8, 1), ("0000101", 9, 1),
    ("00100110", 0, 5), ("00100001", 0, 6), ("00100101", 1, 3), ("00100100", 3, 2),
    ("00100111", 10, 1), ("00100011", 11, 1), ("00100010", 12, 1), ("00100000", 13, 1),
    ("0000001010", 0, 7), ("0000001100", 1, 4), ("0000001011", 2, 3), ("0000001111", 4, 2),
    ("0000001001", 5, 2), ("0000001110", 14, 1), ("0000001101", 15, 1), ("0000001000", 16, 1),
    ("000000011101", 0, 8), ("000000011000", 0, 9), ("000000010011", 0, 10), ("000000010000", 0, 11),
    ("000000011011", 1, 5), ("000000010100", 2, 4),
    ("0000000011010", 0, 12), ("0000000011001", 0, 13), ("0000000011000", 0, 14), ("0000000010111", 0, 15),
];

/// Tabelle B-15 (intra_vlc_format = 1).
const DCT_ONE: &[DctCode] = &[
    ("0110", TOK_EOB, 0), ("000001", TOK_ESC, 0),
    ("10", 0, 1), ("010", 1, 1), ("110", 0, 2), ("00101", 2, 1), ("0111", 0, 3), ("00111", 3, 1),
    ("000110", 4, 1), ("00110", 1, 2), ("000111", 5, 1), ("0000110", 6, 1), ("0000100", 7, 1),
    ("11100", 0, 4), ("0000111", 2, 2), ("0000101", 8, 1), ("1111000", 9, 1),
    ("11101", 0, 5), ("000101", 0, 6), ("1111001", 1, 3), ("00100110", 3, 2),
    ("1111010", 10, 1), ("00100001", 11, 1), ("00100101", 12, 1), ("00100100", 13, 1),
    ("000100", 0, 7), ("00100111", 1, 4), ("11111100", 2, 3), ("11111101", 4, 2),
    ("000000100", 5, 2), ("000000101", 14, 1), ("000000111", 15, 1), ("0000001101", 16, 1),
    ("1111011", 0, 8), ("1111100", 0, 9), ("00100011", 0, 10), ("00100010", 0, 11),
    ("00100000", 1, 5), ("0000001100", 2, 4),
    ("11111010", 0, 12), ("11111011", 0, 13), ("11111110", 0, 14), ("11111111", 0, 15),
];

/// Eintrag der schnellen DCT-Tabelle.
#[derive(Clone, Copy, Default)]
pub struct DctEntry {
    pub run: u8,
    pub level: u8,
    pub len: u8,
}

pub struct DctTable {
    pub dec: Vec<DctEntry>,
    /// Kodieren: enc[run][level] = (Code, Länge) ohne Vorzeichen; Länge 0 = Escape nötig.
    pub enc: [[(u16, u8); 41]; 32],
    pub eob: (u16, u8),
}

impl DctTable {
    fn build(lists: [&[DctCode]; 2]) -> Self {
        let mut dec = vec![DctEntry::default(); 1 << 16];
        let mut enc = [[(0u16, 0u8); 41]; 32];
        let mut eob = (0u16, 0u8);
        for list in lists {
            for (s, run, level) in list {
                let (c, n) = bin(s);
                let shift = 16 - u32::from(n);
                let start = (c << shift) as usize;
                for e in dec.iter_mut().skip(start).take(1usize << shift) {
                    *e = DctEntry { run: *run, level: *level, len: n };
                }
                if *run == TOK_EOB {
                    eob = (c as u16, n);
                } else if *run != TOK_ESC {
                    enc[*run as usize][*level as usize] = (c as u16, n);
                }
            }
        }
        Self { dec, enc, eob }
    }
}

pub struct Tables {
    pub addr: Vlc,
    pub cbp: Vlc,
    pub motion: Vlc,
    pub dc_luma: Vlc,
    pub dc_chroma: Vlc,
    /// mb_type[picture_coding_type - 1]
    pub mb_type: [Vec<(u8, u8)>; 3],
    /// Kodieren: (Code, Länge) für jede Flag-Kombination, je Bildtyp.
    pub mb_enc: [[(u8, u8); 32]; 3],
    pub dct: [DctTable; 2],
    pub cbp_enc: [(u16, u8); 64],
    pub addr_enc: [(u16, u8); 34],
    pub motion_enc: [(u16, u8); 33],
    pub dc_luma_enc: [(u16, u8); 12],
    pub dc_chroma_enc: [(u16, u8); 12],
}

fn enc_arr<const N: usize>(codes: &[(&str, i16)], offset: i32) -> [(u16, u8); N] {
    let mut a = [(0u16, 0u8); N];
    for (s, v) in codes {
        let idx = i32::from(*v) + offset;
        if idx >= 0 && (idx as usize) < N {
            let (c, n) = bin(s);
            a[idx as usize] = (c as u16, n);
        }
    }
    a
}

pub fn tables() -> &'static Tables {
    static T: OnceLock<Tables> = OnceLock::new();
    T.get_or_init(|| {
        let mut mb_type: [Vec<(u8, u8)>; 3] = [vec![(0, 0); 64], vec![(0, 0); 64], vec![(0, 0); 64]];
        let mut mb_enc = [[(0u8, 0u8); 32]; 3];
        for (idx, list) in [MB_TYPE_I, MB_TYPE_P, MB_TYPE_B].iter().enumerate() {
            for (s, flags) in list.iter() {
                let (c, n) = bin(s);
                let shift = 6 - u32::from(n);
                let start = (c << shift) as usize;
                for e in mb_type[idx].iter_mut().skip(start).take(1usize << shift) {
                    *e = (*flags, n);
                }
                mb_enc[idx][*flags as usize] = (c as u8, n);
            }
        }
        Tables {
            addr: Vlc::build(11, ADDR_INC),
            cbp: Vlc::build(9, CBP),
            motion: Vlc::build(11, MOTION),
            dc_luma: Vlc::build(9, DC_LUMA),
            dc_chroma: Vlc::build(10, DC_CHROMA),
            mb_type,
            mb_enc,
            dct: [
                DctTable::build([DCT_ZERO, DCT_COMMON]),
                DctTable::build([DCT_ONE, DCT_COMMON]),
            ],
            cbp_enc: enc_arr(CBP, 0),
            addr_enc: enc_arr(ADDR_INC, 0),
            motion_enc: enc_arr(MOTION, 16),
            dc_luma_enc: enc_arr(DC_LUMA, 0),
            dc_chroma_enc: enc_arr(DC_CHROMA, 0),
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Ein vollständiger Präfixcode hat die Kraft-Summe 1. Tippfehler fallen so auf.
    fn kraft(lists: &[&[DctCode]]) -> f64 {
        let mut sum = 0.0;
        for l in lists {
            for (s, run, _) in l.iter() {
                let (_, n) = bin(s);
                // Vorzeichenbit verdoppelt nichts: jeder Code belegt seinen Präfixraum.
                let _ = run;
                sum += 0.5f64.powi(i32::from(n));
            }
        }
        sum
    }

    #[test]
    fn dct_tables_are_prefix_free() {
        for lists in [[DCT_ZERO, DCT_COMMON], [DCT_ONE, DCT_COMMON]] {
            let mut codes: Vec<(u32, u8)> = Vec::new();
            for l in lists {
                for (s, _, _) in l.iter() {
                    codes.push(bin(s));
                }
            }
            for (i, a) in codes.iter().enumerate() {
                for (j, b) in codes.iter().enumerate() {
                    if i != j && a.1 <= b.1 {
                        assert_ne!(b.0 >> (b.1 - a.1), a.0, "Präfixkonflikt {a:?} {b:?}");
                    }
                }
            }
            assert!(kraft(&lists) < 1.0);
        }
    }

    #[test]
    fn table_counts() {
        // 111 Lauf/Pegel-Paare plus EOB und Escape (ISO 13818-2)
        assert_eq!(DCT_ZERO.len() + DCT_COMMON.len(), 113);
        assert_eq!(DCT_ONE.len() + DCT_COMMON.len(), 113);
    }
}
