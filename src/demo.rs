//! Erzeugt eine kleine, echte Video-DVD (ohne Kopierschutz) zum Ausprobieren und
//! Testen: mehrere Titel, Kapitel, zwei Tonspuren, Untertitel, Menüs.

use std::path::{Path, PathBuf};

use crate::dvd::ifo::{encode_time, CellAddr, TitleEntry};
use crate::dvd::vob::*;
use crate::dvd::{put16, put32, Res, SECTOR};
use crate::engine::author::{self, ACell, AMenu, APgc, AVmg, AVts, TimeMap};
use crate::mpeg2::bits::{next_start_code, BitWriter};
use crate::mpeg2::encode::*;

const PTS_OFFSET: u64 = 45_000;

/// Fertige VOBU, noch ohne endgültige Sektornummern.
struct Vobu {
    data: Vec<u8>,
    frames: u64,
    start_pts: u64,
    end_pts: u64,
    cell: u8,
    vob: u16,
    cell_elapsed_ms: u64,
}

pub(crate) fn pack_header(scr: u64) -> [u8; 14] {
    let mut h = [0u8; 14];
    h[0..4].copy_from_slice(&[0, 0, 1, 0xBA]);
    put_scr(&mut h, scr);
    // 10,08 MBit/s in Einheiten von 50 Byte/s
    let mux: u32 = 25_200;
    h[10] = (mux >> 14) as u8;
    h[11] = (mux >> 6) as u8;
    h[12] = ((mux << 2) as u8) | 3;
    h[13] = 0xF8;
    h
}

fn audio_pack(scr: u64, pts: u64, stream: u8, samples: &[u8]) -> ([u8; SECTOR], usize) {
    let mut s = [0u8; SECTOR];
    s[..14].copy_from_slice(&pack_header(scr));
    let header = 9 + 5 + 7; // PES-Kopf, Zeitstempel, LPCM-Kopf
    let room = (SECTOR - 14 - header) & !3;
    let take = samples.len().min(room);
    let pes_len = 3 + 5 + 7 + take;
    s[14..18].copy_from_slice(&[0, 0, 1, 0xBD]);
    put16(&mut s, 18, pes_len as u16);
    s[20] = 0x81;
    s[21] = 0x80;
    s[22] = 5;
    write_ts(&mut s[23..], 2, pts);
    let o = 28;
    s[o] = 0xA0 | stream;
    s[o + 1] = 1; // Audio-Rahmen
    put16(&mut s, o + 2, 4);
    s[o + 4] = 0;
    s[o + 5] = 0x01; // 16 Bit, 48 kHz, Stereo
    s[o + 6] = 0x80;
    s[o + 7..o + 7 + take].copy_from_slice(&samples[..take]);
    let used = o + 7 + take;
    if SECTOR - used >= 7 {
        let pad = SECTOR - used;
        s[used..used + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
        put16(&mut s, used + 4, (pad - 6) as u16);
        s[used + 6..SECTOR].fill(0xFF);
    } else {
        // Reste passen in den PES-Kopf nicht mehr hinein: als Stille auffüllen
        let extra = SECTOR - used;
        put16(&mut s, 18, (pes_len + extra) as u16);
    }
    (s, take)
}

fn sub_pack(scr: u64, pts: u64, stream: u8) -> [u8; SECTOR] {
    let mut s = [0u8; SECTOR];
    s[..14].copy_from_slice(&pack_header(scr));
    let spu: [u8; 9] = [0, 9, 0, 4, 0, 0, 0, 4, 0xFF];
    let pes_len = 3 + 5 + 1 + spu.len();
    s[14..18].copy_from_slice(&[0, 0, 1, 0xBD]);
    put16(&mut s, 18, pes_len as u16);
    s[20] = 0x81;
    s[21] = 0x80;
    s[22] = 5;
    write_ts(&mut s[23..], 2, pts);
    s[28] = 0x20 | stream;
    s[29..29 + spu.len()].copy_from_slice(&spu);
    let used = 29 + spu.len();
    let pad = SECTOR - used;
    s[used..used + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
    put16(&mut s, used + 4, (pad - 6) as u16);
    s[used + 6..SECTOR].fill(0xFF);
    s
}

pub(crate) fn nav_pack(scr: u64) -> Vec<u8> {
    let mut s = vec![0u8; SECTOR];
    s[..14].copy_from_slice(&pack_header(scr));
    let sys: [u8; 24] = [
        0, 0, 1, 0xBB, 0x00, 0x12, 0x80, 0xC4, 0xE1, 0x00, 0xE1, 0xFF, 0xB9, 0xE0, 0xE8, 0xB8, 0xC0, 0x20, 0xBD, 0xE0,
        0x3A, 0xBF, 0xE0, 0x02,
    ];
    s[14..38].copy_from_slice(&sys);
    s[0x26..0x2A].copy_from_slice(&[0, 0, 1, 0xBF]);
    put16(&mut s, 0x2A, 0x03D4);
    s[0x2C] = 0;
    s[0x400..0x404].copy_from_slice(&[0, 0, 1, 0xBF]);
    put16(&mut s, 0x404, 0x03FA);
    s[0x406] = 1;
    s
}

/// Eine Gruppe von Bildern und ihre Lage im Strom.
struct GopInfo {
    start: usize,
    end: usize,
    pics: Vec<(usize, u32, u8)>,
    frames: u64,
}

/// Teilt einen Elementarstrom an Sequenz- oder GOP-Köpfen.
fn split_gops(es: &[u8]) -> Vec<GopInfo> {
    let mut gops: Vec<GopInfo> = Vec::new();
    let mut cur: Option<GopInfo> = None;
    let mut seq_before: Option<usize> = None;
    let mut i = next_start_code(es, 0).unwrap_or(es.len());
    let mut seen_pic_in_gop = false;
    while i + 4 <= es.len() {
        let code = es[i + 3];
        let j = next_start_code(es, i + 4).unwrap_or(es.len());
        match code {
            0xB3 => {
                if seen_pic_in_gop {
                    if let Some(mut g) = cur.take() {
                        g.end = i;
                        gops.push(g);
                    }
                    seen_pic_in_gop = false;
                }
                if cur.is_none() {
                    seq_before = Some(i);
                }
            }
            0xB8 => {
                if seen_pic_in_gop {
                    if let Some(mut g) = cur.take() {
                        g.end = i;
                        gops.push(g);
                    }
                    seen_pic_in_gop = false;
                }
                if cur.is_none() {
                    cur = Some(GopInfo {
                        start: seq_before.take().unwrap_or(i),
                        end: 0,
                        pics: Vec::new(),
                        frames: 0,
                    });
                }
            }
            0x00 => {
                if cur.is_none() {
                    cur = Some(GopInfo {
                        start: seq_before.take().unwrap_or(i),
                        end: 0,
                        pics: Vec::new(),
                        frames: 0,
                    });
                }
                let tr = (u32::from(es[i + 4]) << 2) | (u32::from(es[i + 5]) >> 6);
                let ty = (es[i + 5] >> 3) & 7;
                if let Some(g) = cur.as_mut() {
                    let new_frame = g.pics.last().map(|p| p.1 != tr).unwrap_or(true);
                    if new_frame {
                        g.frames += 1;
                    }
                    g.pics.push((i, tr, ty));
                }
                seen_pic_in_gop = true;
            }
            _ => {}
        }
        i = j;
    }
    if let Some(mut g) = cur.take() {
        g.end = es.len();
        gops.push(g);
    }
    gops
}

struct AudioGen {
    phase: f32,
    freq: f32,
}

impl AudioGen {
    fn samples(&mut self, count: usize) -> Vec<u8> {
        let mut v = Vec::with_capacity(count * 4);
        for _ in 0..count {
            self.phase += self.freq / 48_000.0;
            if self.phase > 1.0 {
                self.phase -= 1.0;
            }
            let val = if self.freq > 0.0 {
                ((self.phase * std::f32::consts::TAU).sin() * 4000.0) as i16
            } else {
                0
            };
            let b = val.to_be_bytes();
            v.extend_from_slice(&b);
            v.extend_from_slice(&b);
        }
        v
    }
}

#[derive(Clone)]
pub struct TrackSpec {
    pub lang: &'static [u8; 2],
    pub freq: f32,
}

/// Beschreibung eines Titels für den Muxer.
pub struct TitleSpec {
    pub es: Vec<u8>,
    /// Anzahl GOPs je Kapitel; der Rest fällt ins letzte Kapitel.
    pub gops_per_chapter: usize,
    pub audio: Vec<TrackSpec>,
    pub subs: Vec<&'static [u8; 2]>,
    pub fps: f64,
    pub wide: bool,
}

fn mux_title(spec: &TitleSpec, vob: u16) -> (Vec<Vobu>, Vec<u64>) {
    let gops = split_gops(&spec.es);
    let frame_dur = 90_000.0 / spec.fps;
    let mut vobus = Vec::with_capacity(gops.len());
    let mut chapter_ms = Vec::new();
    let mut frames_total = 0u64;
    let mut gens: Vec<AudioGen> = spec.audio.iter().map(|t| AudioGen { phase: 0.0, freq: t.freq }).collect();
    let mut samples_done = 0u64;
    let mut cell = 0u8;
    let mut cell_start_frame = 0u64;

    for (gi, g) in gops.iter().enumerate() {
        if gi % spec.gops_per_chapter.max(1) == 0 && cell < 250 {
            cell += 1;
            cell_start_frame = frames_total;
            chapter_ms.push(0);
        }
        // Dekodierreihenfolge zu Zeitstempeln
        let base = PTS_OFFSET + (frames_total as f64 * frame_dur) as u64;
        let start_pts = base;
        let frames = g.frames.max(1);
        let end_pts = base + (frames as f64 * frame_dur) as u64;

        let mut pts_of = Vec::with_capacity(g.pics.len());
        let mut dts_index = 0u64;
        let mut prev_tr = None;
        for (at, tr, _) in &g.pics {
            if prev_tr.is_some() && prev_tr != Some(*tr) {
                dts_index += 1;
            }
            prev_tr = Some(*tr);
            let pts = base + (f64::from(*tr) * frame_dur) as u64;
            let dts = (base as f64 + (dts_index as f64 - 1.0) * frame_dur) as u64;
            pts_of.push((*at - g.start, pts, dts.min(pts)));
        }

        let es = &spec.es[g.start..g.end];
        let mut vobu = Vec::with_capacity(es.len() + 64 * SECTOR);
        let mut scr90 = base.saturating_sub(20_000);
        let tick = |scr: &mut u64| {
            let v = *scr * 300;
            *scr += 147;
            v
        };
        vobu.extend_from_slice(&nav_pack(tick(&mut scr90)));

        // Ton für die Dauer der GOP
        let wanted_samples = ((frames_total + frames) as f64 / spec.fps * 48_000.0) as u64 - samples_done;
        // Zeitstempel je Tonpaket: Zeit des ersten Abtastwerts darin
        let audio_base = samples_done;
        let audio_pts = move |byte: usize| PTS_OFFSET + (audio_base + (byte / 4) as u64) * 90_000 / 48_000;
        samples_done += wanted_samples;
        let mut tracks: Vec<(usize, Vec<u8>, usize)> =
            gens.iter_mut().enumerate().map(|(i, g)| (i, g.samples(wanted_samples as usize), 0)).collect();

        let video_packs_est = (es.len() / 2000).max(1);
        let audio_bytes: usize = tracks.iter().map(|t| t.1.len()).sum();
        let audio_packs_est = (audio_bytes / 2000).max(1);
        let audio_every = (video_packs_est / audio_packs_est.max(1)).max(1);

        let mut emitted = 0usize;
        let mut idx_pts = 0usize;
        let mut count = 0usize;
        while emitted < es.len() {
            // Nächstes Bild im Paket?
            while idx_pts < pts_of.len() && pts_of[idx_pts].0 < emitted {
                idx_pts += 1;
            }
            let mut ts = None;
            if let Some(p) = pts_of.get(idx_pts) {
                if p.0 < emitted + SECTOR - 14 - 9 - 10 {
                    ts = Some((p.1, Some(p.2)));
                }
            }
            let hdr = pack_header(tick(&mut scr90));
            let (sector, take) = build_video_pack(&hdr, ts, &[], &es[emitted..]);
            vobu.extend_from_slice(&sector);
            emitted += take;
            count += 1;

            if count % audio_every == 0 {
                for t in tracks.iter_mut() {
                    if t.2 < t.1.len() {
                        let (s, n) = audio_pack(tick(&mut scr90), audio_pts(t.2), t.0 as u8, &t.1[t.2..]);
                        vobu.extend_from_slice(&s);
                        t.2 += n;
                    }
                }
            }
        }
        // Rest des Tons
        for t in tracks.iter_mut() {
            while t.2 < t.1.len() {
                let (s, n) = audio_pack(tick(&mut scr90), audio_pts(t.2), t.0 as u8, &t.1[t.2..]);
                vobu.extend_from_slice(&s);
                t.2 += n;
            }
        }
        for sub in 0..spec.subs.len() {
            let s = sub_pack(tick(&mut scr90), start_pts, sub as u8);
            vobu.extend_from_slice(&s);
        }

        let ms = (cell_elapsed(frames_total, cell_start_frame) * 1000.0 / spec.fps) as u64;
        vobus.push(Vobu {
            data: vobu,
            frames,
            start_pts,
            end_pts,
            cell,
            vob,
            cell_elapsed_ms: ms,
        });
        frames_total += frames;
    }

    // Kapitellängen
    let mut per_cell: Vec<u64> = vec![0; usize::from(cell)];
    for v in &vobus {
        per_cell[usize::from(v.cell) - 1] += v.frames;
    }
    for (i, f) in per_cell.iter().enumerate() {
        chapter_ms[i] = (*f as f64 * 1000.0 / spec.fps) as u64;
    }
    (vobus, chapter_ms)
}

fn cell_elapsed(frames_total: u64, cell_start: u64) -> f64 {
    (frames_total - cell_start) as f64
}

/// Vergibt endgültige Sektornummern und trägt alle Verweise in die NAV-Pakete ein.
fn finish_vobus(vobus: &mut [Vobu], fps: f64, pal: bool) -> (Vec<u8>, Vec<u32>, Vec<CellAddr>, Vec<(u64, u32)>) {
    let starts: Vec<u32> = vobus
        .iter()
        .scan(0u32, |acc, v| {
            let s = *acc;
            *acc += (v.data.len() / SECTOR) as u32;
            Some(s)
        })
        .collect();
    let total_frames_before: Vec<u64> = vobus
        .iter()
        .scan(0u64, |acc, v| {
            let s = *acc;
            *acc += v.frames;
            Some(s)
        })
        .collect();
    let mut out = Vec::new();
    let mut cells: Vec<CellAddr> = Vec::new();
    let mut time_points = Vec::new();
    let half_second = (fps / 2.0).round() as u64;
    let n = vobus.len();
    for i in 0..n {
        let len = (vobus[i].data.len() / SECTOR) as u32;
        let start = starts[i];
        let cell = vobus[i].cell;
        let vob = vobus[i].vob;
        let (s_ptm, e_ptm) = (vobus[i].start_pts as u32, vobus[i].end_pts as u32);
        let elapsed = vobus[i].cell_elapsed_ms;
        let nav = &mut vobus[i].data[..SECTOR];

        put32(nav, PCI_OFF, start);
        put32(nav, PCI_OFF + 12, s_ptm);
        put32(nav, PCI_OFF + 16, e_ptm);
        put32(nav, DSI_OFF, (get_scr(nav) / 300) as u32);
        put32(nav, DSI_OFF + 4, start);
        put32(nav, DSI_OFF + 8, len - 1);
        // Erstes Referenzbild: grob der gesamte Bereich der ersten Pakete
        put32(nav, DSI_OFF + 12, (len - 1).min(1));
        put16(nav, DSI_OFF + 24, vob);
        nav[DSI_OFF + 27] = cell;
        nav[DSI_OFF + 28..DSI_OFF + 32].copy_from_slice(&encode_time(elapsed, pal));

        // Sprungtabelle
        let target_fwd = |secs_half: u64| -> u32 {
            let want = total_frames_before[i] + secs_half * half_second;
            let mut j = i;
            while j + 1 < n && total_frames_before[j] < want {
                j += 1;
            }
            if j == i {
                SRI_END
            } else {
                starts[j] - start
            }
        };
        let target_back = |secs_half: u64| -> u32 {
            let want = total_frames_before[i].saturating_sub(secs_half * half_second);
            let mut j = i;
            while j > 0 && total_frames_before[j] > want {
                j -= 1;
            }
            if j == i {
                SRI_END
            } else {
                start - starts[j]
            }
        };
        const STEPS: [u64; 19] = [240, 120, 60, 20, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
        let base = DSI_OFF + DSI_SRI;
        let next = if i + 1 < n { (starts[i + 1] - start) | 0x8000_0000 } else { SRI_END };
        put32(nav, base, next);
        for (k, step) in STEPS.iter().enumerate() {
            let v = target_fwd(*step);
            put32(nav, base + 4 + k * 4, if v == SRI_END { v } else { v | 0x8000_0000 });
        }
        put32(nav, base + 80, if i + 1 < n { starts[i + 1] - start } else { SRI_END });
        put32(nav, base + 84, if i > 0 { start - starts[i - 1] } else { SRI_END });
        for (k, step) in STEPS.iter().rev().enumerate() {
            let v = target_back(*step);
            put32(nav, base + 88 + k * 4, if v == SRI_END { v } else { v | 0x8000_0000 });
        }
        let prev = if i > 0 { (start - starts[i - 1]) | 0x8000_0000 } else { SRI_END };
        put32(nav, base + 164, prev);

        // Zeiger auf ersten Ton und Untertitel: unbekannt
        for k in 0..8 {
            put16(nav, DSI_OFF + DSI_SYNCI + k * 2, 0x3FFF);
        }

        match cells.last_mut() {
            Some(c) if c.cell_id == cell && c.vob_id == vob => c.last = start + len - 1,
            _ => cells.push(CellAddr { vob_id: vob, cell_id: cell, start, last: start + len - 1 }),
        }
        time_points.push(((total_frames_before[i] as f64 * 1000.0 / fps) as u64, start));
        out.extend_from_slice(&vobus[i].data);
    }
    (out, starts, cells, time_points)
}

fn make_attrs(spec: &TitleSpec) -> Vec<u8> {
    let mut a = vec![0u8; author::ATTR_END - author::ATTR_START];
    let at = |o: usize| o - author::ATTR_START;
    let video = [if spec.wide { 0x5D } else { 0x51 }, 0x00];
    a[at(0x100)..at(0x102)].copy_from_slice(&video);
    a[at(0x200)..at(0x202)].copy_from_slice(&video);
    put16(&mut a, at(0x202), spec.audio.len() as u16);
    for (i, t) in spec.audio.iter().enumerate() {
        let o = at(0x204) + i * 8;
        a[o] = 0x84;
        a[o + 1] = 0x01;
        a[o + 2..o + 4].copy_from_slice(t.lang);
    }
    put16(&mut a, at(0x254), spec.subs.len() as u16);
    for (i, l) in spec.subs.iter().enumerate() {
        let o = at(0x256) + i * 6;
        a[o] = 0x01;
        a[o + 2..o + 4].copy_from_slice(*l);
    }
    a
}

fn make_pgc(spec: &TitleSpec, chapter_ms: &[u64], starts: &[u32], cells: &[CellAddr], vobus: &[Vobu], pal: bool, post: author::Cmd) -> APgc {
    let mut pgc = APgc { pal, ..Default::default() };
    for (i, _) in spec.audio.iter().enumerate() {
        pgc.audio_ctl[i] = 0x8000 | ((i as u16) << 8);
    }
    for (i, _) in spec.subs.iter().enumerate() {
        let n = i as u32;
        pgc.sub_ctl[i] = 0x8000_0000 | (n << 24) | (n << 16) | (n << 8) | n;
    }
    // Einfache Graustufen-Palette für Untertitel
    for i in 0..16 {
        pgc.palette[i * 4 + 1] = (16 + i * 14) as u8;
        pgc.palette[i * 4 + 2] = 128;
        pgc.palette[i * 4 + 3] = 128;
    }
    for (i, c) in cells.iter().enumerate() {
        let last_vobu = starts
            .iter()
            .zip(vobus.iter())
            .filter(|(_, v)| v.cell == c.cell_id)
            .map(|(s, _)| *s)
            .max()
            .unwrap_or(c.start);
        pgc.cells.push(ACell {
            ms: chapter_ms.get(i).copied().unwrap_or(0),
            flags: [0, 0, 0, 0],
            first: c.start,
            last_vobu,
            last: c.last,
            vob: c.vob_id,
            cell: c.cell_id,
        });
        pgc.programs.push((i + 1) as u8);
    }
    pgc.post = vec![post];
    pgc
}

pub(crate) fn make_tmap(points: &[(u64, u32)]) -> TimeMap {
    let dur = points.last().map(|p| p.0).unwrap_or(0);
    let unit = ((dur / 1000 / 2040) + 1).clamp(1, 255) as u8;
    let mut entries = Vec::new();
    let mut k = 1u64;
    let mut idx = 0usize;
    while k * u64::from(unit) * 1000 < dur {
        let at = k * u64::from(unit) * 1000;
        while idx + 1 < points.len() && points[idx + 1].0 <= at {
            idx += 1;
        }
        entries.push(points[idx].1);
        k += 1;
    }
    TimeMap { unit_secs: unit, entries }
}

/// Legt aus den Angaben einen kompletten VIDEO_TS-Ordner an.
pub fn author_disc(
    out_dir: &Path,
    titlesets: Vec<(Vec<TitleSpec>, Option<TitleSpec>)>,
    vmg_menu: Option<TitleSpec>,
    pal: bool,
) -> Res<()> {
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let write = |name: &str, data: &[u8]| -> Res<()> {
        let p = out_dir.join(name);
        std::fs::write(&p, data).map_err(|e| format!("{}: {e}", p.display()))
    };
    let mut tt_entries = Vec::new();
    let mut vts_attrs = Vec::new();
    let mut sizes = Vec::new();
    let mut title_no = 1usize;
    let mut video_attr = [0x51u8, 0];

    for (i, (titles, menu)) in titlesets.iter().enumerate() {
        let vts_no = i + 1;
        let mut all = Vec::new();
        let mut pgcs = Vec::new();
        let mut tmaps = Vec::new();
        let mut vob_data = Vec::new();
        let mut cells_all: Vec<CellAddr> = Vec::new();
        let mut admap: Vec<u32> = Vec::new();
        let mut base = 0u32;
        for (ti, spec) in titles.iter().enumerate() {
            let (mut vobus, chapter_ms) = mux_title(spec, (ti + 1) as u16);
            let (mut data, starts, cells, points) = finish_vobus(&mut vobus, spec.fps, pal);
            // Sektoren dieses Titels liegen hinter den vorigen Titeln im selben VOB.
            let sectors = (data.len() / SECTOR) as u32;
            let shifted: Vec<u32> = starts.iter().map(|s| s + base).collect();
            let cells_shift: Vec<CellAddr> = cells
                .iter()
                .map(|c| CellAddr { start: c.start + base, last: c.last + base, ..*c })
                .collect();
            let pts: Vec<(u64, u32)> = points.iter().map(|p| (p.0, p.1 + base)).collect();
            for (k, s) in data.chunks_exact_mut(SECTOR).enumerate() {
                if is_nav(s) {
                    put32(s, PCI_OFF, base + k as u32);
                    put32(s, DSI_OFF + 4, base + k as u32);
                }
            }
            let next = if title_no + ti < 99 && ti + 1 < titles.len() {
                author::cmd_jump_tt((title_no + ti + 1) as u8)
            } else {
                author::CMD_EXIT
            };
            let pgc = make_pgc(spec, &chapter_ms, &shifted, &cells_shift, &vobus, pal, next);
            tt_entries.push(TitleEntry {
                playback_type: 0x3C,
                angles: 1,
                chapters: pgc.programs.len() as u16,
                vts: vts_no as u8,
                vts_ttn: (ti + 1) as u8,
                vts_sector: 0,
            });
            pgcs.push(pgc);
            tmaps.push(make_tmap(&pts));
            admap.extend_from_slice(&shifted);
            cells_all.extend_from_slice(&cells_shift);
            vob_data.extend_from_slice(&data);
            all.push(sectors);
            base += sectors;
        }
        title_no += titles.len();
        let first = titles.first().ok_or("Titelset ohne Titel")?;
        let attrs = make_attrs(first);
        video_attr = [attrs[0], attrs[1]];

        let mut amenu = None;
        if let Some(m) = menu {
            let (mut vobus, chapter_ms) = mux_title(m, 1);
            let (data, starts, cells, _) = finish_vobus(&mut vobus, m.fps, pal);
            let mut pgc = make_pgc(m, &chapter_ms, &starts, &cells, &vobus, pal, author::cmd_link_pgcn(1));
            pgc.post = vec![author::cmd_link_pgcn(1)];
            write(&format!("VTS_{vts_no:02}_0.VOB"), &data)?;
            amenu = Some(AMenu {
                pgc,
                cells,
                admap: starts,
                sectors: (data.len() / SECTOR) as u32,
                category: 0x83,
                lang_flags: 0x80,
            });
        }

        let avts = AVts {
            attrs: attrs.clone(),
            pgcs,
            menu: amenu,
            cells: cells_all,
            admap,
            tmaps,
            title_sectors: (vob_data.len() / SECTOR) as u32,
        };
        write(&format!("VTS_{vts_no:02}_1.VOB"), &vob_data)?;
        let ifo = author::build_vts(&avts);
        write(&format!("VTS_{vts_no:02}_0.IFO"), &ifo)?;
        write(&format!("VTS_{vts_no:02}_0.BUP"), &ifo)?;
        vts_attrs.push(attrs);
        sizes.push((ifo.len() / SECTOR) as u32 * 2 + avts.title_sectors + avts.menu.map(|m| m.sectors).unwrap_or(0));
        let _ = all;
    }

    let mut menu_block = None;
    if let Some(m) = &vmg_menu {
        let (mut vobus, chapter_ms) = mux_title(m, 1);
        let (data, starts, cells, _) = finish_vobus(&mut vobus, m.fps, pal);
        let pgc = make_pgc(m, &chapter_ms, &starts, &cells, &vobus, pal, author::cmd_jump_tt(1));
        write("VIDEO_TS.VOB", &data)?;
        menu_block = Some(AMenu {
            pgc,
            cells,
            admap: starts,
            sectors: (data.len() / SECTOR) as u32,
            category: 0x82,
            lang_flags: 0x80,
        });
    }

    let mut vmg = AVmg {
        titles: tt_entries,
        vts_attrs,
        first_play: vec![author::cmd_jump_tt(1)],
        menu: menu_block,
        menu_video: video_attr,
        pal,
    };
    let probe = author::build_vmg(&vmg);
    let menu_size = vmg.menu.as_ref().map(|m| m.sectors).unwrap_or(0);
    let vmg_sectors = (probe.len() / SECTOR) as u32 * 2 + menu_size;
    author::set_vts_starts(&mut vmg, &sizes, vmg_sectors);
    let bytes = author::build_vmg(&vmg);
    write("VIDEO_TS.IFO", &bytes)?;
    write("VIDEO_TS.BUP", &bytes)?;
    Ok(())
}

// --- Testbild ---------------------------------------------------------------

const SAMPLE_GOP: u32 = 6;

/// Siebensegment-Ziffer: Welche Balken leuchten.
const DIGITS: [u8; 10] = [
    0b0111111, 0b0000110, 0b1011011, 0b1001111, 0b1100110, 0b1101101, 0b1111101, 0b0000111, 0b1111111, 0b1101111,
];

struct Painter {
    w: usize,
    h: usize,
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

impl Painter {
    fn new(mbw: usize, mbh: usize) -> Self {
        let (w, h) = (mbw * 16, mbh * 16);
        Self { w, h, y: vec![0; w * h], u: vec![128; w / 2 * h / 2], v: vec![128; w / 2 * h / 2] }
    }

    fn rect(&mut self, x0: usize, y0: usize, rw: usize, rh: usize, luma: u8, cb: u8, cr: u8) {
        for y in y0..(y0 + rh).min(self.h) {
            for x in x0..(x0 + rw).min(self.w) {
                self.y[y * self.w + x] = luma;
            }
        }
        let cs = self.w / 2;
        for y in (y0 / 2)..((y0 + rh) / 2).min(self.h / 2) {
            for x in (x0 / 2)..((x0 + rw) / 2).min(cs) {
                self.u[y * cs + x] = cb;
                self.v[y * cs + x] = cr;
            }
        }
    }

    fn digit(&mut self, d: usize, x: usize, y: usize, s: usize) {
        let seg = DIGITS[d % 10];
        let t = s / 4;
        let boxes = [
            (x + t, y, s, t),
            (x + s + t, y + t, t, s),
            (x + s + t, y + s + 2 * t, t, s),
            (x + t, y + 2 * s + 2 * t, s, t),
            (x, y + s + 2 * t, t, s),
            (x, y + t, t, s),
            (x + t, y + s + t, s, t),
        ];
        for (i, b) in boxes.iter().enumerate() {
            if seg & (1 << i) != 0 {
                self.rect(b.0, b.1, b.2, b.3, 235, 128, 128);
            }
        }
    }

    /// Hintergrund mit Verlauf und feinem Muster (das sich gut verkleinern lässt).
    fn scene(&mut self, hue: f32, t: f32) {
        let (cb, cr) = {
            let a = hue * std::f32::consts::TAU;
            ((128.0 + 60.0 * a.cos()) as u8, (128.0 + 60.0 * a.sin()) as u8)
        };
        for yy in 0..self.h {
            for xx in 0..self.w {
                let fx = xx as f32 / self.w as f32;
                let fy = yy as f32 / self.h as f32;
                let wave = ((fx * 18.0 + t).sin() + (fy * 14.0 - t * 0.7).sin()) * 12.0;
                let fine = (((xx * 7 + yy * 3) ^ (xx * yy / 64)) & 15) as f32 * 1.4;
                let base = 60.0 + 90.0 * fy + wave + fine;
                self.y[yy * self.w + xx] = base.clamp(16.0, 235.0) as u8;
            }
        }
        self.u.fill(cb);
        self.v.fill(cr);
    }
}

/// Testbild-Video: je Kapitel eine eigene Farbe und Nummer.
fn make_video(p: &EncParams, seconds: u32, chapters: u32, hue: f32, first_digit: usize) -> Vec<u8> {
    let mbw = (p.width as usize).div_ceil(16);
    let mbh = (p.height as usize).div_ceil(16);
    let fps = if p.fps_code == 3 { 25u32 } else { 30u32 };
    let gop = SAMPLE_GOP;
    let total = seconds * fps;
    let gops = total.div_ceil(gop);
    let gops_per_chapter = gops.div_ceil(chapters.max(1)).max(1);
    let mut w = BitWriter::new();
    let mut painter = Painter::new(mbw, mbh);
    for g in 0..gops {
        let frame = u64::from(g * gop);
        let chapter = g / gops_per_chapter;
        let t = frame as f32 / fps as f32;
        painter.scene(hue + chapter as f32 * 0.17, t);
        let bar = ((t * 40.0) as usize) % (painter.w.saturating_sub(120)).max(1);
        painter.rect(bar, painter.h / 2 + 90, 100, 40, 200, 90, 200);
        let num = first_digit + chapter as usize;
        if num >= 10 {
            painter.digit(num / 10, painter.w / 2 - 170, painter.h / 2 - 110, 70);
        }
        painter.digit(num, painter.w / 2 - 50, painter.h / 2 - 110, 70);

        write_seq_header(&mut w, p);
        write_gop_header(&mut w, frame, fps, true);
        let (y, u, v) = (&painter.y, &painter.u, &painter.v);
        write_intra_picture(&mut w, 0, (mbw, mbh), y, u, v, 2);
        for k in 1..gop {
            let dir = if (g / 2) % 2 == 0 { 2 } else { -2 };
            write_pan_picture(&mut w, k, (mbw, mbh), (dir, 0));
        }
    }
    w.align();
    w.out
}

/// Baut die Beispiel-DVD im Ordner `dir` und liefert den Pfad zu VIDEO_TS.
pub fn build_sample(dir: &Path, mut progress: impl FnMut(f32)) -> Res<PathBuf> {
    let video_ts = dir.join("VIDEO_TS");
    if video_ts.join("VIDEO_TS.IFO").exists() && std::fs::metadata(video_ts.join("VTS_02_1.VOB")).is_ok() {
        progress(1.0);
        return Ok(video_ts);
    }
    let wide = EncParams { width: 720, height: 576, aspect: 3, fps_code: 3, bitrate_400: 15_000 };
    let normal = EncParams { width: 720, height: 576, aspect: 2, fps_code: 3, bitrate_400: 15_000 };
    let tracks = vec![
        TrackSpec { lang: b"de", freq: 440.0 },
        TrackSpec { lang: b"en", freq: 660.0 },
    ];
    let subs: Vec<&'static [u8; 2]> = vec![b"de", b"en"];
    let spec = |es: Vec<u8>, gops_per_chapter: usize, audio: Vec<TrackSpec>, subs: Vec<&'static [u8; 2]>, wide: bool| TitleSpec {
        es,
        gops_per_chapter,
        audio,
        subs,
        fps: 25.0,
        wide,
    };

    let main_secs = 72u32;
    let chapters = 4u32;
    let main_es = make_video(&wide, main_secs, chapters, 0.58, 1);
    progress(0.45);
    let total_gops = (main_secs * 25).div_ceil(SAMPLE_GOP);
    let main = spec(main_es, total_gops.div_ceil(chapters) as usize, tracks.clone(), subs.clone(), true);
    let extra1 = spec(make_video(&wide, 14, 1, 0.1, 1), 1000, tracks[..1].to_vec(), Vec::new(), true);
    progress(0.6);
    let extra2 = spec(make_video(&normal, 18, 2, 0.32, 1), (18 * 25u32).div_ceil(SAMPLE_GOP).div_ceil(2) as usize, tracks[1..].to_vec(), subs[..1].to_vec(), false);
    progress(0.72);
    let menu1 = spec(make_video(&wide, 6, 1, 0.78, 0), 1000, Vec::new(), Vec::new(), true);
    let vmg_menu = spec(make_video(&wide, 6, 1, 0.9, 0), 1000, Vec::new(), Vec::new(), true);
    progress(0.82);

    let _ = std::fs::remove_dir_all(&video_ts);
    author_disc(
        &video_ts,
        vec![(vec![main, extra1], Some(menu1)), (vec![extra2], None)],
        Some(vmg_menu),
        true,
    )?;
    progress(1.0);
    Ok(video_ts)
}

/// Standardort der Beispiel-DVD.
pub fn sample_dir() -> PathBuf {
    std::env::temp_dir().join("DVDmanager-Beispiel-DVD")
}
