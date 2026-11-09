//! Erzeugt einen Zufallsstrom mit Halbbildern, 16x8- und Dual-Prime-Bewegung, den
//! ffmpeg nicht selbst kodieren kann, und vergleicht unsere Decoder-Ausgabe damit.

use std::path::PathBuf;
use std::process::Command;

use discslim::mpeg2::bits::BitWriter;
use discslim::mpeg2::decode::Decoder;
use discslim::mpeg2::encode::{write_gop_header, write_seq_header, EncParams};
use discslim::mpeg2::requant::{write_block, Requant, VideoOpts};
use discslim::mpeg2::tables::*;

const W: usize = 352;
const H: usize = 288;
const MBW: usize = W / 16;
const MBH: usize = H / 16;

struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        (self.0 >> 16) as u32
    }
    fn below(&mut self, n: u32) -> u32 {
        self.next() % n
    }
    fn range(&mut self, a: i32, b: i32) -> i32 {
        a + self.below((b - a + 1) as u32) as i32
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Pic {
    ct: u8,
    structure: u8,
    frame_pred: bool,
    second: bool,
    intra_vlc: bool,
    alt: bool,
    nl: bool,
    first_gop_pair: bool,
    fc: u8,
}

fn write_header(w: &mut BitWriter, temporal: u32, pic: &Pic) {
    w.start_code(0x00);
    w.put(temporal, 10);
    w.put(u32::from(pic.ct), 3);
    w.put(0xFFFF, 16);
    if pic.ct >= 2 {
        w.put(0, 1);
        w.put(7, 3);
    }
    if pic.ct == 3 {
        w.put(0, 1);
        w.put(7, 3);
    }
    w.put(0, 1);
    w.align();
    w.start_code(0xB5);
    w.put(8, 4);
    let f = u32::from(pic.fc);
    let (f0, f1) = match pic.ct {
        1 => (15, 15),
        2 => (f, 15),
        _ => (f, f),
    };
    w.put(f0, 4);
    w.put(f0, 4);
    w.put(f1, 4);
    w.put(f1, 4);
    w.put(0, 2);
    w.put(u32::from(pic.structure), 2);
    w.put(1, 1); // Oberes Halbbild zuerst
    w.put(u32::from(pic.frame_pred), 1);
    w.put(0, 1);
    w.put(u32::from(pic.nl), 1);
    w.put(u32::from(pic.intra_vlc), 1);
    w.put(u32::from(pic.alt), 1);
    w.put(0, 1);
    w.put(1, 1);
    w.put(0, 1);
    w.put(0, 1);
    w.align();
}

fn put_mv(w: &mut BitWriter, delta: i32, fc: u8) {
    let t = tables();
    let f = 1i32 << (fc - 1);
    // Der Decoder rechnet modulo; so bleibt jeder Sprung kodierbar.
    let mut delta = delta;
    if delta < -16 * f {
        delta += 32 * f;
    } else if delta >= 16 * f {
        delta -= 32 * f;
    }
    if delta == 0 {
        w.put(1, 1);
        return;
    }
    let abs = delta.abs();
    let code = (abs - 1) / f + 1;
    let resid = (abs - 1) % f;
    let signed = if delta < 0 { -code } else { code };
    let (c, n) = t.motion_enc[(signed + 16) as usize];
    w.put(u32::from(c), u32::from(n));
    if fc > 1 {
        w.put(resid as u32, u32::from(fc) - 1);
    }
}

fn tokens(rng: &mut Rng) -> Vec<(u8, i16)> {
    let mut v = Vec::new();
    let n = rng.below(6);
    let mut idx = 0u32;
    for _ in 0..n {
        idx += 1 + rng.below(9);
        if idx > 63 {
            break;
        }
        let mut lvl = rng.range(-3, 3);
        if lvl == 0 {
            lvl = 1;
        }
        v.push((idx as u8, lvl as i16));
    }
    v
}

fn put_dc(w: &mut BitWriter, diff: i32, luma: bool) {
    let t = tables();
    let size = 32 - diff.unsigned_abs().leading_zeros();
    let (c, n) = if luma { t.dc_luma_enc[size as usize] } else { t.dc_chroma_enc[size as usize] };
    w.put(u32::from(c), u32::from(n));
    if size > 0 {
        let bits = if diff > 0 { diff as u32 } else { (diff + (1 << size) - 1) as u32 };
        w.put(bits, size);
    }
}

fn write_slices(w: &mut BitWriter, rng: &mut Rng, pic: &Pic) {
    let t = tables();
    let frame = pic.structure == 3;
    let rows = if frame { MBH } else { MBH / 2 };
    let ct_idx = usize::from(pic.ct) - 1;
    for row in 0..rows {
        w.start_code((row + 1) as u8);
        let mut q = rng.range(2, 12) as u32;
        w.put(q, 5);
        w.put(0, 1);
        let mut pmv = [[[0i32; 2]; 2]; 2];
        let mut dc = [128i32; 3];
        let mut prev_intra = true;
        let mut x = 0usize;
        while x < MBW {
            // Gelegentlich übersprungene Makroblöcke, nie am Rand einer Slice
            let mut inc = 1usize;
            let may_skip = pic.ct != 1 && x > 0 && x + 2 < MBW && !prev_intra && !pic.first_gop_pair;
            if may_skip && rng.below(5) == 0 {
                inc = 2;
                x += 1;
                if pic.ct == 2 {
                    pmv = Default::default();
                }
                dc = [128; 3];
            }
            let (c, n) = t.addr_enc[inc];
            w.put(u32::from(c), u32::from(n));

            let interior = x >= 2 && x + 2 < MBW && row >= 2 && row + 2 < rows;
            let mut flags = match pic.ct {
                1 => MB_INTRA,
                2 => match rng.below(5) {
                    0 => MB_FWD | MB_PATTERN,
                    1 if !pic.first_gop_pair => MB_PATTERN,
                    2 => MB_FWD,
                    3 => MB_INTRA,
                    _ => MB_FWD | MB_PATTERN,
                },
                _ => match rng.below(7) {
                    0 => MB_FWD | MB_BWD,
                    1 => MB_FWD | MB_BWD | MB_PATTERN,
                    2 => MB_FWD,
                    3 => MB_BWD | MB_PATTERN,
                    4 => MB_BWD,
                    5 => MB_FWD | MB_PATTERN,
                    _ => MB_INTRA,
                },
            };
            // Ein Quantisierer-Wechsel
            let new_q = flags & (MB_INTRA | MB_PATTERN) != 0 && rng.below(6) == 0;
            if new_q {
                flags |= MB_QUANT;
            }
            let (c, n) = t.mb_enc[ct_idx][flags as usize];
            assert!(n > 0);
            w.put(u32::from(c), u32::from(n));

            let intra = flags & MB_INTRA != 0;
            let pattern = flags & MB_PATTERN != 0;
            let mut motion = if frame { 2u32 } else { 1u32 };
            if flags & (MB_FWD | MB_BWD) != 0 && (!frame || !pic.frame_pred) {
                // Dual Prime leitet Vektoren ab; am Bildrand zeigten sie sonst aus dem Bild.
                motion = if pic.ct == 2 && !pic.first_gop_pair && flags & MB_BWD == 0 && interior {
                    1 + rng.below(3)
                } else {
                    1 + rng.below(2)
                };
                w.put(motion, 2);
            }
            if frame && !pic.frame_pred && (intra || pattern) {
                w.put(rng.below(2), 1);
            }
            if new_q {
                q = rng.range(2, 12) as u32;
                w.put(q, 5);
            }

            for s in 0..2usize {
                let use_it = if s == 0 { flags & MB_FWD != 0 } else { flags & MB_BWD != 0 };
                if !use_it {
                    continue;
                }
                let amp = if interior { 14 } else { 0 };
                let parity = u32::from(pic.structure == 2);
                let vectors = if (frame && motion == 1) || (!frame && motion == 2) { 2 } else { 1 };
                let halve = frame && motion != 2; // Halbbild-Vektor im Vollbild
                let dual = motion == 3;
                for r in 0..vectors {
                    if !dual && !(frame && motion == 2) {
                        // Das erste P-Paar darf nur das erste Halbbild desselben Bildes nutzen.
                        let sel = if pic.first_gop_pair { 1 - parity } else { rng.below(2) };
                        w.put(sel, 1);
                    }
                    let vh = rng.range(-amp, amp);
                    let vv = rng.range(-amp, amp);
                    put_mv(w, vh - pmv[r][s][0], pic.fc);
                    if dual {
                        w.put(0, 1); // Differenz null
                    }
                    let pred_v = if halve { pmv[r][s][1] >> 1 } else { pmv[r][s][1] };
                    put_mv(w, vv - pred_v, pic.fc);
                    if dual {
                        w.put(0, 1);
                    }
                    let stored = [vh, if halve { vv * 2 } else { vv }];
                    if vectors == 1 {
                        pmv[0][s] = stored;
                        pmv[1][s] = stored;
                    } else {
                        pmv[r][s] = stored;
                    }
                }
            }

            let mut cbp = 0u32;
            if pattern {
                cbp = 1 + rng.below(63);
                let (c, n) = t.cbp_enc[cbp as usize];
                w.put(u32::from(c), u32::from(n));
            }

            if intra {
                pmv = Default::default();
                let vlc_table = usize::from(pic.intra_vlc);
                for b in 0..6usize {
                    let cc = if b < 4 { 0 } else { b - 3 };
                    let target = rng.range(40, 215);
                    put_dc(w, target - dc[cc], b < 4);
                    dc[cc] = target;
                    write_block(w, &tokens(rng), true, vlc_table);
                }
            } else {
                dc = [128; 3];
                if pic.ct == 2 && flags & MB_FWD == 0 {
                    pmv = Default::default();
                }
                for b in 0..6usize {
                    if cbp & (1 << (5 - b)) != 0 {
                        let mut tk = tokens(rng);
                        // Nicht-Intra-Blöcke beginnen bei Position 0 und sind nie leer
                        if tk.is_empty() {
                            tk.push((rng.below(5) as u8, 1));
                        } else if rng.below(2) == 0 {
                            tk.insert(0, (0, if rng.below(2) == 0 { 1 } else { -1 }));
                        }
                        write_block(w, &tk, false, 0);
                    }
                }
            }
            prev_intra = intra;
            x += 1;
        }
    }
    w.align();
}

fn build_stream(seed: u64, intra_vlc: bool, alt: bool, nl: bool) -> Vec<u8> {
    let mut rng = Rng(seed);
    let mut w = BitWriter::new();
    let p = EncParams { width: W as u32, height: H as u32, aspect: 2, fps_code: 3, bitrate_400: 15_000 };
    let base = Pic { ct: 1, structure: 3, frame_pred: false, second: false, intra_vlc, alt, nl, first_gop_pair: false, fc: 2 };

    for gop in 0..3u64 {
        write_seq_header(&mut w, &p);
        write_gop_header(&mut w, gop * 7, 25, true);

        // I-Halbbild plus P-Halbbild
        let i_top = Pic { ct: 1, structure: 1, ..base };
        write_header(&mut w, 0, &i_top);
        write_slices(&mut w, &mut rng, &i_top);
        let p_bottom = Pic { ct: 2, structure: 2, second: true, first_gop_pair: true, ..base };
        write_header(&mut w, 0, &p_bottom);
        write_slices(&mut w, &mut rng, &p_bottom);

        // P als Vollbild mit Halbbild- und Dual-Prime-Bewegung
        let pf = Pic { ct: 2, structure: 3, ..base };
        write_header(&mut w, 3, &pf);
        write_slices(&mut w, &mut rng, &pf);

        // B-Halbbildpaar
        let bt = Pic { ct: 3, structure: 1, ..base };
        write_header(&mut w, 1, &bt);
        write_slices(&mut w, &mut rng, &bt);
        let bb = Pic { ct: 3, structure: 2, second: true, ..base };
        write_header(&mut w, 1, &bb);
        write_slices(&mut w, &mut rng, &bb);

        // B als Vollbild
        let bf = Pic { ct: 3, structure: 3, ..base };
        write_header(&mut w, 2, &bf);
        write_slices(&mut w, &mut rng, &bf);

        // P-Halbbildpaar, auch mit Dual Prime
        let pt = Pic { ct: 2, structure: 1, ..base };
        write_header(&mut w, 6, &pt);
        write_slices(&mut w, &mut rng, &pt);
        let pb = Pic { ct: 2, structure: 2, second: true, ..base };
        write_header(&mut w, 6, &pb);
        write_slices(&mut w, &mut rng, &pb);

        // Zwei B-Bilder dazu
        for tr in [4u32, 5] {
            let b = Pic { ct: 3, structure: 3, frame_pred: tr == 5, ..base };
            write_header(&mut w, tr, &b);
            write_slices(&mut w, &mut rng, &b);
        }
    }
    w.align();
    w.out
}

fn ffmpeg() -> Option<PathBuf> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let dir = root.join("target").join("devtools").join("ffdl").join("x").join("imageio_ffmpeg").join("binaries");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "exe"))
}

fn ff_decode(ff: &PathBuf, input: &PathBuf) -> (Vec<u8>, String) {
    let out = Command::new(ff)
        .args(["-v", "error", "-i"])
        .arg(input)
        .args(["-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg");
    (out.stdout, String::from_utf8_lossy(&out.stderr).to_string())
}

fn my_decode(es: &[u8]) -> Vec<u8> {
    let mut d = Decoder::new();
    d.push(es);
    d.flush();
    let mut out = Vec::new();
    while let Some(f) = d.pop_frame() {
        for r in 0..f.height {
            out.extend_from_slice(&f.y[r * f.stride..r * f.stride + f.width]);
        }
        let cs = f.stride / 2;
        for r in 0..f.height / 2 {
            out.extend_from_slice(&f.u[r * cs..r * cs + f.width / 2]);
        }
        for r in 0..f.height / 2 {
            out.extend_from_slice(&f.v[r * cs..r * cs + f.width / 2]);
        }
    }
    out
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    let mut sse = 0f64;
    for i in 0..n {
        let d = f64::from(a[i]) - f64::from(b[i]);
        sse += d * d;
    }
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 * n as f64 / sse).log10()
}

#[test]
fn fields_and_dual_prime_match_reference() {
    let Some(ff) = ffmpeg() else { return };
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("testwork").join("fields");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    for (i, (vlc, alt, nl)) in [(false, false, false), (true, true, true), (true, false, true)].iter().enumerate() {
        let es = build_stream(0x1234_5678_9ABC + i as u64 * 977, *vlc, *alt, *nl);
        let path = dir.join(format!("s{i}.m2v"));
        std::fs::write(&path, &es).unwrap();
        let (reference, err) = ff_decode(&ff, &path);
        println!("Referenz-Meldungen: {err}");
        assert!(err.is_empty(), "{err}");
        let mine = my_decode(&es);
        assert_eq!(mine.len(), reference.len(), "Bildanzahl");
        let fs = W * H * 3 / 2;
        for k in 0..mine.len() / fs {
            let pp = psnr(&mine[k * fs..(k + 1) * fs], &reference[k * fs..(k + 1) * fs]);
            println!("Variante {i} Bild {k}: {pp:.1} dB");
        }
        let p = psnr(&mine, &reference);
        println!("Variante {i}: gesamt {p:.1} dB");
        assert!(p > 55.0, "Variante {i}: {p:.1} dB");

        // Der Verkleinerer muss den Strom verstehen und gültig ausgeben.
        let mut rq = Requant::new();
        rq.set_opts(VideoOpts { ratio: 0.7, smooth_cutoff: 64, still: false });
        let out = rq.process(&es);
        assert_eq!(rq.slices_fallback, 0, "Slices nicht verstanden");
        let small = dir.join(format!("s{i}_small.m2v"));
        std::fs::write(&small, &out.es).unwrap();
        let (decoded, err) = ff_decode(&ff, &small);
        assert!(err.is_empty(), "verkleinert: {err}");
        assert_eq!(decoded.len(), reference.len());
        let m2 = my_decode(&out.es);
        let q = psnr(&m2, &decoded);
        println!("Variante {i}: verkleinert {:.3}, eigener Decoder {q:.1} dB", out.es.len() as f64 / es.len() as f64);
        assert!(q > 55.0);
    }
}

/// Beschädigte Daten dürfen nie zum Absturz führen, höchstens zu Bildfehlern.
#[test]
fn damaged_streams_do_not_crash() {
    let clean = build_stream(0xABCD_EF01_2345, true, false, true);
    let mut rng = Rng(0x9E37_79B9_7F4A);
    for _ in 0..40 {
        let mut es = clean.clone();
        for _ in 0..200 {
            let at = rng.below(es.len() as u32) as usize;
            es[at] ^= 1 << rng.below(8);
        }
        let mut rq = Requant::new();
        rq.set_opts(VideoOpts { ratio: 0.6, smooth_cutoff: 40, still: false });
        let out = rq.process(&es);
        let mut still = Requant::new();
        still.set_opts(VideoOpts { ratio: 0.12, smooth_cutoff: 64, still: true });
        let _ = still.process(&es);
        for stream in [&es, &out.es] {
            let mut d = Decoder::new();
            d.push(stream);
            d.flush();
            while d.pop_frame().is_some() {}
        }
    }
}
