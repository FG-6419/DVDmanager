//! Ton-Blenden ohne Neu-Kodieren: Die Lautstärke wird direkt im Datenstrom verändert,
//! jede Rahmengröße bleibt gleich.
//!
//! - LPCM: Abtastwerte werden skaliert.
//! - MPEG-Ton (Layer II): Die Skalenfaktoren (2-dB-Stufen, je 8 ms) werden erhöht.
//! - Dolby Digital (AC-3): Die Mantissen werden im selben Quantisierer neu gerundet,
//!   Exponenten und Bitzuteilung bleiben, wie sie sind. Dazu wird die Bitzuteilung
//!   nach ATSC A/52 nachgerechnet, um die Mantissen im Rahmen zu finden. Danach werden
//!   die beiden Prüfsummen neu berechnet. Lässt sich ein Rahmen nicht sicher lesen,
//!   bleibt er unverändert.

use crate::dvd::vob::{pack_header_len, read_ts};
use crate::dvd::SECTOR;
use crate::engine::audio::{split_frames, Codec};

/// Eine VOBU der Ausgabe samt Zeitbezug: `t0` ist ihr Beginn auf der Zeitachse des
/// Eintrags (ms), `s_ptm` die Anfangszeit laut Navigationspaket im Original (90 kHz).
pub struct FadeVobu<'a> {
    pub data: &'a mut Vec<u8>,
    pub t0: f64,
    pub s_ptm: u32,
}

/// Lautstärke (0..1) an einer Stelle der Zeitachse des Eintrags.
pub type GainFn<'a> = &'a dyn Fn(f64) -> f32;

struct Piece {
    vobu: usize,
    start: usize,
    end: usize,
    time: Option<f64>,
    /// LPCM: (Bits, Abtastrate, Kanäle)
    lpcm: Option<(u8, u32, u8)>,
}

/// Blendet den Ton aller Spuren in diesen VOBUs gemäß `gain`.
pub fn fade_audio(vobus: &mut [FadeVobu], gain: GainFn) {
    // Pakete je Spur sammeln: (Strom-Kennung, Codec) -> Stücke
    let mut streams: Vec<((u8, Codec), Vec<Piece>)> = Vec::new();
    for (vi, v) in vobus.iter().enumerate() {
        for (si, s) in v.data.chunks_exact(SECTOR).enumerate() {
            let Some((key, mut piece)) = audio_piece(s) else { continue };
            piece.vobu = vi;
            piece.start += si * SECTOR;
            piece.end += si * SECTOR;
            piece.time = piece.time.map(|pts| entry_time(pts as u64, v.t0, v.s_ptm));
            match streams.iter_mut().find(|(k, _)| *k == key) {
                Some((_, list)) => list.push(piece),
                None => streams.push((key, vec![piece])),
            }
        }
    }
    for ((_, codec), pieces) in streams {
        match codec {
            Codec::Lpcm => fade_lpcm(vobus, &pieces, gain),
            c => fade_frames(vobus, &pieces, c, gain),
        }
    }
}

fn entry_time(pts: u64, t0: f64, s_ptm: u32) -> f64 {
    let mut d = pts as i64 - i64::from(s_ptm);
    const WRAP: i64 = 1 << 33;
    if d > WRAP / 2 {
        d -= WRAP;
    } else if d < -WRAP / 2 {
        d += WRAP;
    }
    t0 + d as f64 / 90.0
}

/// Ton-Paket: Kennung, Codec und Lage der Nutzdaten im Sektor. Die Zeit steht zunächst
/// als roher Zeitstempel in `time`.
fn audio_piece(s: &[u8]) -> Option<((u8, Codec), Piece)> {
    let h = pack_header_len(s);
    if h + 9 > s.len() || s[h] != 0 || s[h + 1] != 0 || s[h + 2] != 1 {
        return None;
    }
    let id = s[h + 3];
    let len = usize::from(u16::from_be_bytes([s[h + 4], s[h + 5]]));
    let end = (h + 6 + len).min(s.len());
    let hdr = usize::from(s[h + 8]);
    let pts = if s[h + 7] & 0x80 != 0 && h + 14 <= s.len() { Some(read_ts(&s[h + 9..]) as f64) } else { None };
    let p = h + 9 + hdr;
    let piece = |start: usize, lpcm| Piece { vobu: 0, start, end, time: pts, lpcm };
    let out = match id {
        0xC0..=0xC7 => ((id, Codec::Mpeg), piece(p, None)),
        0xBD => {
            let sub = *s.get(p)?;
            match sub {
                0x80..=0x87 => ((sub, Codec::Ac3), piece(p + 4, None)),
                0xA0..=0xA7 => {
                    let info = *s.get(p + 5)?;
                    let bits = match info >> 6 {
                        0 => 16,
                        1 => 20,
                        _ => 24,
                    };
                    let rate = if (info >> 4) & 3 == 1 { 96_000 } else { 48_000 };
                    let ch = (info & 7) + 1;
                    ((sub, Codec::Lpcm), piece(p + 7, Some((bits, rate, ch))))
                }
                _ => return None,
            }
        }
        _ => return None,
    };
    (out.1.start <= out.1.end).then_some(out)
}

fn fade_lpcm(vobus: &mut [FadeVobu], pieces: &[Piece], gain: GainFn) {
    let mut next: Option<f64> = None;
    for p in pieces {
        let Some((bits, rate, ch)) = p.lpcm else { continue };
        let ch = usize::from(ch.max(1));
        let data = &mut vobus[p.vobu].data[p.start..p.end];
        let t = p.time.or(next).unwrap_or(0.0);
        let ms_per = 1000.0 / f64::from(rate);
        let (group, per_group, high) = match bits {
            16 => (2 * ch, 1, 2 * ch),
            20 => (5 * ch, 2, 4 * ch),
            _ => (6 * ch, 2, 4 * ch),
        };
        let groups = data.len() / group;
        for g in 0..groups {
            let at = t + (g * per_group) as f64 * ms_per;
            let a = gain(at);
            if a >= 0.9995 {
                continue;
            }
            let base = g * group;
            for k in (0..high).step_by(2) {
                let v = i16::from_be_bytes([data[base + k], data[base + k + 1]]);
                let nv = (f32::from(v) * a.max(0.0)).round().clamp(-32768.0, 32767.0) as i16;
                data[base + k..base + k + 2].copy_from_slice(&nv.to_be_bytes());
            }
            // Untere Bits (20/24 Bit) fallen weg
            data[base + high..base + group].fill(0);
        }
        next = Some(t + (groups * per_group) as f64 * ms_per);
    }
}

fn fade_frames(vobus: &mut [FadeVobu], pieces: &[Piece], codec: Codec, gain: GainFn) {
    // Zusammenhängender Strom und Zeitmarken
    let mut es = Vec::new();
    let mut marks: Vec<(usize, f64)> = Vec::new();
    for p in pieces {
        if let Some(t) = p.time {
            marks.push((es.len(), t));
        }
        es.extend_from_slice(&vobus[p.vobu].data[p.start..p.end]);
    }
    let frames = split_frames(&es, codec);
    let mut changed = false;
    let mut mi = 0;
    let mut next: Option<f64> = None;
    for (start, len) in frames {
        let mut t = None;
        while mi < marks.len() && marks[mi].0 <= start {
            t = Some(marks[mi].1);
            mi += 1;
        }
        let Some(t) = t.or(next) else { continue };
        let frame = &mut es[start..start + len];
        let dur = match codec {
            Codec::Ac3 => {
                let mut g = [1f32; 6];
                for (b, v) in g.iter_mut().enumerate() {
                    *v = gain(t + (b as f64 + 0.5) * 256.0 / 48.0);
                }
                if g.iter().any(|v| *v < 0.9995) {
                    changed |= ac3::scale_frame(frame, &g);
                }
                32.0
            }
            _ => {
                let Some(rate) = mpa::sample_rate(frame) else { continue };
                let part = 384.0 * 1000.0 / f64::from(rate);
                let mut g = [1f32; 3];
                for (k, v) in g.iter_mut().enumerate() {
                    *v = gain(t + (k as f64 + 0.5) * part);
                }
                if g.iter().any(|v| *v < 0.9995) {
                    changed |= mpa::scale_frame(frame, &g);
                }
                part * 3.0
            }
        };
        next = Some(t + dur);
    }
    if !changed {
        return;
    }
    // Zurückschreiben
    let mut at = 0;
    for p in pieces {
        let n = p.end - p.start;
        vobus[p.vobu].data[p.start..p.end].copy_from_slice(&es[at..at + n]);
        at += n;
    }
}

// ---------------------------------------------------------------------------
// Bits lesen und an derselben Stelle wieder schreiben
// ---------------------------------------------------------------------------

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
    ok: bool,
}

impl<'a> Bits<'a> {
    fn new(d: &'a [u8], pos: usize) -> Self {
        Self { d, pos, ok: true }
    }

    fn get(&mut self, n: usize) -> u32 {
        let v = peek(self.d, self.pos, n);
        if self.pos + n > self.d.len() * 8 {
            self.ok = false;
        }
        self.pos += n;
        v.unwrap_or(0)
    }

    fn bit(&mut self) -> bool {
        self.get(1) != 0
    }
}

fn peek(d: &[u8], pos: usize, n: usize) -> Option<u32> {
    if pos + n > d.len() * 8 {
        return None;
    }
    let mut v = 0u32;
    for i in 0..n {
        let p = pos + i;
        v = (v << 1) | u32::from((d[p >> 3] >> (7 - (p & 7))) & 1);
    }
    Some(v)
}

fn put(d: &mut [u8], pos: usize, n: usize, v: u32) {
    for i in 0..n {
        let p = pos + i;
        let bit = ((v >> (n - 1 - i)) & 1) as u8;
        let mask = 1u8 << (7 - (p & 7));
        if bit != 0 {
            d[p >> 3] |= mask;
        } else {
            d[p >> 3] &= !mask;
        }
    }
}

/// Faktor 0..1 auf eine Stufe eines symmetrischen Quantisierers mit `levels` Stufen.
fn scale_sym(k: u32, levels: u32, g: f32) -> u32 {
    if k >= levels {
        return k;
    }
    let l = levels as f32;
    let v = (2.0 * k as f32 - (l - 1.0)) / l * g;
    ((v * l + l - 1.0) / 2.0).round().clamp(0.0, l - 1.0) as u32
}

// ---------------------------------------------------------------------------
// MPEG-1/2 Layer II
// ---------------------------------------------------------------------------

mod mpa {
    use super::{peek, put, Bits};

    pub fn sample_rate(f: &[u8]) -> Option<u32> {
        if f.len() < 4 {
            return None;
        }
        let version = (f[1] >> 3) & 3;
        let idx = usize::from((f[2] >> 2) & 3);
        let rates = if version == 3 { [44_100, 48_000, 32_000] } else { [22_050, 24_000, 16_000] };
        rates.get(idx).copied()
    }

    /// Bitbreiten der Zuteilung je Teilband (ISO 11172-3, Tabellen B.2a-d, ISO 13818-3).
    fn nbal(table: usize) -> Vec<usize> {
        let mut v = Vec::new();
        let mut push = |n: usize, count: usize| v.extend(std::iter::repeat_n(n, count));
        match table {
            0 => {
                push(4, 11);
                push(3, 12);
                push(2, 4);
            }
            1 => {
                push(4, 11);
                push(3, 12);
                push(2, 7);
            }
            2 => {
                push(4, 2);
                push(3, 6);
            }
            3 => {
                push(4, 2);
                push(3, 10);
            }
            _ => {
                push(4, 4);
                push(3, 7);
                push(2, 19);
            }
        }
        v
    }

    /// Skaliert einen Layer-II-Rahmen; `g` gilt je Drittel (384 Abtastwerte).
    pub fn scale_frame(f: &mut [u8], g: &[f32; 3]) -> bool {
        if f.len() < 8 || f[0] != 0xFF || f[1] & 0xE0 != 0xE0 {
            return false;
        }
        let version = (f[1] >> 3) & 3;
        let layer = (f[1] >> 1) & 3;
        if layer != 2 || version == 1 {
            return false;
        }
        let lsf = version != 3;
        let protected = f[1] & 1 == 0;
        let br_idx = usize::from(f[2] >> 4);
        const L2: [u32; 16] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0];
        const L2_LSF: [u32; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];
        let kbps = if lsf { L2_LSF[br_idx] } else { L2[br_idx] };
        if kbps == 0 {
            return false;
        }
        let Some(freq) = sample_rate(f) else { return false };
        let mode = f[3] >> 6;
        let mode_ext = usize::from((f[3] >> 4) & 3);
        let nch = if mode == 3 { 1 } else { 2 };
        let table = if lsf {
            4
        } else {
            let ch_br = kbps / nch as u32;
            if (freq == 48_000 && ch_br >= 56) || (56..=80).contains(&ch_br) {
                0
            } else if freq != 48_000 && ch_br >= 96 {
                1
            } else if freq != 32_000 && ch_br <= 48 {
                2
            } else {
                3
            }
        };
        let nbal = nbal(table);
        let sblimit = nbal.len();
        let bound = if mode == 1 { ((mode_ext + 1) * 4).min(sblimit) } else { sblimit };
        let mut r = Bits::new(f, 32 + if protected { 16 } else { 0 });
        let mut alloc = [[0u32; 32]; 2];
        for sb in 0..sblimit {
            if sb < bound {
                for a in alloc.iter_mut().take(nch) {
                    a[sb] = r.get(nbal[sb]);
                }
            } else {
                let a = r.get(nbal[sb]);
                alloc[0][sb] = a;
                alloc[1][sb] = a;
            }
        }
        let mut scfsi = [[0u32; 32]; 2];
        for sb in 0..sblimit {
            for ch in 0..nch {
                if alloc[ch][sb] != 0 {
                    scfsi[ch][sb] = r.get(2);
                }
            }
        }
        // Skalenfaktoren: Lage und abgedeckte Drittel
        let mut fields: Vec<(usize, [bool; 3])> = Vec::new();
        for sb in 0..sblimit {
            for ch in 0..nch {
                if alloc[ch][sb] == 0 {
                    continue;
                }
                let parts: &[[bool; 3]] = match scfsi[ch][sb] {
                    0 => &[[true, false, false], [false, true, false], [false, false, true]],
                    1 => &[[true, true, false], [false, false, true]],
                    2 => &[[true, true, true]],
                    _ => &[[true, false, false], [false, true, true]],
                };
                for p in parts {
                    fields.push((r.pos, *p));
                    r.pos += 6;
                }
            }
        }
        if !r.ok || r.pos > f.len() * 8 {
            return false;
        }
        for (pos, parts) in fields {
            let a = (0..3).filter(|k| parts[*k]).map(|k| g[k]).fold(1f32, f32::min);
            if a >= 0.9995 {
                continue;
            }
            let Some(idx) = peek(f, pos, 6) else { return false };
            let new = if a <= 0.0001 {
                62
            } else {
                let steps = (3.0 * (1.0 / f64::from(a)).log2()).round() as u32;
                (idx + steps).min(62)
            };
            put(f, pos, 6, new.max(idx));
        }
        true
    }
}

// ---------------------------------------------------------------------------
// Dolby Digital (AC-3), Bitzuteilung nach ATSC A/52
// ---------------------------------------------------------------------------

pub mod ac3 {
    use super::{put, scale_sym, Bits};

    const LOG_ADD: [u8; 260] = [
        0x40, 0x3f, 0x3e, 0x3d, 0x3c, 0x3b, 0x3a, 0x39, 0x38, 0x37, 0x36, 0x35, 0x34, 0x34, 0x33, 0x32, 0x31, 0x30,
        0x2f, 0x2f, 0x2e, 0x2d, 0x2c, 0x2c, 0x2b, 0x2a, 0x29, 0x29, 0x28, 0x27, 0x26, 0x26, 0x25, 0x24, 0x24, 0x23,
        0x23, 0x22, 0x21, 0x21, 0x20, 0x20, 0x1f, 0x1e, 0x1e, 0x1d, 0x1d, 0x1c, 0x1c, 0x1b, 0x1b, 0x1a, 0x1a, 0x19,
        0x19, 0x18, 0x18, 0x17, 0x17, 0x16, 0x16, 0x15, 0x15, 0x15, 0x14, 0x14, 0x13, 0x13, 0x13, 0x12, 0x12, 0x12,
        0x11, 0x11, 0x11, 0x10, 0x10, 0x10, 0x0f, 0x0f, 0x0f, 0x0e, 0x0e, 0x0e, 0x0d, 0x0d, 0x0d, 0x0d, 0x0c, 0x0c,
        0x0c, 0x0c, 0x0b, 0x0b, 0x0b, 0x0b, 0x0a, 0x0a, 0x0a, 0x0a, 0x0a, 0x09, 0x09, 0x09, 0x09, 0x09, 0x08, 0x08,
        0x08, 0x08, 0x08, 0x08, 0x07, 0x07, 0x07, 0x07, 0x07, 0x07, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06, 0x06,
        0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x05, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04,
        0x04, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x03, 0x02, 0x02, 0x02,
        0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x02, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01,
        0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];

    const HTH: [[u16; 3]; 50] = [
        [0x04d0, 0x04f0, 0x0580],
        [0x04d0, 0x04f0, 0x0580],
        [0x0440, 0x0460, 0x04b0],
        [0x0400, 0x0410, 0x0450],
        [0x03e0, 0x03e0, 0x0420],
        [0x03c0, 0x03d0, 0x03f0],
        [0x03b0, 0x03c0, 0x03e0],
        [0x03b0, 0x03b0, 0x03d0],
        [0x03a0, 0x03b0, 0x03c0],
        [0x03a0, 0x03a0, 0x03b0],
        [0x03a0, 0x03a0, 0x03b0],
        [0x03a0, 0x03a0, 0x03b0],
        [0x03a0, 0x03a0, 0x03a0],
        [0x0390, 0x03a0, 0x03a0],
        [0x0390, 0x0390, 0x03a0],
        [0x0390, 0x0390, 0x03a0],
        [0x0380, 0x0390, 0x03a0],
        [0x0380, 0x0380, 0x03a0],
        [0x0370, 0x0380, 0x03a0],
        [0x0370, 0x0380, 0x03a0],
        [0x0360, 0x0370, 0x0390],
        [0x0360, 0x0370, 0x0390],
        [0x0350, 0x0360, 0x0390],
        [0x0350, 0x0360, 0x0390],
        [0x0340, 0x0350, 0x0380],
        [0x0340, 0x0350, 0x0380],
        [0x0330, 0x0340, 0x0380],
        [0x0320, 0x0340, 0x0370],
        [0x0310, 0x0320, 0x0360],
        [0x0300, 0x0310, 0x0350],
        [0x02f0, 0x0300, 0x0340],
        [0x02f0, 0x02f0, 0x0330],
        [0x02f0, 0x02f0, 0x0320],
        [0x02f0, 0x02f0, 0x0310],
        [0x0300, 0x02f0, 0x0300],
        [0x0310, 0x0300, 0x02f0],
        [0x0340, 0x0320, 0x02f0],
        [0x0390, 0x0350, 0x02f0],
        [0x03e0, 0x0390, 0x0300],
        [0x0420, 0x03e0, 0x0310],
        [0x0460, 0x0420, 0x0330],
        [0x0490, 0x0450, 0x0350],
        [0x04a0, 0x04a0, 0x03c0],
        [0x0460, 0x0490, 0x0410],
        [0x0440, 0x0460, 0x0470],
        [0x0440, 0x0440, 0x04a0],
        [0x0520, 0x0480, 0x0460],
        [0x0800, 0x0630, 0x0440],
        [0x0840, 0x0840, 0x0450],
        [0x0840, 0x0840, 0x04e0],
    ];

    const BAP_TAB: [u8; 64] = [
        0, 1, 1, 1, 1, 1, 2, 2, 3, 3, 3, 4, 4, 5, 5, 6, 6, 6, 6, 7, 7, 7, 7, 8, 8, 8, 8, 9, 9, 9, 9, 10, 10, 10, 10, 11,
        11, 11, 11, 12, 12, 12, 12, 13, 13, 13, 13, 14, 14, 14, 14, 14, 14, 14, 14, 15, 15, 15, 15, 15, 15, 15, 15, 15,
    ];
    const SLOW_DECAY: [i32; 4] = [0x0f, 0x11, 0x13, 0x15];
    const FAST_DECAY: [i32; 4] = [0x3f, 0x53, 0x67, 0x7b];
    const SLOW_GAIN: [i32; 4] = [0x540, 0x4d8, 0x478, 0x410];
    const DB_PER_BIT: [i32; 4] = [0x000, 0x700, 0x900, 0xb00];
    const FLOOR: [i32; 8] = [0x2f0, 0x2b0, 0x270, 0x230, 0x1f0, 0x170, 0x0f0, -2048];
    const FAST_GAIN: [i32; 8] = [0x080, 0x100, 0x180, 0x200, 0x280, 0x300, 0x380, 0x400];
    const BAND_START: [usize; 51] = [
        0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 31, 34,
        37, 40, 43, 46, 49, 55, 61, 67, 73, 79, 85, 97, 109, 121, 133, 157, 181, 205, 229, 253,
    ];
    /// Bitbreite der Mantissen je Zuteilung (1, 2 und 4 sind gruppiert)
    const BAP_BITS: [usize; 16] = [0, 0, 0, 3, 0, 4, 5, 6, 7, 8, 9, 10, 11, 12, 14, 16];

    fn bin_to_band(bin: usize) -> usize {
        BAND_START.partition_point(|s| *s <= bin) - 1
    }

    #[derive(Clone, Copy, Default)]
    struct Params {
        sr_code: usize,
        slow_decay: i32,
        fast_decay: i32,
        slow_gain: i32,
        db_per_bit: i32,
        floor: i32,
        cpl_fast_leak: i32,
        cpl_slow_leak: i32,
    }

    #[derive(Clone, Default)]
    struct Dba {
        mode: u32,
        segs: Vec<(usize, usize, u32)>,
    }

    fn lowcomp1(a: i32, b0: i32, b1: i32, c: i32) -> i32 {
        if b0 + 256 == b1 {
            c
        } else if b0 > b1 {
            (a - 64).max(0)
        } else {
            a
        }
    }

    fn lowcomp(a: i32, b0: i32, b1: i32, bin: usize) -> i32 {
        if bin < 7 {
            lowcomp1(a, b0, b1, 384)
        } else if bin < 20 {
            lowcomp1(a, b0, b1, 320)
        } else {
            (a - 128).max(0)
        }
    }

    /// Bitzuteilung eines Kanals (A/52 Abschnitt 7.2).
    #[allow(clippy::too_many_arguments)]
    fn bit_alloc(
        exp: &[i32; 256],
        start: usize,
        end: usize,
        p: &Params,
        fast_gain: i32,
        snr_offset: i32,
        is_lfe: bool,
        dba: &Dba,
        bap: &mut [u8; 256],
    ) -> Option<()> {
        if end <= start || end > 253 {
            return None;
        }
        if snr_offset == -960 {
            bap.fill(0);
            return Some(());
        }
        let mut psd = [0i32; 256];
        let mut band_psd = [0i32; 50];
        for bin in start..end {
            psd[bin] = 3072 - (exp[bin] << 7);
        }
        let mut bin = start;
        while bin < end.min(28) {
            band_psd[bin] = psd[bin];
            bin += 1;
        }
        if bin < end {
            let mut band = bin_to_band(bin);
            loop {
                let mut v = psd[bin];
                bin += 1;
                let band_end = BAND_START[band + 1].min(end);
                while bin < band_end {
                    let max = v.max(psd[bin]);
                    let adr = (max - ((v + psd[bin] + 1) >> 1)).min(255);
                    v = max + i32::from(LOG_ADD[adr as usize]);
                    bin += 1;
                }
                band_psd[band] = v;
                band += 1;
                if end <= BAND_START[band] {
                    break;
                }
            }
        }

        // Anregung
        let mut excite = [0i32; 50];
        let band_start = bin_to_band(start);
        let band_end = bin_to_band(end - 1) + 1;
        let (mut fastleak, mut slowleak);
        let begin;
        if band_start == 0 {
            let mut lc = lowcomp1(0, band_psd[0], band_psd[1], 384);
            excite[0] = band_psd[0] - fast_gain - lc;
            lc = lowcomp1(lc, band_psd[1], band_psd[2], 384);
            excite[1] = band_psd[1] - fast_gain - lc;
            let mut b = 7;
            fastleak = 0;
            slowleak = 0;
            for band in 2..7 {
                if !(is_lfe && band == 6) {
                    lc = lowcomp1(lc, band_psd[band], band_psd[band + 1], 384);
                }
                fastleak = band_psd[band] - fast_gain;
                slowleak = band_psd[band] - p.slow_gain;
                excite[band] = fastleak - lc;
                if !(is_lfe && band == 6) && band_psd[band] <= band_psd[band + 1] {
                    b = band + 1;
                    break;
                }
            }
            let end1 = band_end.min(22);
            for band in b..end1 {
                if !(is_lfe && band == 6) {
                    lc = lowcomp(lc, band_psd[band], band_psd[band + 1], band);
                }
                fastleak = (fastleak - p.fast_decay).max(band_psd[band] - fast_gain);
                slowleak = (slowleak - p.slow_decay).max(band_psd[band] - p.slow_gain);
                excite[band] = (fastleak - lc).max(slowleak);
            }
            begin = 22;
        } else {
            begin = band_start;
            fastleak = (p.cpl_fast_leak << 8) + 768;
            slowleak = (p.cpl_slow_leak << 8) + 768;
        }
        for band in begin..band_end {
            fastleak = (fastleak - p.fast_decay).max(band_psd[band] - fast_gain);
            slowleak = (slowleak - p.slow_decay).max(band_psd[band] - p.slow_gain);
            excite[band] = fastleak.max(slowleak);
        }

        // Maskierungskurve
        let mut mask = [0i32; 50];
        for band in band_start..band_end {
            let tmp = p.db_per_bit - band_psd[band];
            if tmp > 0 {
                excite[band] += tmp >> 2;
            }
            mask[band] = i32::from(HTH[band][p.sr_code]).max(excite[band]);
        }
        if dba.mode == 0 || dba.mode == 1 {
            let mut band = band_start;
            for &(off, len, val) in &dba.segs {
                band += off;
                if band >= 50 || len > 50 - band {
                    return None;
                }
                let delta = if val >= 4 { (val as i32 - 3) * 128 } else { (val as i32 - 4) * 128 };
                for _ in 0..len {
                    mask[band] += delta;
                    band += 1;
                }
            }
        }

        // Zuteilung je Koeffizient
        let mut bin = start;
        let mut band = bin_to_band(start);
        loop {
            let m = (((mask[band] - snr_offset - p.floor).max(0)) & 0x1FE0) + p.floor;
            band += 1;
            let band_end = BAND_START[band].min(end);
            while bin < band_end {
                let address = ((psd[bin] - m) >> 5).clamp(0, 63);
                bap[bin] = BAP_TAB[address as usize];
                bin += 1;
            }
            if end <= band_end {
                break;
            }
        }
        Some(())
    }

    fn decode_exps(r: &mut Bits, strategy: u32, ngrps: usize, absexp: i32, out: &mut [i32]) -> Option<()> {
        let gs = match strategy {
            1 => 1,
            2 => 2,
            _ => 4,
        };
        let mut prev = absexp;
        let mut j = 0;
        for _ in 0..ngrps {
            let g = r.get(7) as i32;
            if g >= 125 {
                return None;
            }
            for d in [g / 25, (g % 25) / 5, g % 5] {
                prev += d - 2;
                if !(0..=24).contains(&prev) {
                    return None;
                }
                for _ in 0..gs {
                    *out.get_mut(j)? = prev;
                    j += 1;
                }
            }
        }
        Some(())
    }

    /// Ein Mantissenfeld im Rahmen.
    enum Field {
        /// Asymmetrisch, Zweierkomplement
        Asym { pos: usize, bits: usize },
        /// Symmetrisch mit `levels` Stufen
        Sym { pos: usize, bits: usize, levels: u32 },
        /// Gruppe aus `count` symmetrischen Werten mit `levels` Stufen
        Group { pos: usize, bits: usize, levels: u32, count: u32 },
    }

    struct Block {
        fields: Vec<Field>,
        dither_pos: Vec<usize>,
    }

    const CPL: usize = 5;
    const LFE: usize = 6;

    /// Liest einen Rahmen bis zu den Mantissen. `None`, wenn etwas nicht passt.
    fn parse(f: &[u8]) -> Option<Vec<Block>> {
        if f.len() < 8 || f[0] != 0x0B || f[1] != 0x77 {
            return None;
        }
        let fscod = usize::from(f[4] >> 6);
        if fscod == 3 {
            return None;
        }
        let mut r = Bits::new(f, 40);
        let bsid = r.get(5);
        if bsid > 8 {
            return None;
        }
        r.get(3); // bsmod
        let acmod = r.get(3) as usize;
        if acmod == 2 {
            r.get(2);
        } else {
            if acmod & 1 != 0 && acmod != 1 {
                r.get(2);
            }
            if acmod & 4 != 0 {
                r.get(2);
            }
        }
        let lfeon = r.bit();
        let nfch = [2, 1, 2, 3, 3, 4, 4, 5][acmod];
        for _ in 0..if acmod == 0 { 2 } else { 1 } {
            r.get(5);
            if r.bit() {
                r.get(8);
            }
            if r.bit() {
                r.get(8);
            }
            if r.bit() {
                r.get(7);
            }
        }
        r.get(2);
        if r.bit() {
            r.get(14);
        }
        if r.bit() {
            r.get(14);
        }
        if r.bit() {
            let n = r.get(6) as usize + 1;
            r.pos += 8 * n;
        }
        if !r.ok {
            return None;
        }

        // Zustand über die Blöcke hinweg; Kanäle 0..5 Hauptkanäle, 5 = Kopplung, 6 = LFE
        let mut exps = [[0i32; 256]; 7];
        let mut start = [0usize; 7];
        let mut endm = [0usize; 7];
        endm[LFE] = 7;
        let mut p = Params { sr_code: fscod, ..Default::default() };
        let mut snr = [0i32; 7];
        let mut fgain = [0i32; 7];
        let mut dba: [Dba; 7] = Default::default();
        let mut cplinu = false;
        let mut chincpl = [false; 5];
        let mut ncplbnd = 0usize;
        let mut phsflginu = false;
        let mut blocks = Vec::with_capacity(6);

        for blk in 0..6 {
            let mut block = Block { fields: Vec::new(), dither_pos: Vec::new() };
            r.pos += nfch; // blksw
            for _ in 0..nfch {
                block.dither_pos.push(r.pos);
                r.pos += 1;
            }
            for _ in 0..if acmod == 0 { 2 } else { 1 } {
                if r.bit() {
                    r.get(8);
                }
            }
            // Kopplung
            if r.bit() {
                cplinu = r.bit();
                if cplinu {
                    for c in chincpl.iter_mut().take(nfch) {
                        *c = r.bit();
                    }
                    if acmod == 2 {
                        phsflginu = r.bit();
                    }
                    let begf = r.get(4) as usize;
                    let endf = r.get(4) as usize + 3;
                    if begf >= endf {
                        return None;
                    }
                    start[CPL] = begf * 12 + 37;
                    endm[CPL] = endf * 12 + 37;
                    let nsub = endf - begf;
                    let mut nb = nsub;
                    for _ in 1..nsub {
                        if r.bit() {
                            nb -= 1;
                        }
                    }
                    ncplbnd = nb;
                } else {
                    chincpl = [false; 5];
                }
            } else if blk == 0 {
                return None;
            }
            if cplinu {
                let mut any = false;
                for ch in 0..nfch {
                    if chincpl[ch] && r.bit() {
                        any = true;
                        r.get(2);
                        r.pos += 8 * ncplbnd;
                    }
                }
                if acmod == 2 && any && phsflginu {
                    r.pos += ncplbnd;
                }
            }
            // Rematrizierung
            if acmod == 2 && r.bit() {
                let mut n = 4;
                if cplinu && start[CPL] <= 61 {
                    n -= 1 + usize::from(start[CPL] == 37);
                }
                r.pos += n;
            }
            // Exponenten-Strategien
            let mut strat = [0u32; 7];
            if cplinu {
                strat[CPL] = r.get(2);
            }
            for s in strat.iter_mut().take(nfch) {
                *s = r.get(2);
            }
            if lfeon {
                strat[LFE] = r.get(1);
            }
            for ch in 0..nfch {
                if strat[ch] != 0 {
                    if cplinu && chincpl[ch] {
                        endm[ch] = start[CPL];
                    } else {
                        let bw = r.get(6) as usize;
                        if bw > 60 {
                            return None;
                        }
                        endm[ch] = bw * 3 + 73;
                    }
                }
            }
            if blk == 0 && (strat[..nfch].contains(&0) || (cplinu && strat[CPL] == 0) || (lfeon && strat[LFE] == 0)) {
                return None;
            }
            // Exponenten
            if cplinu && strat[CPL] != 0 {
                let abs = (r.get(4) as i32) << 1;
                let ngr = (endm[CPL] - start[CPL]) / (3 << (strat[CPL] - 1));
                let s = start[CPL];
                decode_exps(&mut r, strat[CPL], ngr, abs, &mut exps[CPL][s..])?;
            }
            for ch in 0..nfch {
                if strat[ch] != 0 {
                    let gs = 3usize << (strat[ch] - 1);
                    let ngr = (endm[ch] + gs - 4) / gs;
                    let abs = r.get(4) as i32;
                    exps[ch][0] = abs;
                    decode_exps(&mut r, strat[ch], ngr, abs, &mut exps[ch][1..])?;
                    r.get(2); // gainrng
                }
            }
            if lfeon && strat[LFE] != 0 {
                let abs = r.get(4) as i32;
                exps[LFE][0] = abs;
                decode_exps(&mut r, 1, 2, abs, &mut exps[LFE][1..])?;
            }
            // Parameter der Bitzuteilung
            if r.bit() {
                p.slow_decay = SLOW_DECAY[r.get(2) as usize];
                p.fast_decay = FAST_DECAY[r.get(2) as usize];
                p.slow_gain = SLOW_GAIN[r.get(2) as usize];
                p.db_per_bit = DB_PER_BIT[r.get(2) as usize];
                p.floor = FLOOR[r.get(3) as usize];
            } else if blk == 0 {
                return None;
            }
            let order: Vec<usize> = cplinu.then_some(CPL).into_iter().chain(0..nfch).chain(lfeon.then_some(LFE)).collect();
            if r.bit() {
                let csnr = r.get(6) as i32;
                for &ch in &order {
                    snr[ch] = (((csnr - 15) << 4) + r.get(4) as i32) << 2;
                    fgain[ch] = FAST_GAIN[r.get(3) as usize];
                }
            } else if blk == 0 {
                return None;
            }
            if cplinu && r.bit() {
                p.cpl_fast_leak = r.get(3) as i32;
                p.cpl_slow_leak = r.get(3) as i32;
            }
            let dba_chs: Vec<usize> = cplinu.then_some(CPL).into_iter().chain(0..nfch).collect();
            if r.bit() {
                for &ch in &dba_chs {
                    dba[ch].mode = r.get(2);
                }
                for &ch in &dba_chs {
                    if dba[ch].mode == 1 {
                        let n = r.get(3) as usize + 1;
                        dba[ch].segs = (0..n).map(|_| (r.get(5) as usize, r.get(4) as usize, r.get(3))).collect();
                    }
                }
            } else if blk == 0 {
                for d in dba.iter_mut() {
                    d.mode = 2;
                    d.segs.clear();
                }
            }
            if r.bit() {
                let n = r.get(9) as usize;
                r.pos += 8 * n;
            }
            if !r.ok {
                return None;
            }

            // Bitzuteilung und Mantissen
            let mut bap = [[0u8; 256]; 7];
            for &ch in &order {
                let (s, e) = if ch == CPL || ch == LFE { (start[ch], endm[ch]) } else { (0, endm[ch]) };
                bit_alloc(&exps[ch], s, e, &p, fgain[ch], snr[ch], ch == LFE, &dba[ch], &mut bap[ch])?;
            }
            let mut left = [0u32; 3]; // offene Werte in Gruppen für Zuteilung 1, 2, 4
            let mut read = |r: &mut Bits, b: u8, fields: &mut Vec<Field>| {
                match b {
                    0 => {}
                    1 | 2 | 4 => {
                        let gi = usize::from(b >> 1);
                        if left[gi] == 0 {
                            let (bits, levels, count) = match b {
                                1 => (5, 3, 3),
                                2 => (7, 5, 3),
                                _ => (7, 11, 2),
                            };
                            fields.push(Field::Group { pos: r.pos, bits, levels, count });
                            r.pos += bits;
                            left[gi] = count;
                        }
                        left[gi] -= 1;
                    }
                    3 | 5 => {
                        let levels = if b == 3 { 7 } else { 15 };
                        fields.push(Field::Sym { pos: r.pos, bits: BAP_BITS[usize::from(b)], levels });
                        r.pos += BAP_BITS[usize::from(b)];
                    }
                    _ => {
                        let bits = BAP_BITS[usize::from(b & 15)];
                        fields.push(Field::Asym { pos: r.pos, bits });
                        r.pos += bits;
                    }
                }
            };
            let mut got_cpl = false;
            for ch in 0..nfch {
                for bin in 0..endm[ch] {
                    read(&mut r, bap[ch][bin], &mut block.fields);
                }
                if cplinu && chincpl[ch] && !got_cpl {
                    for bin in start[CPL]..endm[CPL] {
                        read(&mut r, bap[CPL][bin], &mut block.fields);
                    }
                    got_cpl = true;
                }
            }
            if lfeon {
                for bin in 0..7 {
                    read(&mut r, bap[LFE][bin], &mut block.fields);
                }
            }
            if !r.ok {
                return None;
            }
            blocks.push(block);
        }
        // Platz für die zweite Prüfsumme muss bleiben
        if r.pos + 16 > f.len() * 8 {
            return None;
        }
        Some(blocks)
    }

    /// Skaliert einen AC-3-Rahmen; `g` gilt je Block (256 Abtastwerte).
    pub fn scale_frame(f: &mut [u8], g: &[f32; 6]) -> bool {
        let Some(blocks) = parse(f) else { return false };
        for (b, block) in blocks.iter().enumerate() {
            let a = g[b].clamp(0.0, 1.0);
            if a >= 0.9995 {
                continue;
            }
            // Ohne Rauschfüllung, sonst bliebe bei leisen Stellen Rauschen stehen
            if a < 0.5 {
                for &pos in &block.dither_pos {
                    put(f, pos, 1, 0);
                }
            }
            for field in &block.fields {
                match *field {
                    Field::Asym { pos, bits } => {
                        let Some(raw) = super::peek(f, pos, bits) else { return false };
                        let half = 1i32 << (bits - 1);
                        let v = if raw as i32 >= half { raw as i32 - 2 * half } else { raw as i32 };
                        let nv = (v as f32 * a).round().clamp(-(half as f32), (half - 1) as f32) as i32;
                        put(f, pos, bits, (nv as u32) & ((1u32 << bits) - 1));
                    }
                    Field::Sym { pos, bits, levels } => {
                        let Some(k) = super::peek(f, pos, bits) else { return false };
                        put(f, pos, bits, scale_sym(k, levels, a));
                    }
                    Field::Group { pos, bits, levels, count } => {
                        let Some(code) = super::peek(f, pos, bits) else { return false };
                        let max = levels.pow(count);
                        if code >= max {
                            continue;
                        }
                        let mut digits = Vec::with_capacity(count as usize);
                        let mut c = code;
                        for _ in 0..count {
                            digits.push(c % levels);
                            c /= levels;
                        }
                        digits.reverse();
                        let new = digits.iter().fold(0u32, |acc, d| acc * levels + scale_sym(*d, levels, a));
                        put(f, pos, bits, new);
                    }
                }
            }
        }
        fix_crc(f);
        true
    }

    fn crc16(d: &[u8], mut crc: u16) -> u16 {
        for &b in d {
            crc ^= u16::from(b) << 8;
            for _ in 0..8 {
                crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x8005 } else { crc << 1 };
            }
        }
        crc
    }

    /// Setzt beide Prüfsummen neu: crc1 so, dass die ersten 5/8 des Rahmens (ohne
    /// Sync-Wort) die Prüfsumme 0 ergeben, crc2 für den Rest.
    pub fn fix_crc(f: &mut [u8]) {
        let n = f.len();
        let f58 = ((n >> 2) + (n >> 4)) << 1;
        if f58 < 4 || f58 + 2 > n {
            return;
        }
        f[2] = 0;
        f[3] = 0;
        let r0 = crc16(&f[2..f58], 0);
        // Wirkung jedes Bits von crc1 auf die Prüfsumme (linear)
        let zeros = vec![0u8; f58 - 4];
        let mut cols = [0u16; 16];
        for (i, c) in cols.iter_mut().enumerate() {
            let v = 1u16 << i;
            *c = crc16(&zeros, crc16(&v.to_be_bytes(), 0));
        }
        // cols * x = r0 über GF(2) lösen
        let mut rows: Vec<(u16, u16)> = Vec::new(); // (Spaltenkombination, Wert)
        let mut basis: Vec<(u16, u16)> = Vec::new();
        for (i, c) in cols.iter().enumerate() {
            rows.push((*c, 1u16 << i));
        }
        for (mut v, mut comb) in rows {
            for &(bv, bc) in &basis {
                if v ^ bv < v {
                    v ^= bv;
                    comb ^= bc;
                }
            }
            if v != 0 {
                basis.push((v, comb));
                basis.sort_by(|a, b| b.0.cmp(&a.0));
            }
        }
        let mut target = r0;
        let mut x = 0u16;
        for &(bv, bc) in &basis {
            if target ^ bv < target {
                target ^= bv;
                x ^= bc;
            }
        }
        f[2..4].copy_from_slice(&x.to_be_bytes());
        let crc2 = crc16(&f[f58..n - 2], 0);
        f[n - 2..].copy_from_slice(&crc2.to_be_bytes());
    }

    /// Prüft beide Prüfsummen (für Tests).
    pub fn crc_ok(f: &[u8]) -> bool {
        let n = f.len();
        let f58 = ((n >> 2) + (n >> 4)) << 1;
        crc16(&f[2..f58], 0) == 0 && crc16(&f[2..], 0) == 0
    }
}
