//! Schneiden beim Neu-Zusammenstellen: Auswahl der VOBUs zwischen Anfang und Ende und,
//! auf Wunsch, bildgenaue Schnitte. Dann wird nur die Bildgruppe an der Schnittstelle
//! neu kodiert; alles andere bleibt, wie es ist.

use crate::dvd::ifo::{encode_time, CellAddr};
use crate::dvd::model::{Disc, PlayCell};
use crate::dvd::source::Space;
use crate::dvd::vob::*;
use crate::dvd::{be32, put16, put32, Res, SECTOR};
use crate::engine::audiofade::{fade_audio, FadeVobu};
use crate::engine::plan::{FadeKind, ReauthorEntry, DECLICK_MS};
use crate::engine::splice::{append_tail_audio, AudioSplicer};
use crate::engine::transcode::VobuReader;
use crate::mpeg2::bits::next_start_code;
use crate::mpeg2::decode::{Decoder, FrameClock};
use crate::mpeg2::reencode::encode_frames;

const SCR_TICK: u64 = 43_886;

/// Dauer einer VOBU laut Navigationspaket (Millisekunden), ersatzweise eine halbe Sekunde.
pub fn vobu_dur_ms(data: &[u8]) -> u64 {
    let (s, e) = (be32(data, PCI_OFF + 12), be32(data, PCI_OFF + 16));
    if e > s && e - s < 90_000 * 10 {
        u64::from((e - s) / 90)
    } else {
        500
    }
}

/// Markiert die erste Bildgruppe als "abgeschnitten": Bilder, die sich auf die
/// weggelassene Gruppe davor beziehen, zeigt der Player dann nicht an.
pub fn mark_broken_link(data: &mut [u8]) {
    for s in data.chunks_exact_mut(SECTOR) {
        if classify(s) != Stream::Video {
            continue;
        }
        let h = pack_header_len(s);
        let start = h + 9 + usize::from(s[h + 8]);
        if start >= s.len() {
            return;
        }
        let payload = &s[start..];
        let mut i = 0;
        while let Some(p) = next_start_code(payload, i) {
            if p + 8 > payload.len() {
                return;
            }
            if payload[p + 3] == 0xB8 {
                let at = start + p + 7;
                if s[at] & 0x40 == 0 {
                    s[at] |= 0x20;
                }
                return;
            }
            if payload[p + 3] == 0x00 {
                return; // Erstes Bild erreicht, kein GOP-Kopf davor
            }
            i = p + 4;
        }
        return;
    }
}

pub(crate) fn video_es(data: &[u8]) -> Vec<u8> {
    let mut es = Vec::with_capacity(data.len());
    for s in data.chunks_exact(SECTOR) {
        if classify(s) == Stream::Video {
            if let Some(p) = parse_video(s) {
                es.extend_from_slice(p.payload);
            }
        }
    }
    es
}

fn pes_pts(s: &[u8]) -> Option<u64> {
    let h = pack_header_len(s);
    if h + 14 > s.len() || s[h + 7] & 0x80 == 0 {
        return None;
    }
    Some(read_ts(&s[h + 9..]))
}

/// Baut eine VOBU neu, in der nur die Bilder ab `from_ms` und vor `until_ms` (relativ
/// zum Beginn der VOBU) bleiben. Liefert `None`, wenn kein Bild übrig bleibt.
pub fn rebuild_vobu(prev: Option<&[u8]>, data: &[u8], from_ms: Option<f64>, until_ms: Option<f64>) -> Res<Option<Vec<u8>>> {
    rebuild_vobu_with(prev, data, from_ms, until_ms, None)
}

/// Blende für ein Bild: Zeit relativ zum Beginn der VOBU (ms) -> Sichtbarkeit und Zielfarbe.
pub type FadeFn<'a> = &'a dyn Fn(f64) -> Option<(f32, FadeKind)>;

/// Blendet ein dekodiertes Bild Richtung Schwarz oder Weiß (Sichtbarkeit `a`, 1 = unverändert).
pub fn apply_fade(f: &mut crate::mpeg2::decode::DecodedFrame, a: f32, kind: FadeKind) {
    let target = match kind {
        FadeKind::None => return,
        FadeKind::Black => 16.0,
        FadeKind::White => 235.0,
    };
    let a = a.clamp(0.0, 1.0);
    if a >= 0.999 {
        return;
    }
    for p in f.y.iter_mut() {
        *p = (target + (f32::from(*p) - target) * a).round().clamp(0.0, 255.0) as u8;
    }
    for p in f.u.iter_mut().chain(f.v.iter_mut()) {
        *p = (128.0 + (f32::from(*p) - 128.0) * a).round().clamp(0.0, 255.0) as u8;
    }
}

/// Bytes für die neu kodierten Bilder einer angeschnittenen Bildgruppe.
///
/// Das erste neue Bild ist immer ein I-Bild und braucht so viel wie das I-Bild der
/// Gruppe im Original; die übrigen sind Differenzbilder ohne Bewegungsausgleich und
/// bekommen das Anderthalbfache ihres Anteils. Bisher galt nur der Anteil an der Gruppe:
/// Blieb ein einziges Bild übrig, bekam dieses I-Bild ein Zwölftel und wurde blockig.
/// Obergrenze ist, was die DVD-Datenrate in dieser Zeit zulässt (plus Puffer).
fn reencode_budget(es: &[u8], kept: usize, total: usize, frame_ms: f64) -> usize {
    let mut pics = Vec::new();
    let mut i = 0;
    while let Some(p) = next_start_code(es, i) {
        if p + 3 < es.len() && es[p + 3] == 0x00 {
            pics.push(p);
        }
        i = p + 4;
    }
    let i_size = match (pics.first(), pics.get(1)) {
        (Some(a), Some(b)) => b - a,
        _ => es.len() / total.max(1),
    };
    let rest_avg = es.len().saturating_sub(i_size) as f64 / total.saturating_sub(1).max(1) as f64;
    let wanted = i_size as f64 + kept.saturating_sub(1) as f64 * rest_avg * 1.5;
    let proportional = es.len() as f64 * kept as f64 / total.max(1) as f64;
    // 9 MBit/s für die Dauer der Bilder, dazu der halbe Bildpuffer (224 KB)
    let cap = kept as f64 * frame_ms / 1000.0 * 9_000_000.0 / 8.0 + 112_000.0;
    (wanted.max(proportional).min(cap.max(proportional)) as usize).max(kept * 6_000)
}

/// Wie `rebuild_vobu`, dazu auf Wunsch eine Blende je Bild.
pub fn rebuild_vobu_with(
    prev: Option<&[u8]>,
    data: &[u8],
    from_ms: Option<f64>,
    until_ms: Option<f64>,
    fade: Option<FadeFn>,
) -> Res<Option<Vec<u8>>> {
    let es = video_es(data);
    let mut dec = Decoder::new();
    if let Some(p) = prev {
        dec.push_tagged(&video_es(p), 0);
    }
    dec.push_tagged(&es, 1);
    dec.flush();
    let mut clock = FrameClock::default();
    let mut frames = Vec::new();
    while let Some(f) = dec.pop_frame() {
        if f.tag == 1 {
            let t = clock.offset_ms(&f);
            frames.push((t, f));
        }
    }
    if frames.is_empty() {
        return Err(tr!("Die Bilder an der Schnittstelle ließen sich nicht dekodieren.", "The frames at the cut point could not be decoded.").into());
    }
    let total = frames.len();
    let frame_ms = frames[0].1.frame_ms;
    let half = frame_ms / 2.0;
    let kept: Vec<&(f64, _)> = frames
        .iter()
        .filter(|(t, _)| from_ms.is_none_or(|a| *t + half >= a) && until_ms.is_none_or(|b| *t + half < b))
        .collect();
    if kept.is_empty() {
        return Ok(None);
    }
    let seq = dec.seq.clone();
    let budget = reencode_budget(&es, kept.len(), total, frame_ms);
    // Blende: nur die betroffenen Bilder werden kopiert und verändert
    let faded: Vec<Option<crate::mpeg2::decode::DecodedFrame>> = kept
        .iter()
        .map(|(t, f)| {
            let (a, kind) = fade.and_then(|g| g(*t))?;
            let mut c = f.clone();
            apply_fade(&mut c, a, kind);
            Some(c)
        })
        .collect();
    let refs: Vec<&crate::mpeg2::decode::DecodedFrame> =
        kept.iter().zip(&faded).map(|((_, f), c)| c.as_ref().unwrap_or(f)).collect();
    let enc = encode_frames(&seq, &refs, budget);

    // Zeitstempel: Anzeige wie im Original, Dekodierung jeweils ein Bild früher.
    let s_ptm = u64::from(be32(data, PCI_OFF + 12));
    let frame90 = (frame_ms * 90.0).round() as u64;
    let pts: Vec<u64> = kept.iter().map(|(t, _)| s_ptm + (t * 90.0).round() as u64).collect();
    let first_pts = pts[0];
    let end_pts = pts[pts.len() - 1] + frame90;

    // Videopakete
    let input: Vec<&[u8]> = data.chunks_exact(SECTOR).collect();
    let nav = input[0];
    let mut video_packs: Vec<[u8; SECTOR]> = Vec::new();
    let mut marks: Vec<usize> = Vec::new(); // Ende (Byte) je Paket
    let mut emitted = 0usize;
    let mut next_pic = 0usize;
    while emitted < enc.es.len() {
        while next_pic < enc.pics.len() && enc.pics[next_pic].0 < emitted {
            next_pic += 1;
        }
        let mut ts = None;
        if let Some(&(at, _)) = enc.pics.get(next_pic) {
            if at < emitted + SECTOR - 14 - 9 - 10 || (next_pic == 0 && emitted == 0) {
                let p = pts[next_pic];
                ts = Some((p, Some(p.saturating_sub(frame90))));
            }
        }
        let (sector, take) = build_video_pack(&nav[..14], ts, &[], &enc.es[emitted..]);
        emitted += take;
        marks.push(emitted);
        video_packs.push(sector);
    }

    // Ton und Untertitel im verbleibenden Zeitraum
    let mut others: Vec<Vec<u8>> = Vec::new();
    let mut keep_state = [[from_ms.is_none(); 32]; 2];
    for s in input.iter().skip(1) {
        let (kind, idx) = match classify(s) {
            Stream::Audio(a) => (0usize, a as usize),
            Stream::Sub(a) => (1usize, a as usize),
            _ => continue,
        };
        if let Some(p) = pes_pts(s) {
            // Der Ton liegt im Datenstrom oft etwas hinter dem Bild. Eine Grenze gilt daher
            // nur dort, wo wirklich geschnitten wird.
            keep_state[kind][idx] =
                (from_ms.is_none() || p + frame90 >= first_pts) && (until_ms.is_none() || p < end_pts);
        }
        if keep_state[kind][idx] {
            others.push(s.to_vec());
        }
    }

    // Gleichmäßig verzahnen und Systemuhr im ursprünglichen Zeitfenster verteilen.
    let n_v = video_packs.len();
    let n_o = others.len();
    let total_packs = 1 + n_v + n_o;
    let orig_scr = get_scr(nav);
    let orig_last = input.iter().rev().find(|s| is_pack(s)).map(|s| get_scr(s)).unwrap_or(orig_scr);
    // Die Systemuhr rückt um den abgeschnittenen Anfang vor und endet um das abgeschnittene
    // Ende früher. Sonst passt sie nicht mehr zu den Zeitstempeln, und beim Aneinanderfügen
    // entstünde eine Lücke in Bild und Ton.
    let e_ptm = u64::from(be32(data, PCI_OFF + 16));
    let head_trim = if from_ms.is_some() { first_pts.saturating_sub(s_ptm) * 300 } else { 0 };
    let tail_trim = if until_ms.is_some() && e_ptm > end_pts { (e_ptm - end_pts) * 300 } else { 0 };
    let need = total_packs as u64 * SCR_TICK;
    let room = orig_last.saturating_sub(orig_scr);
    let (nav_scr, last_scr) = if room > need {
        let spare = room - need;
        let head = head_trim.min(spare);
        let tail = tail_trim.min(spare - head);
        (orig_scr + head, orig_last - tail)
    } else {
        (orig_scr, orig_last)
    };
    let window = last_scr.saturating_sub(nav_scr);
    let step = (window / total_packs as u64).max(SCR_TICK);
    let mut out = Vec::with_capacity(total_packs * SECTOR);
    out.extend_from_slice(nav);
    put_scr(&mut out[..SECTOR], nav_scr);
    let (mut vi, mut oi) = (0usize, 0usize);
    let mut video_index: Vec<usize> = Vec::with_capacity(n_v);
    let mut first_of_stream = [[None::<usize>; 32]; 2];
    let mut k = 1u64;
    while vi < n_v || oi < n_o {
        let take_video = oi >= n_o || (vi < n_v && (vi as f64 / n_v as f64) <= (oi as f64 / n_o.max(1) as f64));
        let at = out.len();
        if take_video {
            video_index.push(at / SECTOR);
            out.extend_from_slice(&video_packs[vi]);
            vi += 1;
        } else {
            let s = &others[oi];
            if let (kind, Some(idx)) = match classify(s) {
                Stream::Audio(a) => (0usize, Some(a as usize)),
                Stream::Sub(a) => (1usize, Some(a as usize)),
                _ => (0, None),
            } {
                first_of_stream[kind][idx].get_or_insert(at / SECTOR);
            }
            out.extend_from_slice(s);
            oi += 1;
        }
        put_scr(&mut out[at..at + SECTOR], nav_scr + k * step);
        k += 1;
    }

    // Navigationspaket anpassen
    let count = out.len() / SECTOR;
    let nav_out = &mut out[..SECTOR];
    if from_ms.is_some() {
        put32(nav_out, PCI_OFF + 12, first_pts as u32);
        // Verstrichene Zeit in der Zelle rückt um den abgeschnittenen Anfang weiter.
        let pal = nav_out[DSI_OFF + 31] >> 6 != 3;
        let elapsed = Nav::cell_elapsed_ms(nav_out) + (kept[0].0.max(0.0) as u64);
        nav_out[DSI_OFF + 28..DSI_OFF + 32].copy_from_slice(&encode_time(elapsed, pal));
    }
    if until_ms.is_some() {
        put32(nav_out, PCI_OFF + 16, end_pts as u32);
    }
    put32(nav_out, PCI_OFF + 20, 0);
    put32(nav_out, DSI_OFF + 8, (count - 1) as u32);
    for r in 0..3usize {
        let v = enc.pics.get(r).and_then(|&(_, end)| {
            let p = marks.partition_point(|m| *m < end);
            video_index.get(p).map(|i| *i as u32)
        });
        put32(nav_out, DSI_OFF + 12 + r * 4, v.unwrap_or(0));
    }
    for a in 0..8usize {
        let v = first_of_stream[0][a].map(|i| i as u16).unwrap_or(0x3FFF);
        put16(nav_out, DSI_OFF + DSI_SYNCI + a * 2, v);
    }
    for sp in 0..32usize {
        let v = first_of_stream[1][sp].map(|i| i as u32).unwrap_or(0x3FFF_FFFF);
        put32(nav_out, DSI_OFF + DSI_SYNCI + 16 + sp * 4, v);
    }
    Ok(Some(out))
}

/// Eine VOBU, die in die Ausgabe gehört.
pub struct EntryVobu<'a> {
    /// Index der Zelle im Titel
    pub title_cell: usize,
    pub pc: &'a PlayCell,
    /// Erste VOBU dieser Zelle in der Ausgabe
    pub cell_start: bool,
    /// Lage im Original (für die Verweise in den Navigationspaketen)
    pub start: u64,
    pub in_sectors: u64,
    /// Daten, bei Schnittstellen bereits neu aufgebaut
    pub data: Vec<u8>,
    /// Beginn und Dauer in der geschnittenen Fassung (Millisekunden)
    pub out_t0_ms: u64,
    pub dur_ms: u64,
}

pub enum StreamEvent<'a> {
    Vobu(EntryVobu<'a>),
    /// Gelesen, aber weggeschnitten (für Fortschritt und Abbruch)
    Skipped(u64),
}

/// Liefert alle VOBUs eines Eintrags in Abspielreihenfolge, schon geschnitten. Der Ton
/// beginnt und endet dabei mit vollständigen Rahmen (siehe `splice`).
/// `declick`: Ton am Anfang und Ende ganz kurz ein- bzw. ausblenden (`DECLICK_MS`).
pub fn entry_stream<'a>(
    disc: &'a Disc,
    entry: &ReauthorEntry,
    declick: bool,
    mut sink: impl FnMut(StreamEvent<'a>) -> Res<()>,
) -> Res<()> {
    let mut splicer = AudioSplicer::default();
    let mut held: Option<EntryVobu<'a>> = None;
    let mut after: Option<Vec<u8>> = None;
    entry_stream_raw(disc, entry, declick, &mut after, |ev| match ev {
        StreamEvent::Vobu(v) => {
            let mut v = v;
            let data = std::mem::take(&mut v.data);
            let prev = held.replace(v);
            if let Some(clean) = splicer.push(data) {
                let mut p = prev.expect("zurückgehaltene VOBU");
                p.data = clean;
                sink(StreamEvent::Vobu(p))?;
            }
            Ok(())
        }
        other => sink(other),
    })?;
    if let Some(next) = after {
        splicer.extend_last(&next);
    }
    if let (Some(mut last), Some(data)) = (held, splicer.finish()) {
        last.data = data;
        sink(StreamEvent::Vobu(last))?;
    }
    Ok(())
}

/// `after`: die erste VOBU hinter dem Eintrag, falls sie gelesen wurde.
fn entry_stream_raw<'a>(
    disc: &'a Disc,
    entry: &ReauthorEntry,
    declick: bool,
    after: &mut Option<Vec<u8>>,
    mut sink: impl FnMut(StreamEvent<'a>) -> Res<()>,
) -> Res<()> {
    let t = disc.titles.get(entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
    let v = disc.vts.get(&t.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
    let first_idx = t.chapters.get(entry.first_chapter).map(|c| c.first_cell_idx).unwrap_or(0);
    let end_idx = t.chapters.get(entry.last_chapter + 1).map(|c| c.first_cell_idx).unwrap_or(t.cells.len());
    let (cut_in, cut_out) = entry.cut.effective(crate::engine::plan::entry_duration_ms(disc, entry));
    let exact = entry.cut.exact;
    // Sichtbarer Bereich und Länge der Blenden (Zeitachse des Eintrags)
    let fades = entry.active_fade();
    let total_ms = crate::engine::plan::entry_duration_ms(disc, entry);
    let vis_start = cut_in as f64;
    let vis_end = cut_out.min(total_ms) as f64;
    let fade_len = fades.duration_ms().min(((vis_end - vis_start) / 2.0).max(1.0));
    let mut after_fade_in = false;
    // Ton im Blendenbereich: VOBUs werden gesammelt, weil Tonrahmen über VOBU-Grenzen
    // reichen und der Ton gegenüber dem Bild versetzt gemultiplext ist.
    const AUDIO_MARGIN_MS: f64 = 1_500.0;
    let mut held: Vec<(EntryVobu<'a>, f64, u32)> = Vec::new();
    let fade_audio_on = fades.is_active() && fades.audio;
    // Tatsächlicher Anfang und Ende der Ausgabe: Beim Schnitt am Schlüsselbild beginnt sie
    // vor der Schnittmarke und endet dahinter. Werden beim Durchlauf gesetzt.
    let out_start = std::cell::Cell::new(f64::NAN);
    let out_end = std::cell::Cell::new(vis_end);
    let audio_gain = |t: f64| {
        let mut g = if fade_audio_on { fades.at(t, vis_start, vis_end).map(|(a, _)| a).unwrap_or(1.0) } else { 1.0 };
        if declick {
            // Kurze Rampe an beiden Rändern: kein harter Sprung im Ton an der Schnittstelle
            let start = if out_start.get().is_nan() { vis_start } else { out_start.get() };
            let up = ((t - start) / DECLICK_MS).clamp(0.0, 1.0);
            let down = ((out_end.get() - t) / DECLICK_MS).clamp(0.0, 1.0);
            g *= up.min(down) as f32;
        }
        g
    };
    let mut cell_base = 0u64;
    let mut out_t = 0u64;
    let mut prev: Option<Vec<u8>> = None;
    let mut first_kept = true;

    'cells: for (idx, pc) in t.cells.iter().enumerate().take(end_idx).skip(first_idx) {
        let Some(src_cell) = v.pgcs.get(pc.pgc).and_then(|p| p.cells.get(pc.cell_idx)) else { continue };
        if src_cell.block_mode >= 2 {
            continue; // Weitere Kamerawinkel entfallen
        }
        if cell_base >= cut_out {
            break;
        }
        if pc.ms > 0 && cell_base + pc.ms <= cut_in {
            cell_base += pc.ms;
            prev = None;
            continue;
        }
        let mut extents: Vec<&CellAddr> =
            v.cells.iter().filter(|c| c.vob_id == pc.vob_id && c.cell_id == pc.cell_id).collect();
        extents.sort_by_key(|c| c.start);
        let mut cell_start = true;
        for ext in extents {
            let reader = disc.source.reader(Space::title(t.vts))?;
            let mut vr = VobuReader::new(reader, u64::from(ext.start), u64::from(ext.last) + 1);
            while let Some((start, data)) = vr.next()? {
                let in_sectors = (data.len() / SECTOR) as u64;
                let t0 = cell_base + Nav::cell_elapsed_ms(&data);
                let dur = vobu_dur_ms(&data);
                let t1 = t0 + dur;
                if t1 <= cut_in {
                    sink(StreamEvent::Skipped(in_sectors))?;
                    prev = Some(data);
                    continue;
                }
                if t0 >= cut_out {
                    sink(StreamEvent::Skipped(in_sectors))?;
                    // Der Ton zu den letzten Bildern liegt oft erst in dieser VOBU. Wartet die
                    // letzte VOBU noch auf die Ton-Blende, kommt er gleich mit hinein.
                    match held.last_mut() {
                        Some((v, _, _)) => append_tail_audio(&mut v.data, &data),
                        None => *after = Some(data),
                    }
                    break 'cells;
                }
                let head = exact && cut_in > t0 && cut_in < t1;
                let tail = exact && cut_out > t0 && cut_out < t1;
                // Blenden: Bildgruppen im Blendenbereich werden neu kodiert. Nach dem
                // Einblenden auch noch die nächste, damit sie sich nicht auf ein
                // abgedunkeltes Bild davor bezieht.
                let in_fade = fades.is_active()
                    && ((fades.fade_in != FadeKind::None && (t0 as f64) < vis_start + fade_len)
                        || (fades.fade_out != FadeKind::None && (t1 as f64) > vis_end - fade_len));
                let settle = !in_fade && after_fade_in;
                after_fade_in = in_fade && fades.fade_in != FadeKind::None && (t0 as f64) < vis_start + fade_len;
                let mut item = None;
                if head || tail || in_fade || settle {
                    let from = head.then(|| (cut_in - t0) as f64);
                    let until = tail.then(|| (cut_out - t0) as f64);
                    let t0f = t0 as f64;
                    let fade_fn = move |t: f64| fades.at(t0f + t, vis_start, vis_end);
                    let fade: Option<FadeFn> = if in_fade { Some(&fade_fn) } else { None };
                    match rebuild_vobu_with(prev.as_deref(), &data, from, until, fade) {
                        Ok(Some(d)) => item = Some(d),
                        Ok(None) => {
                            sink(StreamEvent::Skipped(in_sectors))?;
                            prev = Some(data);
                            continue;
                        }
                        // Lässt sich die Stelle nicht neu kodieren, wird am Schlüsselbild geschnitten.
                        Err(_) => {}
                    }
                }
                let rebuilt = item.is_some();
                if out_start.get().is_nan() {
                    out_start.set(if head && rebuilt { cut_in as f64 } else { t0 as f64 });
                }
                out_end.set(if tail && rebuilt { cut_out as f64 } else { t1 as f64 });
                let mut out = item.unwrap_or_else(|| data.clone());
                if first_kept && !rebuilt {
                    mark_broken_link(&mut out);
                }
                first_kept = false;
                let dur_out = if rebuilt { vobu_dur_ms(&out) } else { dur };
                let edge_zone = declick
                    && ((t0 as f64) < vis_start + DECLICK_MS + AUDIO_MARGIN_MS
                        || (t1 as f64) > vis_end - DECLICK_MS - AUDIO_MARGIN_MS);
                let audio_zone = edge_zone
                    || (fade_audio_on
                        && ((fades.fade_in != FadeKind::None && (t0 as f64) < vis_start + fade_len + AUDIO_MARGIN_MS)
                            || (fades.fade_out != FadeKind::None && (t1 as f64) > vis_end - fade_len - AUDIO_MARGIN_MS)));
                if !audio_zone {
                    flush_held(&mut held, &audio_gain, &mut sink)?;
                }
                let ev = EntryVobu {
                    title_cell: idx,
                    pc,
                    cell_start,
                    start,
                    in_sectors,
                    data: out,
                    out_t0_ms: out_t,
                    dur_ms: dur_out,
                };
                if audio_zone {
                    held.push((ev, t0 as f64, be32(&data, PCI_OFF + 12)));
                } else {
                    sink(StreamEvent::Vobu(ev))?;
                }
                cell_start = false;
                out_t += dur_out;
                prev = Some(data);
            }
        }
        cell_base += pc.ms;
    }
    flush_held(&mut held, &audio_gain, &mut sink)?;
    Ok(())
}

/// Blendet den Ton der gesammelten VOBUs und gibt sie weiter.
fn flush_held<'a>(
    held: &mut Vec<(EntryVobu<'a>, f64, u32)>,
    gain: &dyn Fn(f64) -> f32,
    sink: &mut impl FnMut(StreamEvent<'a>) -> Res<()>,
) -> Res<()> {
    if held.is_empty() {
        return Ok(());
    }
    {
        let mut group: Vec<FadeVobu> =
            held.iter_mut().map(|(v, t0, s_ptm)| FadeVobu { data: &mut v.data, t0: *t0, s_ptm: *s_ptm }).collect();
        fade_audio(&mut group, gain);
    }
    for (v, _, _) in held.drain(..) {
        sink(StreamEvent::Vobu(v))?;
    }
    Ok(())
}
