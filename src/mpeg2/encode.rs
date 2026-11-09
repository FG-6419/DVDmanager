//! Kleiner MPEG-2-Encoder für die eingebaute Beispiel-DVD: I-Bilder und
//! P-Bilder mit einem einzigen Bewegungsvektor, mehr braucht das Testbild nicht.

use std::sync::OnceLock;

use super::bits::BitWriter;
use super::requant::write_block;
use super::tables::*;

#[derive(Clone, Copy)]
pub struct EncParams {
    pub width: u32,
    pub height: u32,
    /// 2 = 4:3, 3 = 16:9
    pub aspect: u8,
    /// 3 = 25 Bilder/s, 4 = 29,97 Bilder/s
    pub fps_code: u8,
    pub bitrate_400: u32,
}

pub fn write_seq_header(w: &mut BitWriter, p: &EncParams) {
    w.start_code(0xB3);
    w.put(p.width, 12);
    w.put(p.height, 12);
    w.put(u32::from(p.aspect), 4);
    w.put(u32::from(p.fps_code), 4);
    w.put(p.bitrate_400 & 0x3FFFF, 18);
    w.put(1, 1);
    w.put(112, 10);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.align();
    w.start_code(0xB5);
    w.put(1, 4);
    w.put(0x48, 8);
    w.put(0, 1);
    w.put(1, 2);
    w.put(0, 2);
    w.put(0, 2);
    w.put(0, 12);
    w.put(1, 1);
    w.put(0, 8);
    w.put(0, 1);
    w.put(0, 2);
    w.put(0, 5);
    w.align();
}

pub fn write_gop_header(w: &mut BitWriter, frame: u64, fps: u32, closed: bool) {
    let secs = frame / u64::from(fps);
    w.start_code(0xB8);
    w.put(0, 1);
    w.put((secs / 3600) as u32, 5);
    w.put(((secs / 60) % 60) as u32, 6);
    w.put(1, 1);
    w.put((secs % 60) as u32, 6);
    w.put((frame % u64::from(fps)) as u32, 6);
    w.put(u32::from(closed), 1);
    w.put(0, 1);
    w.align();
}

pub fn write_picture(w: &mut BitWriter, temporal: u32, ct: u8) {
    w.start_code(0x00);
    w.put(temporal, 10);
    w.put(u32::from(ct), 3);
    w.put(0xFFFF, 16);
    if ct == 2 {
        w.put(0, 1);
        w.put(7, 3);
    }
    w.put(0, 1);
    w.align();
    w.start_code(0xB5);
    w.put(8, 4);
    let fc = if ct == 2 { 1 } else { 15 };
    w.put(fc, 4);
    w.put(fc, 4);
    w.put(15, 4);
    w.put(15, 4);
    w.put(0, 2); // intra_dc_precision 8 Bit
    w.put(3, 2); // Vollbild
    w.put(1, 1); // top_field_first
    w.put(1, 1); // frame_pred_frame_dct
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.put(1, 1);
    w.put(1, 1); // progressive_frame
    w.put(0, 1);
    w.align();
}

fn cos_table() -> &'static [[f32; 8]; 8] {
    static T: OnceLock<[[f32; 8]; 8]> = OnceLock::new();
    T.get_or_init(|| {
        let mut t = [[0f32; 8]; 8];
        for (u, row) in t.iter_mut().enumerate() {
            for (x, v) in row.iter_mut().enumerate() {
                let c = if u == 0 { (0.5f64).sqrt() } else { 1.0 };
                *v = (0.5
                    * c
                    * (((2 * x + 1) as f64) * (u as f64) * std::f64::consts::PI / 16.0).cos())
                    as f32;
            }
        }
        t
    })
}

pub fn fdct(src: &[f32; 64]) -> [f32; 64] {
    let t = cos_table();
    let mut tmp = [0f32; 64];
    for y in 0..8 {
        for u in 0..8 {
            let mut s = 0.0;
            for x in 0..8 {
                s += src[y * 8 + x] * t[u][x];
            }
            tmp[y * 8 + u] = s;
        }
    }
    let mut out = [0f32; 64];
    for u in 0..8 {
        for v in 0..8 {
            let mut s = 0.0;
            for y in 0..8 {
                s += tmp[y * 8 + u] * t[v][y];
            }
            out[v * 8 + u] = s;
        }
    }
    out
}

pub fn put_dc(w: &mut BitWriter, diff: i32, luma: bool) {
    let t = tables();
    let abs = diff.unsigned_abs();
    let size = 32 - abs.leading_zeros();
    let (c, n) = if luma { t.dc_luma_enc[size as usize] } else { t.dc_chroma_enc[size as usize] };
    w.put(u32::from(c), u32::from(n));
    if size > 0 {
        let bits = if diff > 0 { diff as u32 } else { (diff + (1 << size) - 1) as u32 };
        w.put(bits, size);
    }
}

fn encode_block(w: &mut BitWriter, block: &[f32; 64], q_code: u8, dc_pred: &mut i32, luma: bool) {
    let f = fdct(block);
    let qs = qscale(q_code, false) as i32;
    let dc = ((f[0] / 8.0).round() as i32).clamp(1, 254);
    put_dc(w, dc - *dc_pred, luma);
    *dc_pred = dc;
    let mut tokens: Vec<(u8, i16)> = Vec::with_capacity(16);
    for idx in 1..64usize {
        let pos = ZIGZAG[idx] as usize;
        let m = i32::from(DEFAULT_INTRA[pos]);
        let level = (f[pos] * 16.0 / (m * qs) as f32).round() as i32;
        if level != 0 {
            tokens.push((idx as u8, level.clamp(-2047, 2047) as i16));
        }
    }
    write_block(w, &tokens, true, 0);
}

/// Schreibt ein vollständiges I-Bild. Die Ebenen liegen in Makroblock-Größe vor.
pub fn write_intra_picture(
    w: &mut BitWriter,
    temporal: u32,
    (mbw, mbh): (usize, usize),
    y: &[u8],
    u: &[u8],
    v: &[u8],
    q_code: u8,
) {
    write_picture(w, temporal, 1);
    let stride = mbw * 16;
    let cs = mbw * 8;
    for row in 0..mbh {
        w.start_code((row + 1) as u8);
        w.put(u32::from(q_code), 5);
        w.put(0, 1);
        let mut dc = [128i32; 3];
        for x in 0..mbw {
            w.put(1, 1); // Adress-Inkrement
            w.put(1, 1); // Makroblock-Typ intra
            for b in 0..4 {
                let bx = x * 16 + (b & 1) * 8;
                let by = row * 16 + (b >> 1) * 8;
                let mut block = [0f32; 64];
                for j in 0..8 {
                    for i in 0..8 {
                        block[j * 8 + i] = f32::from(y[(by + j) * stride + bx + i]);
                    }
                }
                encode_block(w, &block, q_code, &mut dc[0], true);
            }
            for (plane, cc) in [(u, 1usize), (v, 2usize)] {
                let mut block = [0f32; 64];
                for j in 0..8 {
                    for i in 0..8 {
                        block[j * 8 + i] = f32::from(plane[(row * 8 + j) * cs + x * 8 + i]);
                    }
                }
                encode_block(w, &block, q_code, &mut dc[cc], false);
            }
        }
    }
    w.align();
}

fn put_motion_delta(w: &mut BitWriter, d: i32) {
    let t = tables();
    let (c, n) = t.motion_enc[(d + 16) as usize];
    w.put(u32::from(c), u32::from(n));
}

/// Schreibt ein P-Bild, das das vorige Bild als Ganzes verschiebt (Halbpixel-Einheiten).
pub fn write_pan_picture(w: &mut BitWriter, temporal: u32, (mbw, mbh): (usize, usize), mv: (i32, i32)) {
    write_picture(w, temporal, 2);
    let t = tables();
    let (ty, ty_n) = t.mb_enc[1][MB_FWD as usize];
    for row in 0..mbh {
        w.start_code((row + 1) as u8);
        w.put(4, 5);
        w.put(0, 1);
        let mut first = true;
        for _ in 0..mbw {
            w.put(1, 1);
            w.put(u32::from(ty), u32::from(ty_n));
            if first {
                put_motion_delta(w, mv.0);
                put_motion_delta(w, mv.1);
                first = false;
            } else {
                put_motion_delta(w, 0);
                put_motion_delta(w, 0);
            }
        }
    }
    w.align();
}
