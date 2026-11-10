//! Szenenwechsel finden: Die Schlüsselbilder aller Bildgruppen werden nach Farbe und
//! Helligkeit verglichen. Ein großer Sprung, der deutlich über der Bewegung in der
//! Umgebung liegt, gilt als Szenenwechsel. Auf Wunsch wird danach in den beiden
//! betroffenen Bildgruppen das genaue Bild des Wechsels gesucht.
//!
//! Die Analyse liefert alle Kandidaten mit ihrer Stärke; welche davon zählen, hängt von
//! Empfindlichkeit und Mindestlänge ab und lässt sich ohne neue Analyse ändern.

use std::sync::atomic::{AtomicBool, Ordering};

use crate::dvd::ifo::CellAddr;
use crate::dvd::model::Disc;
use crate::dvd::source::Space;
use crate::dvd::vob::Nav;
use crate::dvd::Res;
use crate::engine::cut::{video_es, vobu_dur_ms};
use crate::engine::plan::{entry_duration_ms, Cut, ReauthorEntry};
use crate::engine::transcode::VobuReader;
use crate::mpeg2::bits::next_start_code;
use crate::mpeg2::decode::{DecodedFrame, Decoder, FrameClock};

const GRID_W: usize = 8;
const GRID_H: usize = 6;
const BINS: usize = 16;

/// Fingerabdruck eines Bildes: Mittelwerte von Y, U, V in einem groben Raster und
/// die Helligkeitsverteilung.
#[derive(Clone)]
pub struct Signature {
    grid: [f32; GRID_W * GRID_H * 3],
    hist: [f32; BINS],
}

impl Signature {
    pub fn of(f: &DecodedFrame) -> Self {
        let mut grid = [0f32; GRID_W * GRID_H * 3];
        let mut count = [0u32; GRID_W * GRID_H];
        let mut hist = [0f32; BINS];
        let (w, h) = (f.width.max(2), f.height.max(2));
        let cs = f.stride / 2;
        let mut n = 0u32;
        for y in (0..h).step_by(4) {
            let gy = y * GRID_H / h;
            for x in (0..w).step_by(4) {
                let gx = x * GRID_W / w;
                let g = gy * GRID_W + gx;
                let luma = f.y[y * f.stride + x];
                let ci = (y / 2) * cs + x / 2;
                grid[g * 3] += f32::from(luma);
                grid[g * 3 + 1] += f32::from(f.u[ci]);
                grid[g * 3 + 2] += f32::from(f.v[ci]);
                count[g] += 1;
                hist[usize::from(luma) * BINS / 256] += 1.0;
                n += 1;
            }
        }
        for (g, c) in count.iter().enumerate() {
            let c = (*c).max(1) as f32 * 255.0;
            for k in 0..3 {
                grid[g * 3 + k] /= c;
            }
        }
        for v in hist.iter_mut() {
            *v /= n.max(1) as f32;
        }
        Self { grid, hist }
    }

    /// Unterschied zweier Bilder, 0 (gleich) bis 1.
    pub fn distance(&self, o: &Signature) -> f32 {
        let grid = self.grid.iter().zip(&o.grid).map(|(a, b)| (a - b).abs()).sum::<f32>() / self.grid.len() as f32;
        let hist = self.hist.iter().zip(&o.hist).map(|(a, b)| (a - b).abs()).sum::<f32>() / 2.0;
        (grid * 4.0).max(hist).min(1.0)
    }
}

/// Ein möglicher Szenenwechsel.
#[derive(Clone, Debug)]
pub struct Candidate {
    /// Beginn der Bildgruppe mit dem neuen Bild (Zeitachse des Eintrags)
    pub at_ms: u64,
    /// Genaues erstes Bild der neuen Szene, wenn ermittelt
    pub exact_ms: Option<u64>,
    /// Stärke des Sprungs (0..1)
    pub score: f32,
    /// Übliche Veränderung in der Umgebung
    pub base: f32,
}

/// Ergebnis der Analyse eines Eintrags.
#[derive(Clone, Debug, Default)]
pub struct SceneScan {
    pub candidates: Vec<Candidate>,
    /// Untersuchter Bereich (Zeitachse des Eintrags)
    pub start_ms: u64,
    pub end_ms: u64,
}

/// Schwelle für einen Kandidaten bei Empfindlichkeit 1 (alles darunter zählt nie).
const MIN_SCORE: f32 = 0.06;

fn threshold(sensitivity: f32) -> (f32, f32) {
    let s = sensitivity.clamp(0.0, 1.0);
    // Absolute Schwelle und Abstand zur üblichen Bewegung
    (0.45 - 0.39 * s, 3.5 - 2.0 * s)
}

impl SceneScan {
    /// Szenenanfänge (ohne den Anfang des Bereichs) für diese Einstellungen.
    pub fn cuts(&self, sensitivity: f32, min_len_ms: u64, exact: bool) -> Vec<u64> {
        let (thr, ratio) = threshold(sensitivity);
        let mut found: Vec<(u64, f32)> = self
            .candidates
            .iter()
            .filter(|c| c.score >= thr && c.score >= c.base * ratio + 0.01)
            .map(|c| (if exact { c.exact_ms.unwrap_or(c.at_ms) } else { c.at_ms }, c.score))
            .collect();
        // Stärkste zuerst; zu nahe an einem schon gewählten Wechsel oder am Rand entfällt
        found.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut kept: Vec<u64> = Vec::new();
        for (t, _) in found {
            let near_edge = t < self.start_ms + min_len_ms || t + min_len_ms > self.end_ms;
            if near_edge || kept.iter().any(|k| k.abs_diff(t) < min_len_ms) {
                continue;
            }
            kept.push(t);
        }
        kept.sort_unstable();
        kept
    }
}

/// Teilt einen Eintrag an den Szenenanfängen `cuts` in einzelne Einträge.
pub fn split_entry(disc: &Disc, entry: &ReauthorEntry, cuts: &[u64], exact: bool) -> Vec<ReauthorEntry> {
    let total = entry_duration_ms(disc, entry);
    let (a, b) = entry.cut.effective(total);
    let b = b.min(total);
    let mut bounds = vec![a];
    bounds.extend(cuts.iter().copied().filter(|t| *t > a && *t < b));
    bounds.push(b);
    bounds
        .windows(2)
        .map(|w| {
            let mut e = ReauthorEntry::new(entry.title, entry.first_chapter, entry.last_chapter, entry.settings.clone());
            e.cut = Cut { start_ms: Some(w[0]), end_ms: Some(w[1]), exact };
            e
        })
        .collect()
}

/// Erstes Bild einer VOBU dekodieren (das Schlüsselbild am Anfang der Bildgruppe).
fn key_frame(data: &[u8]) -> Option<DecodedFrame> {
    let es = video_es(data);
    // Nur bis zum zweiten Bild: Das Schlüsselbild braucht keine anderen Bilder.
    let mut pics = 0;
    let mut cut = es.len();
    let mut i = 0;
    while let Some(p) = next_start_code(&es, i) {
        if p + 3 < es.len() && es[p + 3] == 0x00 {
            pics += 1;
            if pics == 2 {
                cut = p;
                break;
            }
        }
        i = p + 4;
    }
    let mut dec = Decoder::new();
    dec.push(&es[..cut]);
    dec.flush();
    dec.pop_frame()
}

/// Sucht in zwei aufeinanderfolgenden VOBUs das erste Bild der neuen Szene.
/// `t_prev` und `t_cur` sind ihre Anfangszeiten.
fn exact_change(prev: &[u8], cur: &[u8], t_prev: u64, t_cur: u64) -> Option<u64> {
    let mut dec = Decoder::new();
    dec.push_tagged(&video_es(prev), 0);
    dec.push_tagged(&video_es(cur), 1);
    dec.flush();
    let mut clock = FrameClock::default();
    let mut frames: Vec<(f64, Signature)> = Vec::new();
    while let Some(f) = dec.pop_frame() {
        let base = if f.tag == 0 { t_prev } else { t_cur } as f64;
        let t = base + clock.offset_ms(&f);
        frames.push((t, Signature::of(&f)));
    }
    frames.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(f32, f64)> = None;
    for w in frames.windows(2) {
        let d = w[0].1.distance(&w[1].1);
        if best.is_none_or(|b| d > b.0) {
            best = Some((d, w[1].0));
        }
    }
    best.filter(|b| b.0 >= MIN_SCORE).map(|b| b.1.round().max(0.0) as u64)
}

/// Analysiert den gewählten Bereich eines Eintrags (innerhalb seiner Schnittmarken).
/// `progress` erhält den Anteil 0..1.
pub fn scan(disc: &Disc, entry: &ReauthorEntry, cancel: &AtomicBool, progress: &mut dyn FnMut(f32)) -> Res<SceneScan> {
    let t = disc.titles.get(entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
    let v = disc.vts.get(&t.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
    let first_idx = t.chapters.get(entry.first_chapter).map(|c| c.first_cell_idx).unwrap_or(0);
    let end_idx = t.chapters.get(entry.last_chapter + 1).map(|c| c.first_cell_idx).unwrap_or(t.cells.len());
    let total = entry_duration_ms(disc, entry);
    let (cut_in, cut_out) = entry.cut.effective(total);
    let cut_out = cut_out.min(total);
    let span = cut_out.saturating_sub(cut_in).max(1) as f32;

    // Fingerabdrücke aller Schlüsselbilder, dazu die Rohdaten der vorigen VOBU
    let mut keys: Vec<(u64, f32)> = Vec::new(); // (Beginn, Abstand zum vorigen Schlüsselbild)
    let mut prev_sig: Option<Signature> = None;
    let mut prev: Option<(u64, Vec<u8>)> = None;
    let mut exact: Vec<Option<u64>> = Vec::new();
    let mut cell_base = 0u64;

    'cells: for pc in t.cells.iter().take(end_idx).skip(first_idx) {
        let Some(src_cell) = v.pgcs.get(pc.pgc).and_then(|p| p.cells.get(pc.cell_idx)) else { continue };
        if src_cell.block_mode >= 2 {
            continue;
        }
        if cell_base >= cut_out {
            break;
        }
        if pc.ms > 0 && cell_base + pc.ms <= cut_in {
            cell_base += pc.ms;
            continue;
        }
        let mut extents: Vec<&CellAddr> = v.cells.iter().filter(|c| c.vob_id == pc.vob_id && c.cell_id == pc.cell_id).collect();
        extents.sort_by_key(|c| c.start);
        for ext in extents {
            let reader = disc.source.reader(Space::title(t.vts))?;
            let mut vr = VobuReader::new(reader, u64::from(ext.start), u64::from(ext.last) + 1);
            while let Some((_, data)) = vr.next()? {
                if cancel.load(Ordering::Relaxed) {
                    return Err(tr!("Abgebrochen", "Cancelled").into());
                }
                let t0 = cell_base + Nav::cell_elapsed_ms(&data);
                if t0 + vobu_dur_ms(&data) <= cut_in {
                    continue;
                }
                if t0 >= cut_out {
                    break 'cells;
                }
                let Some(frame) = key_frame(&data) else { continue };
                let sig = Signature::of(&frame);
                let d = prev_sig.as_ref().map(|p| p.distance(&sig)).unwrap_or(0.0);
                // Genaues Bild nur für deutliche Sprünge suchen
                let ex = match &prev {
                    Some((tp, pd)) if d >= MIN_SCORE => exact_change(pd, &data, *tp, t0),
                    _ => None,
                };
                keys.push((t0, d));
                exact.push(ex);
                prev_sig = Some(sig);
                prev = Some((t0, data));
                progress((t0.saturating_sub(cut_in)) as f32 / span);
            }
        }
        cell_base += pc.ms;
    }

    // Übliche Veränderung je Stelle: Mittelwert der Nachbarn ohne die Stelle selbst
    let mut candidates = Vec::new();
    for (i, &(t0, d)) in keys.iter().enumerate().skip(1) {
        if d < MIN_SCORE {
            continue;
        }
        let lo = i.saturating_sub(6).max(1);
        let hi = (i + 7).min(keys.len());
        // Median ist robust gegen einen zweiten Wechsel in der Nähe
        let mut sorted: Vec<f32> = (lo..hi).filter(|k| *k != i).map(|k| keys[k].1).collect();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let base = sorted.get(sorted.len() / 2).copied().unwrap_or(0.0);
        candidates.push(Candidate { at_ms: t0, exact_ms: exact[i], score: d, base });
    }
    progress(1.0);
    Ok(SceneScan { candidates, start_ms: cut_in, end_ms: cut_out })
}
