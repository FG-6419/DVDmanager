//! Vorschau in einem eigenen Thread: liest VOBUs, dekodiert das Bild und stellt
//! fertige Einzelbilder bereit. Ton gibt die Vorschau nicht wieder.

use std::sync::mpsc::{sync_channel, Receiver, Sender, SyncSender, TryRecvError, TrySendError};

use crate::dvd::model::Disc;
use crate::dvd::source::{Source, Space};
use crate::dvd::vob::{classify, parse_video, Stream};
use crate::dvd::SECTOR;
use crate::engine::audio::{audio_pes, AudioBus, TrackChoice, TrackDecoder};
use crate::engine::transcode::VobuReader;
use crate::dvd::vob::Nav;
use crate::mpeg2::decode::{Decoder, FrameClock};
use crate::mpeg2::requant::{Requant, VideoOpts};

/// Abschnitt zum Abspielen: Zone und Sektorbereich (einschließlich).
#[derive(Clone, Debug)]
pub struct Segment {
    pub space: Space,
    pub first: u32,
    pub last: u32,
    pub ms: u64,
    /// Anfänge der VOBUs in diesem Bereich (aus der Adresstabelle), für genaue Sprünge.
    pub vobus: Vec<u32>,
}

#[derive(Clone)]
pub struct PreviewFrame {
    pub rgba: Vec<u8>,
    pub width: usize,
    pub height: usize,
    pub aspect: f32,
    pub duration: f64,
    pub position: f32,
    pub seek_id: u64,
    /// Anzeigezeit des Bildes ab Beginn der Abschnittsliste (Millisekunden).
    pub time_ms: f64,
    /// Beginn und Ende der VOBU (Schlüsselbild-Abschnitt), zu der das Bild gehört.
    pub key_t0: f64,
    pub key_t1: f64,
    /// Dauer eines Bildes
    pub frame_ms: f64,
    /// Kein Bild, sondern die Meldung: Ende der Abschnitte erreicht.
    pub eos: bool,
}

pub enum PreviewCmd {
    /// Springt zu einem Anteil der Gesamtlänge.
    Seek(f32, u64),
    /// Springt zu einer Zeit: `exact` zeigt genau das Bild dort, sonst das Schlüsselbild davor.
    SeekTime { ms: f64, exact: bool, id: u64 },
    /// Ab dieser Zeit rückwärts abspielen (Bild für Bild, vor ms beginnend).
    Reverse { ms: f64, id: u64 },
    /// Bildqualität wie im späteren Backup vorführen
    Compression(Option<VideoOpts>),
    Stop,
}

pub struct PreviewHandle {
    pub frames: Receiver<PreviewFrame>,
    pub cmd: Sender<PreviewCmd>,
}

impl Drop for PreviewHandle {
    fn drop(&mut self) {
        let _ = self.cmd.send(PreviewCmd::Stop);
    }
}

/// Ton für die Vorschau: Ausgabe und gewählte Spur.
pub type AudioSetup = Option<(AudioBus, TrackChoice)>;

pub fn spawn(
    source: Source,
    segments: Vec<Segment>,
    start: f32,
    compression: Option<VideoOpts>,
    seek_id: u64,
    audio: AudioSetup,
) -> PreviewHandle {
    let (ftx, frames) = sync_channel(12);
    let (cmd, crx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            worker(source, segments, start, compression, seek_id, audio, ftx, crx);
        }));
    });
    PreviewHandle { frames, cmd }
}

/// Wo das Lesen beginnt und ab welcher Zeit Bilder gezeigt werden.
struct Pos {
    seg: usize,
    sector: u64,
    drop_before: Option<f64>,
}

fn locate(segments: &[Segment], fraction: f32) -> Pos {
    let total: u64 = segments.iter().map(|s| u64::from(s.last - s.first) + 1).sum();
    let mut want = (f64::from(fraction.clamp(0.0, 1.0)) * total as f64) as u64;
    for (i, s) in segments.iter().enumerate() {
        let len = u64::from(s.last - s.first) + 1;
        if want < len {
            return Pos { seg: i, sector: u64::from(s.first) + want, drop_before: None };
        }
        want -= len;
    }
    Pos {
        seg: segments.len().saturating_sub(1),
        sector: segments.last().map(|s| u64::from(s.first)).unwrap_or(0),
        drop_before: None,
    }
}

/// Sucht die VOBU, die zur Zeit `ms` gehört: (Abschnitt, Index in dessen VOBU-Liste).
/// Liest dafür nur wenige Navigationspakete (Binärsuche).
fn find_vobu(source: &Source, segments: &[Segment], seg_base: &[f64], ms: f64) -> Option<(usize, usize)> {
    let i = seg_base.iter().rposition(|b| *b <= ms).unwrap_or(0);
    let seg = segments.get(i)?;
    if seg.vobus.is_empty() {
        return None;
    }
    let base = seg_base[i];
    let mut reader = source.reader(seg.space).ok()?;
    let mut buf = Vec::new();
    let mut time_of = |k: usize| -> Option<f64> {
        reader.read(u64::from(seg.vobus[k]), 1, &mut buf).ok()?;
        if buf.len() < SECTOR || !crate::dvd::vob::is_nav(&buf[..SECTOR]) {
            return None;
        }
        Some(base + Nav::cell_elapsed_ms(&buf[..SECTOR]) as f64)
    };
    let (mut lo, mut hi) = (0usize, seg.vobus.len() - 1);
    while lo < hi {
        let mid = (lo + hi).div_ceil(2);
        match time_of(mid) {
            Some(t) if t <= ms + 0.5 => lo = mid,
            Some(_) => hi = mid - 1,
            None => return None,
        }
    }
    Some((i, lo))
}

/// Die VOBU davor, auch über die Grenze zum vorigen Abschnitt hinweg.
fn previous_vobu(segments: &[Segment], i: usize, k: usize) -> Option<(usize, u32)> {
    if k > 0 {
        return Some((i, segments[i].vobus[k - 1]));
    }
    if i > 0 {
        return segments[i - 1].vobus.last().map(|v| (i - 1, *v));
    }
    None
}

/// Sucht die Leseposition zu einer Zeit.
fn locate_time(source: &Source, segments: &[Segment], seg_base: &[f64], ms: f64, exact: bool) -> Pos {
    match find_vobu(source, segments, seg_base, ms) {
        Some((i, k)) => {
            if exact {
                // Eine VOBU früher anfangen, damit auch führende Bilder einer offenen Bildgruppe
                // stimmen; am Abschnittsanfang zählt die letzte VOBU des vorigen Abschnitts.
                let (si, start) = previous_vobu(segments, i, k).unwrap_or((i, segments[i].vobus[k]));
                Pos { seg: si, sector: u64::from(start), drop_before: Some(ms) }
            } else {
                Pos { seg: i, sector: u64::from(segments[i].vobus[k]), drop_before: None }
            }
        }
        None => {
            let i = seg_base.iter().rposition(|b| *b <= ms).unwrap_or(0);
            let seg = &segments[i];
            let len = f64::from(seg.last - seg.first + 1);
            let frac = if seg.ms > 0 { ((ms - seg_base[i]) / seg.ms as f64).clamp(0.0, 1.0) } else { 0.0 };
            Pos { seg: i, sector: u64::from(seg.first) + (frac * (len - 1.0)) as u64, drop_before: exact.then_some(ms) }
        }
    }
}

/// Dekodiert einzelne VOBUs nacheinander; liefert Bilder mit Zeit und Index der VOBU.
fn decode_vobus(source: &Source, segments: &[Segment], seg_base: &[f64], list: &[(usize, u32)], seek_id: u64) -> Vec<(PreviewFrame, usize)> {
    let mut dec = Decoder::new();
    let mut clock = FrameClock::default();
    let mut times = Vec::with_capacity(list.len());
    for (idx, (si, sector)) in list.iter().enumerate() {
        let seg = &segments[*si];
        let data = source
            .reader(seg.space)
            .ok()
            .and_then(|r| VobuReader::new(r, u64::from(*sector), u64::from(seg.last) + 1).next().ok().flatten());
        let Some((_, data)) = data else {
            times.push((0.0, 0.0));
            continue;
        };
        let t0 = seg_base[*si] + Nav::cell_elapsed_ms(&data) as f64;
        times.push((t0, t0 + crate::engine::cut::vobu_dur_ms(&data) as f64));
        let mut es = Vec::with_capacity(data.len());
        for s in data.chunks_exact(SECTOR) {
            if classify(s) == Stream::Video {
                if let Some(p) = parse_video(s) {
                    es.extend_from_slice(p.payload);
                }
            }
        }
        dec.push_tagged(&es, idx as u64);
    }
    dec.flush();
    let mut out = Vec::new();
    while let Some(f) = dec.pop_frame() {
        let idx = f.tag as usize;
        let (t0, t1) = times.get(idx).copied().unwrap_or((0.0, 0.0));
        let time_ms = t0 + clock.offset_ms(&f);
        out.push((
            PreviewFrame {
                rgba: f.to_rgba(),
                width: f.width,
                height: f.height,
                aspect: f.aspect,
                duration: f.duration,
                position: 0.0,
                seek_id,
                time_ms,
                key_t0: t0,
                key_t1: t1,
                frame_ms: f.frame_ms,
                eos: false,
            },
            idx,
        ));
    }
    out
}

enum RevEnd {
    Quit,
    Cmd(PreviewCmd),
    Done,
}

/// Rückwärts abspielen: VOBU für VOBU dekodieren und deren Bilder in umgekehrter
/// Reihenfolge ausgeben, über Abschnittsgrenzen hinweg.
fn run_reverse(
    source: &Source,
    segments: &[Segment],
    seg_base: &[f64],
    mut target: f64,
    seek_id: u64,
    ftx: &SyncSender<PreviewFrame>,
    crx: &Receiver<PreviewCmd>,
) -> RevEnd {
    for _ in 0..100_000 {
        match crx.try_recv() {
            Ok(cmd) => return RevEnd::Cmd(cmd),
            Err(TryRecvError::Disconnected) => return RevEnd::Quit,
            Err(TryRecvError::Empty) => {}
        }
        if target <= 0.5 {
            return RevEnd::Done;
        }
        let Some((i, k)) = find_vobu(source, segments, seg_base, target - 1.0) else { return RevEnd::Done };
        let mut list = Vec::with_capacity(2);
        if let Some(prev) = previous_vobu(segments, i, k) {
            list.push(prev);
        }
        list.push((i, segments[i].vobus[k]));
        let mine_idx = list.len() - 1;
        let frames = decode_vobus(source, segments, seg_base, &list, seek_id);
        let mut mine: Vec<PreviewFrame> = frames
            .into_iter()
            .filter(|(f, idx)| *idx == mine_idx && f.time_ms + f.frame_ms / 2.0 < target)
            .map(|(f, _)| f)
            .collect();
        if mine.is_empty() {
            // Diese VOBU liefert vor der Zielzeit nichts mehr: weiter mit der davor.
            let start = seg_base[i];
            let next = if k == 0 { start } else { target - 300.0 };
            if next >= target - 0.5 {
                return RevEnd::Done;
            }
            target = next;
            continue;
        }
        mine.sort_by(|a, b| b.time_ms.partial_cmp(&a.time_ms).unwrap_or(std::cmp::Ordering::Equal));
        let earliest = mine.last().map(|f| f.time_ms).unwrap_or(target);
        for f in mine {
            let mut frame = f;
            loop {
                match ftx.try_send(frame) {
                    Ok(()) => break,
                    Err(TrySendError::Full(back)) => {
                        frame = back;
                        match crx.recv_timeout(std::time::Duration::from_millis(15)) {
                            Ok(cmd) => return RevEnd::Cmd(cmd),
                            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return RevEnd::Quit,
                            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        }
                    }
                    Err(TrySendError::Disconnected(_)) => return RevEnd::Quit,
                }
            }
        }
        target = earliest;
    }
    RevEnd::Done
}
enum Next {
    Continue,
    Restart,
    Quit,
}

#[allow(clippy::too_many_arguments)]
fn worker(
    source: Source,
    segments: Vec<Segment>,
    start: f32,
    mut compression: Option<VideoOpts>,
    first_seek_id: u64,
    audio: AudioSetup,
    ftx: SyncSender<PreviewFrame>,
    crx: Receiver<PreviewCmd>,
) {
    if segments.is_empty() {
        return;
    }
    let total: u64 = segments.iter().map(|s| u64::from(s.last - s.first) + 1).sum();
    let mut dec = Decoder::new();
    let mut rq = Requant::new();
    let mut seek_id = first_seek_id;
    let mut pos = locate(&segments, start);
    // Ton: Decoder entsteht in diesem Thread (Windows-Decoder brauchen das).
    let mut track_dec = audio.as_ref().and_then(|(_, t)| TrackDecoder::new(t));
    let mut audio_seek = None;
    let mut chunks: Vec<(f64, Vec<i16>)> = Vec::new();
    let mut reverse: Option<f64> = None;
    let mut es = Vec::with_capacity(1 << 20);
    // Je VOBU eine Kennung mit Beginn und Ende, damit jedes Bild seine genaue Zeit kennt.
    let mut tag = 0u64;
    let mut starts: std::collections::VecDeque<(u64, f64, f64)> = std::collections::VecDeque::new();
    let mut clock = FrameClock::default();
    let seg_base: Vec<f64> = segments
        .iter()
        .scan(0.0f64, |acc, s| {
            let b = *acc;
            *acc += s.ms as f64;
            Some(b)
        })
        .collect();

    // Wertet einen Befehl aus; liefert, ob neu begonnen werden muss.
    let handle = |cmd: PreviewCmd, pos: &mut Pos, seek_id: &mut u64, compression: &mut Option<VideoOpts>, dec: &mut Decoder, rev: &mut Option<f64>| -> Next {
        *rev = None;
        match cmd {
            PreviewCmd::Reverse { ms, id } => {
                *seek_id = id;
                *rev = Some(ms);
            }
            PreviewCmd::Seek(f, id) => {
                *seek_id = id;
                *pos = locate(&segments, f);
            }
            PreviewCmd::SeekTime { ms, exact, id } => {
                *seek_id = id;
                *pos = locate_time(&source, &segments, &seg_base, ms, exact);
            }
            PreviewCmd::Compression(c) => {
                *compression = c;
                return Next::Continue;
            }
            PreviewCmd::Stop => return Next::Quit,
        }
        dec.reset();
        while dec.pop_frame().is_some() {}
        Next::Restart
    };

    'outer: loop {
        if audio_seek != Some(seek_id) {
            if let Some((bus, _)) = &audio {
                bus.reset(seek_id);
            }
            if let Some(d) = track_dec.as_mut() {
                d.flush();
            }
            audio_seek = Some(seek_id);
        }
        if let Some(t) = reverse.take() {
            // Rückwärts: bis zum Anfang oder bis ein neuer Befehl kommt
            let mut end = run_reverse(&source, &segments, &seg_base, t, seek_id, &ftx, &crx);
            loop {
                let cmd = match end {
                    RevEnd::Quit => return,
                    RevEnd::Cmd(c) => c,
                    RevEnd::Done => match crx.recv() {
                        Ok(c) => c,
                        Err(_) => return,
                    },
                };
                match handle(cmd, &mut pos, &mut seek_id, &mut compression, &mut dec, &mut reverse) {
                    Next::Continue => end = RevEnd::Done,
                    Next::Restart => continue 'outer,
                    Next::Quit => return,
                }
            }
        }
        let seg = &segments[pos.seg];
        let Ok(reader) = source.reader(seg.space) else { return };
        let mut vr = VobuReader::new(reader, pos.sector, u64::from(seg.last) + 1);
        let before: u64 = segments[..pos.seg].iter().map(|s| u64::from(s.last - s.first) + 1).sum();

        loop {
            // Befehle zwischen den VOBUs
            match crx.try_recv() {
                Ok(cmd) => match handle(cmd, &mut pos, &mut seek_id, &mut compression, &mut dec, &mut reverse) {
                    Next::Continue => {}
                    Next::Restart => continue 'outer,
                    Next::Quit => return,
                },
                Err(TryRecvError::Disconnected) => return,
                Err(TryRecvError::Empty) => {}
            }
            let Ok(item) = vr.next() else { return };
            let Some((start_sec, data)) = item else {
                break;
            };
            tag += 1;
            let t0 = seg_base[pos.seg] + Nav::cell_elapsed_ms(&data) as f64;
            starts.push_back((tag, t0, t0 + crate::engine::cut::vobu_dur_ms(&data) as f64));
            while starts.len() > 64 {
                starts.pop_front();
            }
            // Ton dieser VOBU: Zeitstempel relativ zum Beginn der VOBU umrechnen
            if let (Some((bus, track)), Some(dec_a)) = (&audio, track_dec.as_mut()) {
                let s_ptm = i64::from(crate::dvd::be32(&data, crate::dvd::vob::PCI_OFF + 12));
                for s in data.chunks_exact(SECTOR) {
                    if let Some(pes) = audio_pes(s, track) {
                        let time = pes.pts.map(|p| {
                            let mut d = p as i64 - s_ptm;
                            if d > 1 << 32 {
                                d -= 1 << 33;
                            } else if d < -(1 << 32) {
                                d += 1 << 33;
                            }
                            t0 + d as f64 / 90.0
                        });
                        dec_a.feed(&pes, time, &mut chunks);
                    }
                }
                for (t, pcm) in chunks.drain(..) {
                    bus.push(seek_id, t, pcm);
                }
            }
            es.clear();
            for s in data.chunks_exact(SECTOR) {
                if classify(s) == Stream::Video {
                    if let Some(p) = parse_video(s) {
                        es.extend_from_slice(p.payload);
                    }
                }
            }
            match compression {
                Some(opts) if !opts.is_passthrough() => {
                    rq.set_opts(opts);
                    let out = rq.process(&es);
                    dec.push_tagged(&out.es, tag);
                }
                _ => dec.push_tagged(&es, tag),
            }

            let done = before + start_sec.saturating_sub(u64::from(seg.first));
            let position = (done as f64 / total as f64) as f32;
            while let Some(f) = dec.pop_frame() {
                let (key_t0, key_t1) = starts.iter().find(|s| s.0 == f.tag).map(|s| (s.1, s.2)).unwrap_or((0.0, 0.0));
                let time_ms = key_t0 + clock.offset_ms(&f);
                // Nach einem bildgenauen Sprung erst ab dem gesuchten Bild zeigen.
                if let Some(d) = pos.drop_before {
                    if time_ms + f.frame_ms / 2.0 < d {
                        continue;
                    }
                    pos.drop_before = None;
                }
                let mut frame = PreviewFrame {
                    rgba: f.to_rgba(),
                    width: f.width,
                    height: f.height,
                    aspect: f.aspect,
                    duration: f.duration,
                    position,
                    seek_id,
                    time_ms,
                    key_t0,
                    key_t1,
                    frame_ms: f.frame_ms,
                    eos: false,
                };
                loop {
                    match ftx.try_send(frame) {
                        Ok(()) => break,
                        Err(TrySendError::Full(back)) => {
                            frame = back;
                            match crx.recv_timeout(std::time::Duration::from_millis(15)) {
                                Ok(cmd) => match handle(cmd, &mut pos, &mut seek_id, &mut compression, &mut dec, &mut reverse) {
                                    Next::Continue => {}
                                    Next::Restart => continue 'outer,
                                    Next::Quit => return,
                                },
                                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                            }
                        }
                        Err(TrySendError::Disconnected(_)) => return,
                    }
                }
            }
        }
        // Nächster Abschnitt
        if pos.seg + 1 < segments.len() {
            let next = pos.seg + 1;
            pos = Pos { seg: next, sector: u64::from(segments[next].first), drop_before: pos.drop_before };
        } else {
            // Ende: restliche Bilder ausgeben, dann auf einen Sprung warten.
            dec.flush();
            while let Some(f) = dec.pop_frame() {
                let (key_t0, key_t1) = starts.iter().find(|s| s.0 == f.tag).map(|s| (s.1, s.2)).unwrap_or((0.0, 0.0));
                let time_ms = key_t0 + clock.offset_ms(&f);
                if pos.drop_before.is_some_and(|d| time_ms + f.frame_ms / 2.0 < d) {
                    continue;
                }
                pos.drop_before = None;
                let frame = PreviewFrame {
                    rgba: f.to_rgba(),
                    width: f.width,
                    height: f.height,
                    aspect: f.aspect,
                    duration: f.duration,
                    position: 1.0,
                    seek_id,
                    time_ms,
                    key_t0,
                    key_t1,
                    frame_ms: f.frame_ms,
                    eos: false,
                };
                if ftx.send(frame).is_err() {
                    return;
                }
            }
            // Ende melden, damit die Wiedergabe weiterschalten oder anhalten kann.
            let end = PreviewFrame {
                rgba: Vec::new(),
                width: 0,
                height: 0,
                aspect: 0.0,
                duration: 0.0,
                position: 1.0,
                seek_id,
                time_ms: f64::INFINITY,
                key_t0: f64::INFINITY,
                key_t1: f64::INFINITY,
                frame_ms: 40.0,
                eos: true,
            };
            if ftx.send(end).is_err() {
                return;
            }
            loop {
                match crx.recv() {
                    Ok(cmd) => match handle(cmd, &mut pos, &mut seek_id, &mut compression, &mut dec, &mut reverse) {
                        Next::Continue => {}
                        Next::Restart => continue 'outer,
                        Next::Quit => return,
                    },
                    Err(_) => return,
                }
            }
        }
    }
}

/// VOBU-Anfänge einer Adresstabelle im Bereich `first..=last`.
fn vobus_in(admap: &[u32], first: u32, last: u32) -> Vec<u32> {
    let a = admap.partition_point(|x| *x < first);
    let b = admap.partition_point(|x| *x <= last);
    admap[a..b.max(a)].to_vec()
}
/// Abschnitte eines Titels in Abspielreihenfolge.
pub fn title_segments(disc: &Disc, title: usize, first_chapter: usize, last_chapter: usize) -> Vec<Segment> {
    let Some(t) = disc.titles.get(title) else { return Vec::new() };
    let first = t.chapters.get(first_chapter).map(|c| c.first_cell_idx).unwrap_or(0);
    let end = t.chapters.get(last_chapter + 1).map(|c| c.first_cell_idx).unwrap_or(t.cells.len());
    let Some(v) = disc.vts.get(&t.vts) else { return Vec::new() };
    t.cells
        .iter()
        .take(end)
        .skip(first)
        .filter(|c| v.pgcs.get(c.pgc).and_then(|p| p.cells.get(c.cell_idx)).map(|x| x.block_mode < 2).unwrap_or(true))
        .filter(|c| c.last_sector >= c.first_sector)
        .map(|c| Segment {
            space: Space::title(t.vts),
            first: c.first_sector,
            last: c.last_sector,
            ms: c.ms,
            vobus: vobus_in(&v.admap, c.first_sector, c.last_sector),
        })
        .collect()
}

/// Ganzer Menübereich als ein Abschnitt.
pub fn menu_segments(disc: &Disc, vts: u8) -> Vec<Segment> {
    let Ok(r) = disc.source.reader(Space::menu(vts)) else { return Vec::new() };
    let total = r.sectors();
    if total == 0 {
        return Vec::new();
    }
    let admap = if vts == 0 { &disc.vmg.menu_admap } else { disc.vts.get(&vts).map(|v| &v.menu_admap).unwrap_or(&disc.vmg.menu_admap) };
    let admap = if vts == 0 || disc.vts.contains_key(&vts) { admap.clone() } else { Vec::new() };
    vec![Segment { space: Space::menu(vts), first: 0, last: total as u32 - 1, ms: 0, vobus: admap }]
}
