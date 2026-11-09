//! Kodiert wenige dekodierte Bilder neu, für bildgenaue Schnitte: ein I-Bild, danach
//! P-Bilder, die nur den Unterschied zum Vorbild tragen. Die Größe richtet sich nach
//! einem Budget, damit der neue Abschnitt in den Platz des alten passt.

use super::bits::BitWriter;
use super::decode::{idct, DecodedFrame};
use super::encode::{fdct, put_dc, write_gop_header, write_picture};
use super::headers::SeqInfo;
use super::requant::write_block;
use super::tables::*;

/// Neu kodierter Abschnitt: Elementarstrom und Lage jedes Bildes darin.
pub struct Reencoded {
    pub es: Vec<u8>,
    /// (Beginn des Bildkopfs, Ende der Bilddaten) je Bild, in Anzeigereihenfolge.
    pub pics: Vec<(usize, usize)>,
    pub q_code: u8,
}

/// Sequenzkopf passend zum Ausgangsmaterial, samt eigener Quantisierungsmatrizen.
pub fn write_seq_from(w: &mut BitWriter, seq: &SeqInfo) {
    w.start_code(0xB3);
    w.put(seq.width & 0xFFF, 12);
    w.put(seq.height & 0xFFF, 12);
    w.put(u32::from(seq.aspect), 4);
    w.put(u32::from(seq.frame_rate_code), 4);
    w.put(24_500, 18); // 9,8 MBit/s
    w.put(1, 1);
    w.put(112, 10);
    w.put(0, 1);
    if seq.intra_matrix != DEFAULT_INTRA {
        w.put(1, 1);
        for z in ZIGZAG {
            w.put(u32::from(seq.intra_matrix[z as usize]), 8);
        }
    } else {
        w.put(0, 1);
    }
    if seq.nonintra_matrix != [16; 64] {
        w.put(1, 1);
        for z in ZIGZAG {
            w.put(u32::from(seq.nonintra_matrix[z as usize]), 8);
        }
    } else {
        w.put(0, 1);
    }
    w.align();
    w.start_code(0xB5);
    w.put(1, 4);
    w.put(0x48, 8);
    w.put(u32::from(seq.progressive), 1);
    w.put(1, 2);
    w.put(seq.width >> 12, 2);
    w.put(seq.height >> 12, 2);
    w.put(0, 12);
    w.put(1, 1);
    w.put(0, 8);
    w.put(0, 1);
    w.put(0, 2);
    w.put(0, 5);
    w.align();
}

struct Planes {
    stride: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Planes {
    fn of(f: &DecodedFrame) -> Self {
        Self { stride: f.stride, y: f.y.clone(), u: f.u.clone(), v: f.v.clone() }
    }

    fn plane(&self, b: usize) -> (&[u8], usize) {
        match b {
            0..=3 => (&self.y, self.stride),
            4 => (&self.u, self.stride / 2),
            _ => (&self.v, self.stride / 2),
        }
    }

    fn plane_mut(&mut self, b: usize) -> (&mut [u8], usize) {
        let s = self.stride;
        match b {
            0..=3 => (&mut self.y, s),
            4 => (&mut self.u, s / 2),
            _ => (&mut self.v, s / 2),
        }
    }
}

/// Lage eines Blocks im Bild: Ebene, x, y.
fn block_origin(mx: usize, my: usize, b: usize) -> (usize, usize) {
    if b < 4 {
        (mx * 16 + (b & 1) * 8, my * 16 + (b >> 1) * 8)
    } else {
        (mx * 8, my * 8)
    }
}

fn read_block(src: &[u8], stride: usize, x: usize, y: usize) -> [i32; 64] {
    let mut out = [0i32; 64];
    for j in 0..8 {
        for i in 0..8 {
            out[j * 8 + i] = i32::from(*src.get((y + j) * stride + x + i).unwrap_or(&128));
        }
    }
    out
}

fn mismatch(c: &mut [i32; 64]) {
    let sum: i32 = c.iter().sum();
    if sum & 1 == 0 {
        c[63] ^= 1;
    }
}

/// Ein Bild kodieren; `reference` ist die Rekonstruktion des Vorbilds (bei P-Bildern).
fn encode_picture(
    w: &mut BitWriter,
    seq: &SeqInfo,
    cur: &Planes,
    reference: Option<&Planes>,
    recon: &mut Planes,
    temporal: u32,
    q_code: u8,
) {
    let t = tables();
    let intra_pic = reference.is_none();
    write_picture(w, temporal, if intra_pic { 1 } else { 2 });
    let mbw = seq.mb_width();
    let mbh = seq.mb_height();
    let qs = qscale(q_code, false) as i32;
    let (p_ty, p_ty_n) = t.mb_enc[1][(MB_FWD | MB_PATTERN) as usize];
    let (skip_ty, skip_ty_n) = t.mb_enc[1][MB_FWD as usize];

    for my in 0..mbh {
        w.start_code((my + 1) as u8);
        w.put(u32::from(q_code), 5);
        w.put(0, 1);
        let mut dc_pred = [128i32; 3];
        for mx in 0..mbw {
            w.put(1, 1); // Adress-Inkrement
            if intra_pic {
                w.put(1, 1); // Typ intra
                for b in 0..6 {
                    let (src, stride) = cur.plane(b);
                    let (x, y) = block_origin(mx, my, b);
                    let px = read_block(src, stride, x, y);
                    let mut f = [0f32; 64];
                    for k in 0..64 {
                        f[k] = px[k] as f32;
                    }
                    let c = fdct(&f);
                    let dc = ((c[0] / 8.0).round() as i32).clamp(1, 254);
                    let cc = if b < 4 { 0 } else { b - 3 };
                    put_dc(w, dc - dc_pred[cc], b < 4);
                    dc_pred[cc] = dc;
                    let mut tokens = Vec::with_capacity(16);
                    let mut coef = [0i32; 64];
                    coef[0] = dc * 8;
                    for idx in 1..64usize {
                        let pos = ZIGZAG[idx] as usize;
                        let m = i32::from(seq.intra_matrix[pos]);
                        let level = ((c[pos] * 16.0 / (m * qs) as f32).round() as i32).clamp(-2047, 2047);
                        if level != 0 {
                            tokens.push((idx as u8, level as i16));
                            coef[pos] = ((2 * level * m * qs) / 32).clamp(-2048, 2047);
                        }
                    }
                    write_block(w, &tokens, true, 0);
                    mismatch(&mut coef);
                    idct(&mut coef);
                    let (dst, ds) = recon.plane_mut(b);
                    for j in 0..8 {
                        for i in 0..8 {
                            if let Some(d) = dst.get_mut((y + j) * ds + x + i) {
                                *d = coef[j * 8 + i].clamp(0, 255) as u8;
                            }
                        }
                    }
                }
            } else {
                let reference = reference.unwrap();
                let mut blocks: [Vec<(u8, i16)>; 6] = Default::default();
                let mut coefs = [[0i32; 64]; 6];
                let mut cbp = 0u8;
                for b in 0..6 {
                    let (src, stride) = cur.plane(b);
                    let (rsrc, _) = reference.plane(b);
                    let (x, y) = block_origin(mx, my, b);
                    let px = read_block(src, stride, x, y);
                    let rp = read_block(rsrc, stride, x, y);
                    let mut f = [0f32; 64];
                    for k in 0..64 {
                        f[k] = (px[k] - rp[k]) as f32;
                    }
                    let c = fdct(&f);
                    for idx in 0..64usize {
                        let pos = ZIGZAG[idx] as usize;
                        let m = i32::from(seq.nonintra_matrix[pos]);
                        let scaled = c[pos].abs() * 16.0 / (m * qs) as f32;
                        let level = ((scaled - 0.2).floor() as i32).clamp(0, 2047);
                        if level > 0 {
                            let signed = if c[pos] < 0.0 { -level } else { level };
                            blocks[b].push((idx as u8, signed as i16));
                            coefs[b][pos] = (((2 * signed + signed.signum()) * m * qs) / 32).clamp(-2048, 2047);
                        }
                    }
                    if !blocks[b].is_empty() {
                        cbp |= 1 << (5 - b);
                    }
                }
                if cbp == 0 {
                    w.put(u32::from(skip_ty), u32::from(skip_ty_n));
                    w.put(1, 1);
                    w.put(1, 1);
                } else {
                    w.put(u32::from(p_ty), u32::from(p_ty_n));
                    w.put(1, 1);
                    w.put(1, 1);
                    let (c, n) = t.cbp_enc[cbp as usize];
                    w.put(u32::from(c), u32::from(n));
                    for b in 0..6 {
                        if cbp & (1 << (5 - b)) != 0 {
                            write_block(w, &blocks[b], false, 0);
                        }
                    }
                }
                for b in 0..6 {
                    let (rsrc, stride) = reference.plane(b);
                    let (x, y) = block_origin(mx, my, b);
                    let rp = read_block(rsrc, stride, x, y);
                    let mut res = coefs[b];
                    let coded = cbp & (1 << (5 - b)) != 0;
                    if coded {
                        mismatch(&mut res);
                        idct(&mut res);
                    }
                    let (dst, ds) = recon.plane_mut(b);
                    for j in 0..8 {
                        for i in 0..8 {
                            let v = rp[j * 8 + i] + if coded { res[j * 8 + i] } else { 0 };
                            if let Some(d) = dst.get_mut((y + j) * ds + x + i) {
                                *d = v.clamp(0, 255) as u8;
                            }
                        }
                    }
                }
            }
        }
    }
    w.align();
}

fn encode_all(seq: &SeqInfo, frames: &[&DecodedFrame], q_code: u8) -> Reencoded {
    let mut w = BitWriter::new();
    write_seq_from(&mut w, seq);
    write_gop_header(&mut w, 0, seq.fps().round().max(1.0) as u32, true);
    let mut pics = Vec::with_capacity(frames.len());
    let mut reference: Option<Planes> = None;
    for (j, f) in frames.iter().enumerate() {
        let cur = Planes::of(f);
        let mut recon = Planes::of(f);
        let start = w.len_bytes();
        encode_picture(&mut w, seq, &cur, reference.as_ref(), &mut recon, j as u32, q_code);
        pics.push((start, w.len_bytes()));
        reference = Some(recon);
    }
    w.align();
    Reencoded { es: w.out, pics, q_code }
}

/// Kodiert die Bilder mit dem feinsten Quantisierer, der ins Budget passt.
pub fn encode_frames(seq: &SeqInfo, frames: &[&DecodedFrame], budget: usize) -> Reencoded {
    const STEPS: [u8; 12] = [2, 3, 4, 5, 6, 8, 10, 12, 15, 19, 24, 31];
    let (mut lo, mut hi) = (0usize, STEPS.len() - 1);
    let mut best = encode_all(seq, frames, STEPS[hi]);
    if best.es.len() > budget {
        return best;
    }
    while lo < hi {
        let mid = (lo + hi) / 2;
        let attempt = encode_all(seq, frames, STEPS[mid]);
        if attempt.es.len() <= budget {
            best = attempt;
            hi = mid;
        } else {
            lo = mid + 1;
        }
    }
    best
}
