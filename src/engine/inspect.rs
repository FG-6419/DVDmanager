//! Liest die fertige Ausgabe noch einmal in einem Durchgang:
//!
//! - Prüfung: Jede Tonspur wird Rahmen für Rahmen verfolgt. Bytes, die zu keinem
//!   vollständigen Rahmen gehören (wären als Knacken zu hören), und Sprünge in den
//!   Zeitstempeln des Tons werden mit ihrer Stelle im Film gemeldet.
//! - Übersichtsbild: In festem Zeitabstand wird eine Bildgruppe dekodiert und ein
//!   verkleinertes Bild daraus in ein Raster gesetzt, mit Zeitangabe, als JPG gespeichert.
//!
//! Die Zeit im Film ergibt sich aus der Zahl der Bilder; das funktioniert auch dort, wo die
//! Zeitstempel springen (DVD-Ordner, an jeder Zelle eine neue Uhr).

use std::collections::VecDeque;
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::dvd::vob::{pack_header_len, read_ts};
use crate::dvd::{Res, SECTOR};
use crate::engine::audio::{frame_len, Codec};
use crate::engine::splice::{frame_ticks, header_key};
use crate::mpeg2::decode::Decoder;

/// Ergebnis der Prüfung.
#[derive(Clone, Debug, Default)]
pub struct CheckReport {
    /// Stellen (Sekunden) mit beschädigten oder angeschnittenen Tonrahmen
    pub audio_breaks: Vec<f64>,
    /// Stellen (Sekunden), an denen die Zeitstempel des Tons springen
    pub time_jumps: Vec<f64>,
    /// Geprüfte Länge (Sekunden) und Zahl der Tonspuren
    pub seconds: f64,
    pub tracks: usize,
    /// Stellen mit AC3-Rahmen, deren Prüfsumme nicht stimmt (verfälschter Ton)
    pub crc_errors: Vec<f64>,
    /// Stellen, an denen Ton bzw. Bild erst nach seiner Abspielzeit beim Player ankommt
    /// (Player stocken dort)
    pub late_audio: Vec<f64>,
    pub late_video: Vec<f64>,
    /// Stellen, an denen die Systemuhr rückwärts läuft (nur bei fortlaufenden Dateien)
    pub scr_back: Vec<f64>,
}

impl CheckReport {
    /// Fehler, die beim Export selbst entstehen können: zerrissene Tonrahmen, Zeitsprünge.
    pub fn is_clean(&self) -> bool {
        self.audio_breaks.is_empty() && self.time_jumps.is_empty()
    }

    /// Auch alles, was schon in einer Quelle stecken kann (für „Datei prüfen“).
    pub fn is_clean_full(&self) -> bool {
        self.is_clean() && self.crc_errors.is_empty() && self.late_audio.is_empty() && self.late_video.is_empty() && self.scr_back.is_empty()
    }
}

/// Merkt eine Stelle, aber höchstens eine je Sekunde (sonst würden Tausende gemeldet).
fn note(list: &mut Vec<f64>, now: f64) {
    if list.last().is_none_or(|l| now - l >= 1.0) {
        list.push(now);
    }
}

/// Zeitverhalten des Datenstroms: Systemuhr und Ankunft der Pakete.
#[derive(Default)]
struct Timing {
    last_scr: Option<i64>,
    late_audio: Vec<f64>,
    late_video: Vec<f64>,
    scr_back: Vec<f64>,
    /// LPCM-Spuren (ohne Rahmen, daher nicht Rahmen für Rahmen geprüft, aber mitgezählt)
    lpcm: std::collections::BTreeSet<u8>,
}

/// Was beim Lesen geschehen soll.
pub struct InspectOpts {
    pub verify: bool,
    /// Zeitstempel sind fortlaufend (MPEG- und VOB-Datei); nur dann zählen Sprünge.
    pub continuous: bool,
    /// Übersichtsbild: Abstand in Sekunden und Zieldatei
    pub sheet: Option<(u32, PathBuf)>,
}

/// Breite eines Vorschaubildes und Zahl der Spalten im Übersichtsbild
const THUMB_W: usize = 320;
const COLUMNS: usize = 4;
const GAP: usize = 8;

/// Liest `groups` (je Gruppe Dateien, die zusammen einen Strom bilden, z. B. VTS_01_1..9.VOB).
/// `progress` erhält den Anteil 0..1 und liefert false zum Abbrechen.
pub fn inspect(
    groups: &[Vec<PathBuf>],
    opts: &InspectOpts,
    progress: &mut dyn FnMut(f32) -> bool,
) -> Res<(Option<CheckReport>, Option<PathBuf>)> {
    let total: u64 = groups.iter().flatten().filter_map(|p| std::fs::metadata(p).ok()).map(|m| m.len()).sum::<u64>().max(1);
    let mut done = 0u64;
    let mut video = VideoScan::new(opts.sheet.as_ref().map(|s| f64::from(s.0.max(1))));
    let mut report = CheckReport::default();
    let mut buf = vec![0u8; SECTOR * 512];
    let mut timing = Timing::default();
    for group in groups {
        let mut audio: Vec<AudioCheck> = Vec::new();
        timing.last_scr = None;
        for path in group {
            let mut f = std::fs::File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
            loop {
                let n = read_full(&mut f, &mut buf).map_err(|e| format!("{}: {e}", path.display()))?;
                if n == 0 {
                    break;
                }
                for s in buf[..n - n % SECTOR].chunks_exact(SECTOR) {
                    sector(s, &mut video, &mut audio, opts.verify.then_some(&mut timing));
                }
                done += n as u64;
                if !progress(done as f32 / total as f32) {
                    return Err(tr!("Abgebrochen", "Cancelled").into());
                }
            }
        }
        for mut a in audio {
            a.process(video.now(), true);
            report.audio_breaks.extend(a.breaks);
            report.crc_errors.extend(a.crc_errors);
            if opts.continuous {
                report.time_jumps.extend(a.jumps);
            }
            report.tracks += 1;
        }
        video.end_group();
    }
    report.seconds = video.now();
    report.tracks += timing.lpcm.len();
    report.late_audio = timing.late_audio;
    report.late_video = timing.late_video;
    if opts.continuous {
        report.scr_back = timing.scr_back;
    }
    for l in [&mut report.audio_breaks, &mut report.time_jumps, &mut report.crc_errors] {
        l.sort_by(f64::total_cmp);
        l.dedup_by(|b, a| *b - *a < 1.0);
    }
    let sheet = match &opts.sheet {
        Some((_, path)) if !video.thumbs.is_empty() => {
            write_sheet(&video.thumbs, path)?;
            Some(path.clone())
        }
        _ => None,
    };
    Ok((opts.verify.then_some(report), sheet))
}

fn read_full(f: &mut std::fs::File, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match f.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Systemuhr eines Pakets (MPEG-2-Kopf) in 90 kHz.
fn scr90(s: &[u8]) -> Option<i64> {
    if s[4] & 0xC0 != 0x40 {
        return None;
    }
    let b = &s[4..10];
    Some((i64::from(b[0] >> 3 & 7) << 30) | (i64::from(b[0] & 3) << 28) | (i64::from(b[1]) << 20) | (i64::from(b[2] >> 3) << 15) | (i64::from(b[2] & 3) << 13) | (i64::from(b[3]) << 5) | i64::from(b[4] >> 3))
}

fn sector(s: &[u8], video: &mut VideoScan, audio: &mut Vec<AudioCheck>, mut timing: Option<&mut Timing>) {
    if s[..4] != [0, 0, 1, 0xBA] {
        return;
    }
    let verify = timing.is_some();
    let scr = scr90(s);
    if let (Some(t), Some(c)) = (timing.as_deref_mut(), scr) {
        if t.last_scr.is_some_and(|l| c < l) {
            note(&mut t.scr_back, video.now());
        }
        t.last_scr = Some(c);
    }
    let mut p = pack_header_len(s);
    while p + 9 <= SECTOR && s[p] == 0 && s[p + 1] == 0 && s[p + 2] == 1 {
        let id = s[p + 3];
        let end = p + 6 + usize::from(u16::from_be_bytes([s[p + 4], s[p + 5]]));
        if end > SECTOR {
            return;
        }
        if matches!(id, 0xBB | 0xBE | 0xBF) {
            p = end;
            continue;
        }
        let payload = (p + 9 + usize::from(s[p + 8])).min(end);
        let pts = (s[p + 7] & 0x80 != 0 && p + 14 <= end).then(|| read_ts(&s[p + 9..]) as i64);
        // Kommt das Paket vor seiner Zeit an? Bild: Dekodierzeit (DTS), sonst Abspielzeit.
        if let (Some(t), Some(c), Some(pts)) = (timing.as_deref_mut(), scr, pts) {
            let is_audio = (0xC0..=0xC7).contains(&id) || (id == 0xBD && payload < end && (0x80..=0xAF).contains(&s[payload]));
            let when = if id == 0xE0 && s[p + 7] & 0xC0 == 0xC0 && p + 19 <= end { read_ts(&s[p + 14..]) as i64 } else { pts };
            // Über den Überlauf der 33-Bit-Uhr hinweg nicht melden
            if id == 0xBD && payload < end && (0xA0..=0xA7).contains(&s[payload]) {
                t.lpcm.insert(s[payload]);
            }
            let lead = when - c;
            if lead < 0 && lead > -(1 << 31) {
                if id == 0xE0 {
                    note(&mut t.late_video, video.now());
                } else if is_audio {
                    note(&mut t.late_audio, video.now());
                }
            }
        }
        match id {
            0xE0 => video.feed(&s[payload..end]),
            0xC0..=0xC7 if verify => feed_audio(audio, id, Codec::Mpeg, &s[payload..end], pts, video.now()),
            0xBD if verify && payload + 4 <= end && (0x80..=0x87).contains(&s[payload]) => {
                feed_audio(audio, s[payload], Codec::Ac3, &s[payload + 4..end], pts, video.now())
            }
            _ => {}
        }
        p = end;
    }
}

fn feed_audio(list: &mut Vec<AudioCheck>, key: u8, codec: Codec, data: &[u8], pts: Option<i64>, now: f64) {
    let i = match list.iter().position(|a| a.key == key) {
        Some(i) => i,
        None => {
            list.push(AudioCheck::new(key, codec));
            list.len() - 1
        }
    };
    list[i].feed(data, pts, now);
}

// ---------------------------------------------------------------------------
// Ton
// ---------------------------------------------------------------------------

struct AudioCheck {
    key: u8,
    codec: Codec,
    buf: Vec<u8>,
    /// Lage von buf[0] im Tonstrom
    base: u64,
    /// Zeitstempel der Pakete: (Lage im Strom, PTS)
    pes: VecDeque<(u64, i64)>,
    /// Lage des zuletzt gelesenen Rahmens
    last_frame: Option<u64>,
    frames: u64,
    anchor: Option<(u64, i64)>,
    in_garbage: bool,
    breaks: Vec<f64>,
    jumps: Vec<f64>,
    crc_errors: Vec<f64>,
}

/// AC3-Prüfsumme (CRC-16, Polynom 0x8005) über einen Rahmen ohne Sync-Wort: 0, wenn heil.
fn ac3_crc(frame: &[u8]) -> u16 {
    let mut c: u16 = 0;
    for &b in frame.get(2..).unwrap_or(&[]) {
        c ^= u16::from(b) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
        }
    }
    c
}

impl AudioCheck {
    fn new(key: u8, codec: Codec) -> Self {
        Self {
            key,
            codec,
            buf: Vec::new(),
            base: 0,
            pes: VecDeque::new(),
            last_frame: None,
            frames: 0,
            anchor: None,
            in_garbage: false,
            breaks: Vec::new(),
            jumps: Vec::new(),
            crc_errors: Vec::new(),
        }
    }

    fn feed(&mut self, data: &[u8], pts: Option<i64>, now: f64) {
        if let Some(t) = pts {
            self.pes.push_back((self.base + self.buf.len() as u64, t));
        }
        self.buf.extend_from_slice(data);
        if self.buf.len() > 16_384 {
            self.process(now, false);
        }
    }

    fn garbage(&mut self, now: f64) {
        // Vor dem ersten vollständigen Rahmen: Die Datei beginnt mitten in einem Rahmen; das
        // überspringt jeder Decoder unhörbar.
        if self.frames == 0 {
            return;
        }
        if !self.in_garbage {
            self.breaks.push(now);
            self.in_garbage = true;
        }
    }

    fn process(&mut self, now: f64, eof: bool) {
        let mut pos = 0usize;
        while pos < self.buf.len() {
            if pos + 6 > self.buf.len() && !eof {
                break;
            }
            let Some(l) = frame_len(&self.buf[pos..], self.codec).filter(|l| *l > 4) else {
                self.garbage(now);
                pos += 1;
                continue;
            };
            let e = pos + l;
            let ok = if e + 6 <= self.buf.len() {
                frame_len(&self.buf[e..], self.codec).is_some()
                    && header_key(&self.buf[pos..], self.codec) == header_key(&self.buf[e..], self.codec)
            } else if !eof {
                break; // Auf mehr Daten warten
            } else {
                e == self.buf.len()
            };
            if ok {
                if self.codec == Codec::Ac3 && ac3_crc(&self.buf[pos..e]) != 0 {
                    note(&mut self.crc_errors, now);
                }
                self.frame(pos, now);
                self.in_garbage = false;
                pos = e;
            } else {
                self.garbage(now);
                pos += 1;
            }
        }
        self.buf.drain(..pos.min(self.buf.len()));
        self.base += pos as u64;
    }

    /// Ein vollständiger Rahmen an `pos` (in buf): Zeitstempel prüfen.
    fn frame(&mut self, pos: usize, now: f64) {
        let off = self.base + pos as u64;
        // Der Zeitstempel eines Pakets gilt für den ersten Rahmen, der darin beginnt.
        let mut pts = None;
        while let Some(&(at, t)) = self.pes.front() {
            if at > off {
                break;
            }
            self.pes.pop_front();
            if self.last_frame.is_none_or(|lf| at > lf) {
                pts = Some(t);
            }
        }
        let idx = self.frames;
        self.frames += 1;
        self.last_frame = Some(off);
        let (Some(t), Some(dur)) = (pts, frame_ticks(&self.buf[pos..], self.codec)) else { return };
        if let Some((ai, at)) = self.anchor {
            let expect = at + (idx - ai) as i64 * dur;
            // Bis zu drei Rahmen (mindestens 0,1 s) gelten als Übergang, nicht als Sprung.
            if (t - expect).abs() > (3 * dur).max(9_000) {
                self.jumps.push(now);
            }
        }
        self.anchor = Some((idx, t));
    }
}

// ---------------------------------------------------------------------------
// Bild: Zeit zählen und Vorschaubilder holen
// ---------------------------------------------------------------------------

struct Thumb {
    rgb: Vec<u8>,
    w: usize,
    h: usize,
    secs: f64,
}

struct VideoScan {
    tail: Vec<u8>,
    pictures: u64,
    fields: u64,
    frame_ms: f64,
    /// Dauer der Gruppen davor (Zeit läuft über Titelsets weiter)
    base_ms: f64,
    interval: Option<f64>,
    next_at: f64,
    collect: Option<Vec<u8>>,
    collect_at: f64,
    seq_seen: u32,
    thumbs: Vec<Thumb>,
}

impl VideoScan {
    fn new(interval: Option<f64>) -> Self {
        Self {
            tail: Vec::new(),
            pictures: 0,
            fields: 0,
            frame_ms: 40.0,
            base_ms: 0.0,
            interval,
            next_at: interval.map(|i| i / 2.0).unwrap_or(f64::MAX),
            collect: None,
            collect_at: 0.0,
            seq_seen: 0,
            thumbs: Vec::new(),
        }
    }

    /// Dauer der bisherigen Bilder dieser Gruppe (ms). MPEG-2 zählt Halbbilder: Ein Bild aus
    /// zwei Halbbildern kann als zwei Bilder kodiert sein, und bei NTSC-Filmen wird oft ein
    /// Halbbild wiederholt.
    fn group_ms(&self) -> f64 {
        if self.fields > 0 {
            self.fields as f64 * self.frame_ms / 2.0
        } else {
            self.pictures as f64 * self.frame_ms
        }
    }

    /// Sekunden ab Beginn der Ausgabe
    fn now(&self) -> f64 {
        (self.base_ms + self.group_ms()) / 1000.0
    }

    fn end_group(&mut self) {
        self.finish_collect();
        self.base_ms += self.group_ms();
        self.pictures = 0;
        self.fields = 0;
        self.tail.clear();
    }

    fn feed(&mut self, data: &[u8]) {
        if let Some(c) = self.collect.as_mut() {
            c.extend_from_slice(data);
            if c.len() > 3_000_000 {
                self.finish_collect();
            }
        }
        let mut buf = std::mem::take(&mut self.tail);
        buf.extend_from_slice(data);
        let mut i = 0;
        while i + 3 < buf.len() {
            if buf[i] != 0 || buf[i + 1] != 0 || buf[i + 2] != 1 {
                i += 1;
                continue;
            }
            match buf[i + 3] {
                0x00 => self.pictures += 1,
                // Bildkodierungs-Erweiterung: Bild- oder Halbbildstruktur, Halbbild-Wiederholung
                0xB5 if i + 8 <= buf.len() && buf[i + 4] >> 4 == 8 => {
                    let structure = buf[i + 6] & 3;
                    let repeat = u64::from((buf[i + 7] >> 1) & 1);
                    self.fields += if structure == 3 { 2 + repeat } else { 1 };
                }
                0xB3 => {
                    if i + 8 <= buf.len() {
                        self.frame_ms = match buf[i + 7] & 0x0F {
                            1 => 1001.0 / 24.0,
                            2 => 1000.0 / 24.0,
                            4 => 1001.0 / 30.0,
                            5 => 1000.0 / 30.0,
                            _ => 40.0,
                        };
                    }
                    if self.collect.is_some() {
                        self.seq_seen += 1;
                        if self.seq_seen >= 3 {
                            self.finish_collect();
                        }
                    } else if self.now() >= self.next_at {
                        // Bildgruppe ab diesem Sequenzkopf sammeln (zwei Gruppen für sichere Bezüge)
                        self.collect = Some(buf[i..].to_vec());
                        self.collect_at = self.now();
                        self.seq_seen = 1;
                        while self.next_at <= self.now() {
                            self.next_at += self.interval.unwrap_or(f64::MAX);
                        }
                    }
                }
                _ => {}
            }
            i += 4;
        }
        let keep = buf.len().min(3);
        self.tail = buf[buf.len() - keep..].to_vec();
    }

    fn finish_collect(&mut self) {
        let Some(es) = self.collect.take() else { return };
        let mut dec = Decoder::new();
        dec.push(&es);
        dec.flush();
        let mut frames = Vec::new();
        while let Some(f) = dec.pop_frame() {
            frames.push(f);
        }
        // Ein Bild aus der Mitte: dort sind alle Bezugsbilder vorhanden.
        let Some(f) = frames.get(frames.len() / 2) else { return };
        let rgba = f.to_rgba();
        let (w, h) = (f.width, f.height);
        let tw = THUMB_W;
        let th = ((tw as f32 / f.aspect.max(0.5)).round() as usize).max(8);
        let mut rgb = Vec::with_capacity(tw * th * 3);
        for y in 0..th {
            let y0 = y * h / th;
            let y1 = ((y + 1) * h / th).max(y0 + 1);
            for x in 0..tw {
                let x0 = x * w / tw;
                let x1 = ((x + 1) * w / tw).max(x0 + 1);
                let mut acc = [0u32; 3];
                let mut n = 0u32;
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let k = (yy * w + xx) * 4;
                        acc[0] += u32::from(rgba[k]);
                        acc[1] += u32::from(rgba[k + 1]);
                        acc[2] += u32::from(rgba[k + 2]);
                        n += 1;
                    }
                }
                let n = n.max(1);
                rgb.extend_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8]);
            }
        }
        self.thumbs.push(Thumb { rgb, w: tw, h: th, secs: self.collect_at });
    }
}

// ---------------------------------------------------------------------------
// Übersichtsbild
// ---------------------------------------------------------------------------

/// Ziffern und Doppelpunkt, 3 x 5 Punkte
const GLYPHS: [(char, [u8; 5]); 11] = [
    ('0', [0b111, 0b101, 0b101, 0b101, 0b111]),
    ('1', [0b010, 0b110, 0b010, 0b010, 0b111]),
    ('2', [0b111, 0b001, 0b111, 0b100, 0b111]),
    ('3', [0b111, 0b001, 0b111, 0b001, 0b111]),
    ('4', [0b101, 0b101, 0b111, 0b001, 0b001]),
    ('5', [0b111, 0b100, 0b111, 0b001, 0b111]),
    ('6', [0b111, 0b100, 0b111, 0b101, 0b111]),
    ('7', [0b111, 0b001, 0b010, 0b010, 0b010]),
    ('8', [0b111, 0b101, 0b111, 0b101, 0b111]),
    ('9', [0b111, 0b101, 0b111, 0b001, 0b111]),
    (':', [0b000, 0b010, 0b000, 0b010, 0b000]),
];

fn clock(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}

/// Zeichnet Text mit Punktgröße `px` an (x, y), weiß auf dunklem Kasten.
fn draw_text(img: &mut [u8], width: usize, x: usize, y: usize, text: &str, px: usize) {
    let tw = text.chars().count() * 4 * px + px;
    let th = 7 * px;
    let height = img.len() / 3 / width;
    for yy in y..(y + th).min(height) {
        for xx in x..(x + tw).min(width) {
            let k = (yy * width + xx) * 3;
            for c in &mut img[k..k + 3] {
                *c = (u16::from(*c) * 2 / 5) as u8;
            }
        }
    }
    for (ci, ch) in text.chars().enumerate() {
        let Some((_, rows)) = GLYPHS.iter().find(|g| g.0 == ch) else { continue };
        for (ry, bits) in rows.iter().enumerate() {
            for rx in 0..3 {
                if bits & (0b100 >> rx) == 0 {
                    continue;
                }
                for dy in 0..px {
                    for dx in 0..px {
                        let xx = x + px + ci * 4 * px + rx * px + dx;
                        let yy = y + px + ry * px + dy;
                        if xx < width && yy < height {
                            let k = (yy * width + xx) * 3;
                            img[k..k + 3].copy_from_slice(&[240, 240, 240]);
                        }
                    }
                }
            }
        }
    }
}

fn write_sheet(thumbs: &[Thumb], path: &Path) -> Res<()> {
    let cell_w = thumbs.iter().map(|t| t.w).max().unwrap_or(THUMB_W);
    let cell_h = thumbs.iter().map(|t| t.h).max().unwrap_or(180);
    let cols = COLUMNS.min(thumbs.len()).max(1);
    let rows = thumbs.len().div_ceil(cols);
    let width = GAP + cols * (cell_w + GAP);
    let height = GAP + rows * (cell_h + GAP);
    let mut img = vec![24u8; width * height * 3];
    for (i, t) in thumbs.iter().enumerate() {
        let (cx, cy) = (GAP + (i % cols) * (cell_w + GAP), GAP + (i / cols) * (cell_h + GAP));
        for y in 0..t.h {
            let dst = ((cy + y) * width + cx) * 3;
            img[dst..dst + t.w * 3].copy_from_slice(&t.rgb[y * t.w * 3..(y + 1) * t.w * 3]);
        }
        let label = clock(t.secs);
        draw_text(&mut img, width, cx + 4, cy + t.h.saturating_sub(7 * 2 + 4), &label, 2);
    }
    use image::ImageEncoder;
    let file = crate::dvd::create_output(path)?;
    let mut w = std::io::BufWriter::new(file);
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut w, 85);
    enc.write_image(&img, width as u32, height as u32, image::ExtendedColorType::Rgb8)
        .map_err(|e| format!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mpa(fill: u8) -> Vec<u8> {
        let mut f = vec![fill; 576];
        f[..4].copy_from_slice(&[0xFF, 0xFD, 0xA4, 0x00]);
        f
    }

    #[test]
    fn audio_check_finds_cut_frame() {
        let mut a = AudioCheck::new(0xC0, Codec::Mpeg);
        let mut es: Vec<u8> = (1..=4u8).flat_map(mpa).collect();
        es.truncate(576 * 3 + 100); // angeschnittener vierter Rahmen
        es.extend((5..=7u8).flat_map(mpa));
        a.feed(&es, Some(90_000), 1.0);
        a.process(2.0, true);
        assert_eq!(a.breaks.len(), 1);
        assert_eq!(a.frames, 6);

        let mut clean = AudioCheck::new(0xC0, Codec::Mpeg);
        let es: Vec<u8> = (1..=6u8).flat_map(mpa).collect();
        clean.feed(&es[..1000], Some(90_000), 1.0);
        clean.feed(&es[1000..], None, 1.0);
        clean.process(2.0, true);
        assert!(clean.breaks.is_empty());
        assert_eq!(clean.frames, 6);
    }

    #[test]
    fn audio_check_finds_time_jump() {
        let mut a = AudioCheck::new(0xC0, Codec::Mpeg);
        // 24 ms je Rahmen; zweites Paket kommt 0,5 s zu spät
        let f: Vec<u8> = (1..=2u8).flat_map(mpa).collect();
        a.feed(&f, Some(90_000), 1.0);
        a.feed(&f, Some(90_000 + 2 * 2160 + 45_000), 1.5);
        a.process(2.0, true);
        assert!(a.breaks.is_empty());
        assert_eq!(a.jumps.len(), 1);
    }

    #[test]
    fn sheet_is_written() {
        let thumbs: Vec<Thumb> = (0..5).map(|i| Thumb { rgb: vec![(i * 40) as u8; 32 * 18 * 3], w: 32, h: 18, secs: f64::from(i) * 61.0 }).collect();
        let p = std::env::temp_dir().join(format!("dvdmanager-sheet-{}.jpg", std::process::id()));
        write_sheet(&thumbs, &p).unwrap();
        let img = image::open(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        assert_eq!(img.width() as usize, GAP + 4 * (32 + GAP));
        assert_eq!(img.height() as usize, GAP + 2 * (18 + GAP));
    }
}
