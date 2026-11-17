//! Öffnet einzelne VOB- oder MPEG-Dateien (MPEG-2-Programmstrom) wie eine DVD.
//!
//! Die Datei wird einmal gelesen und ohne Neukodierung in einen DVD-Ordner umgepackt:
//! Bild und Ton werden getrennt, je Bildgruppe entsteht ein DVD-Block (VOBU) mit
//! Navigationspaket, dazu IFO-Dateien mit einem Titel. Damit funktionieren Vorschau,
//! Schnitt, Szenenerkennung und alle Ausgaben wie bei einer DVD.
//!
//! Der Ordner liegt im Temp-Verzeichnis und wird beim nächsten Öffnen derselben,
//! unveränderten Datei wiederverwendet. Ältere Importe werden dabei entfernt.
//! Die Zeitstempel werden fortlaufend gemacht (Sprünge in der Quelle verschwinden).
//! Untertitel und DTS-Ton werden nicht übernommen.

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::demo::{make_tmap, nav_pack, pack_header};
use crate::dvd::ifo::{encode_time, CellAddr, TitleEntry};
use crate::dvd::source::Source;
use crate::dvd::vob::*;
use crate::dvd::{put16, put32, Res, SECTOR};
use crate::engine::audio::{frame_len, Codec};
use crate::engine::author::{self, ACell, APgc, AVmg, AVts};
use crate::engine::splice::header_key;

/// Beginn der neuen Zeitachse (0,5 s), wie bei der MPEG-Ausgabe.
const START_PTS: i64 = 45_000;
/// Systemuhr-Schritt je Sektor bei 10,08 MBit/s (27 MHz)
const SCR_TICK: u64 = 43_886;
/// Ab dieser Abweichung (0,7 s) gilt ein Zeitstempel als Sprung in der Quelle.
const JUMP: i64 = 63_000;

/// Dateien, die als Einzeldatei geöffnet werden. Eine VOB-Datei in einem DVD-Ordner
/// öffnet weiterhin die ganze DVD.
pub fn is_media_path(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    let ext = p.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
    match ext.as_str() {
        "mpg" | "mpeg" | "m2p" | "mpv" => true,
        "vob" => !p.parent().is_some_and(is_dvd_folder),
        _ => false,
    }
}

fn is_dvd_folder(dir: &Path) -> bool {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten().any(|e| {
                let n = e.file_name().to_string_lossy().to_ascii_uppercase();
                n == "VIDEO_TS.IFO" || (n.starts_with("VTS_") && n.ends_with(".IFO"))
            })
        })
        .unwrap_or(false)
}

/// Öffnet DVD, Ordner, ISO oder Einzeldatei. Eine Einzeldatei wird dafür umgepackt;
/// `progress` erhält Anteil und Text und liefert false zum Abbrechen.
pub fn open_source(path: &Path, progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<Source> {
    open_sources(std::slice::from_ref(&path.to_path_buf()), progress)
}

/// Wie `open_source`, aber auch für mehrere VOB-/MPEG-Dateien: Sie werden zusammen zu
/// einer DVD umgepackt, jede Datei als eigener Titel, benannt nach dem Dateinamen.
pub fn open_sources(paths: &[PathBuf], progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<Source> {
    let Some(first) = paths.first() else { return Err(tr!("Keine Datei gewählt.", "No file chosen.").into()) };
    if paths.len() == 1 && !is_media_path(first) {
        let mut s = Source::open(first)?;
        s.lock();
        return Ok(s);
    }
    if paths.len() > 99 {
        return Err(tr!("Höchstens 99 Dateien lassen sich zusammen öffnen.", "At most 99 files can be opened together.").into());
    }
    if let Some(bad) = paths.iter().find(|p| !is_media_path(p)) {
        return Err(trf!(
            "„{}“ ist keine einzelne VOB- oder MPEG-Datei. Mehrere Dateien lassen sich nur als VOB-/MPEG-Dateien zusammen öffnen.",
            "\"{}\" is not a single VOB or MPEG file. Several files can only be opened together as VOB/MPEG files.",
            bad.display()
        ));
    }
    let dir = import_cached(paths, progress)?;
    let mut s = Source::open(&dir)?;
    let stem = |p: &PathBuf| p.file_stem().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "Video".into());
    s.title_names = paths.iter().map(stem).collect();
    s.label = stem(first);
    s.origin = first.clone();
    s.origins = paths.to_vec();
    // Die umgepackte Kopie liegt im Zwischenspeicher; gesperrt werden die Dateien des Benutzers.
    s.locks = std::sync::Arc::new(paths.iter().filter_map(|p| crate::dvd::source::lock_file(p)).collect());
    Ok(s)
}

fn cache_root() -> PathBuf {
    std::env::temp_dir().join("DVDmanager-Import")
}

/// Kennung der Dateien: Pfade, Größen, Änderungszeiten, in dieser Reihenfolge.
fn cache_key(paths: &[PathBuf]) -> Res<u64> {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for path in paths {
        let meta = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?;
        path.to_string_lossy().to_lowercase().hash(&mut h);
        meta.len().hash(&mut h);
        meta.modified().ok().and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok()).map(|d| d.as_secs()).hash(&mut h);
    }
    Ok(h.finish())
}

fn import_cached(paths: &[PathBuf], progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<PathBuf> {
    let base = cache_root().join(format!("{:016x}", cache_key(paths)?));
    let video_ts = base.join("VIDEO_TS");
    if base.join("fertig").is_file() && video_ts.join("VIDEO_TS.IFO").is_file() {
        return Ok(video_ts);
    }
    // Ältere Importe belegen nur Platz
    if let Ok(rd) = std::fs::read_dir(cache_root()) {
        for e in rd.flatten() {
            if e.path() != base {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(&video_ts).map_err(|e| format!("{}: {e}", video_ts.display()))?;
    match import_many(paths, &video_ts, progress) {
        Ok(()) => {
            let _ = std::fs::write(base.join("fertig"), b"1");
            Ok(video_ts)
        }
        Err(e) => {
            let _ = std::fs::remove_dir_all(&base);
            Err(e)
        }
    }
}

/// Packt `src` in einen VIDEO_TS-Ordner `out` um.
pub fn import(src: &Path, out: &Path, progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<()> {
    import_many(&[src.to_path_buf()], out, progress)
}

/// Packt mehrere Dateien in einen VIDEO_TS-Ordner um: je Datei ein Titelset mit einem Titel.
pub fn import_many(srcs: &[PathBuf], out: &Path, progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<()> {
    let sizes: Vec<u64> = srcs.iter().map(|p| std::fs::metadata(p).map(|m| m.len().max(1)).map_err(|e| format!("{}: {e}", p.display()))).collect::<Res<_>>()?;
    let total: u64 = sizes.iter().sum();
    let mut before = 0u64;
    let mut sets = Vec::with_capacity(srcs.len());
    for (k, src) in srcs.iter().enumerate() {
        let n = srcs.len();
        let text = if n > 1 {
            trf!("Datei {} von {} wird vorbereitet …", "Preparing file {} of {} …", k + 1, n)
        } else {
            tr!("Datei wird für die Bearbeitung vorbereitet …", "Preparing the file for editing …").to_string()
        };
        let (b, t) = (before as f32, total as f32);
        let mut sub = |f: f32, _: &str| progress(0.92 * (b + f * sizes[k] as f32) / t, &text);
        let set = import_one(src, out, (k + 1) as u8, &mut sub).map_err(|e| if n > 1 { format!("{}: {e}", src.display()) } else { e })?;
        sets.push(set);
        before += sizes[k];
    }
    write_vmg(out, &sets)
}

/// Packt eine Datei als Titelset `vts` um (Fortschritt 0..1).
fn import_one(src: &Path, out: &Path, vts: u8, progress: &mut dyn FnMut(f32, &str) -> bool) -> Res<(Vec<u8>, u32, bool)> {
    let total = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?.len().max(1);
    let mut f = File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let text = "";
    let mut imp = Importer::new(&out.join(format!("VTS_{vts:02}_1.VOB")))?;
    let mut buf: Vec<u8> = Vec::with_capacity(8 << 20);
    let mut chunk = vec![0u8; 4 << 20];
    let mut done = 0u64;
    let mut first = true;
    loop {
        let n = f.read(&mut chunk).map_err(|e| format!("{}: {e}", src.display()))?;
        if n == 0 {
            break;
        }
        if first {
            check_container(&chunk[..n])?;
            first = false;
        }
        crate::engine::live::offer(&chunk[..n]);
        buf.extend_from_slice(&chunk[..n]);
        done += n as u64;
        let used = imp.parse(&buf, false)?;
        buf.drain(..used);
        if !progress(0.97 * done as f32 / total as f32, text) {
            return Err(tr!("Abgebrochen", "Cancelled").into());
        }
    }
    imp.parse(&buf, true)?;
    progress(1.0, text);
    imp.finish(out, vts)
}

/// Erkennt Dateien, die kein MPEG-2-Programmstrom sind.
fn check_container(head: &[u8]) -> Res<()> {
    if head.len() > 376 && head[0] == 0x47 && head[188] == 0x47 && head[376] == 0x47 {
        return Err(tr!(
            "Das ist ein MPEG-Transportstrom (z. B. eine TV-Aufnahme). Geöffnet werden können MPEG-2-Programmströme wie VOB- oder MPG-Dateien von DVDs.",
            "This is an MPEG transport stream (e.g. a TV recording). MPEG-2 program streams such as VOB or MPG files from DVDs can be opened."
        )
        .into());
    }
    let has_pack = head.windows(4).take(64 * 1024).any(|w| w == [0, 0, 1, 0xBA]);
    if !has_pack {
        return Err(tr!(
            "In der Datei wurde kein MPEG-Programmstrom gefunden. Geöffnet werden können VOB- und MPG-Dateien mit MPEG-2-Bild.",
            "No MPEG program stream was found in the file. VOB and MPG files with MPEG-2 video can be opened."
        )
        .into());
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Bild
// ---------------------------------------------------------------------------

struct Pic {
    /// Lage im Elementarstrom (absolut)
    at: u64,
    tr: u16,
    kind: u8,
    fields: u32,
    pts: Option<i64>,
    dts: Option<i64>,
}

/// Fertige Bildgruppe: Daten ab Sequenzkopf, Bilder mit Lage relativ zu `data`.
pub(crate) struct Gop {
    pub(crate) data: Vec<u8>,
    pub(crate) pics: Vec<(usize, u16, u8, u32, Option<i64>, Option<i64>)>,
}

#[derive(Default)]
pub(crate) struct VideoParser {
    es: Vec<u8>,
    base: u64,
    scan: usize,
    /// Zeitstempel der Pakete: (Lage der Nutzdaten, PTS, DTS)
    anchors: VecDeque<(u64, i64, Option<i64>)>,
    gop_start: Option<usize>,
    gop_has_seq: bool,
    pics: Vec<Pic>,
    last_pic: Option<u64>,
    /// Laufender Sequenzkopf (Anfang) und die zuletzt vollständige Kopie samt Erweiterungen
    seq_at: Option<usize>,
    pub(crate) last_seq: Vec<u8>,
    pub(crate) width: u32,
    pub(crate) height: u32,
    pub(crate) aspect: u8,
    pub(crate) rate: u8,
    pub(crate) mpeg2: bool,
    seen_seq: bool,
}

impl VideoParser {
    pub(crate) fn push(&mut self, payload: &[u8], pts: Option<i64>, dts: Option<i64>) {
        if let Some(p) = pts {
            self.anchors.push_back((self.base + self.es.len() as u64, p, dts));
        }
        self.es.extend_from_slice(payload);
    }

    /// Sucht Bildgruppen; fertige werden zurückgegeben.
    pub(crate) fn take_gops(&mut self, eof: bool) -> Vec<Gop> {
        let mut out = Vec::new();
        let limit = if eof { self.es.len() } else { self.es.len().saturating_sub(16) };
        let mut i = self.scan;
        while i + 4 <= limit.max(i) && i + 3 < self.es.len() {
            if self.es[i] != 0 || self.es[i + 1] != 0 || self.es[i + 2] != 1 {
                i += 1;
                continue;
            }
            let code = self.es[i + 3];
            let h: [u8; 8] = std::array::from_fn(|k| self.es.get(i + k).copied().unwrap_or(0));
            match code {
                0xB3 => {
                    if !self.pics.is_empty() {
                        if let Some(g) = self.finish_gop(i) {
                            out.push(g);
                        }
                    }
                    self.width = (u32::from(h[4]) << 4) | (u32::from(h[5]) >> 4);
                    self.height = ((u32::from(h[5]) & 0x0F) << 8) | u32::from(h[6]);
                    self.aspect = h[7] >> 4;
                    self.rate = h[7] & 0x0F;
                    self.seen_seq = true;
                    self.seq_at = Some(i);
                    if self.gop_start.is_none() {
                        self.gop_start = Some(i);
                        self.gop_has_seq = true;
                    }
                }
                0xB8 | 0x00 => {
                    if let Some(s) = self.seq_at.take() {
                        self.last_seq = self.es[s..i].to_vec();
                    }
                    if code == 0xB8 && !self.pics.is_empty() {
                        if let Some(g) = self.finish_gop(i) {
                            out.push(g);
                        }
                    }
                    if self.gop_start.is_none() {
                        self.gop_start = Some(i);
                        self.gop_has_seq = false;
                    }
                    if code == 0x00 {
                        let at = self.base + i as u64;
                        // Der Zeitstempel eines Pakets gilt für das erste Bild, das darin beginnt.
                        let mut ts = None;
                        while let Some(&(off, p, d)) = self.anchors.front() {
                            if off > at {
                                break;
                            }
                            self.anchors.pop_front();
                            if self.last_pic.is_none_or(|lp| off > lp) {
                                ts = Some((p, d));
                            }
                        }
                        self.last_pic = Some(at);
                        self.pics.push(Pic {
                            at,
                            tr: (u16::from(h[4]) << 2) | (u16::from(h[5]) >> 6),
                            kind: (h[5] >> 3) & 7,
                            fields: 2,
                            pts: ts.map(|t| t.0),
                            dts: ts.and_then(|t| t.1),
                        });
                    }
                }
                0xB5 => match h[4] >> 4 {
                    1 => self.mpeg2 = true,
                    8 => {
                        if let Some(p) = self.pics.last_mut() {
                            let structure = h[6] & 3;
                            let repeat = u32::from((h[7] >> 1) & 1);
                            p.fields = if structure == 3 { 2 + repeat } else { 1 };
                        }
                    }
                    _ => {}
                },
                0xB7 => {
                    // Sequenzende: die Gruppe davor ist fertig, das Endezeichen entfällt
                    if !self.pics.is_empty() {
                        if let Some(g) = self.finish_gop(i) {
                            out.push(g);
                        }
                    }
                    self.es.drain(i..i + 4);
                    continue;
                }
                _ => {}
            }
            i += 4;
        }
        self.scan = i;
        if eof && !self.pics.is_empty() {
            let end = self.es.len();
            if let Some(g) = self.finish_gop(end) {
                out.push(g);
            }
        }
        // Verbrauchtes vorne abschneiden
        let keep_from = self.gop_start.unwrap_or(self.scan.min(self.es.len()));
        let keep_from = self.seq_at.map_or(keep_from, |s| s.min(keep_from));
        if keep_from > 0 {
            self.es.drain(..keep_from);
            self.base += keep_from as u64;
            // Am Dateiende kann die letzte Gruppe hinter der Suchstelle enden
            self.scan = self.scan.saturating_sub(keep_from);
            self.gop_start = self.gop_start.map(|g| g - keep_from);
            self.seq_at = self.seq_at.map(|s| s - keep_from);
        }
        out
    }

    /// Schließt die laufende Bildgruppe vor `end` ab; die nächste beginnt dort.
    fn finish_gop(&mut self, end: usize) -> Option<Gop> {
        let start = self.gop_start.take()?;
        let pics = std::mem::take(&mut self.pics);
        let has_seq = self.gop_has_seq;
        self.gop_start = Some(end);
        self.gop_has_seq = false;
        // Vor dem ersten Sequenzkopf lässt sich nichts dekodieren.
        if !self.seen_seq || (!has_seq && self.last_seq.is_empty()) {
            return None;
        }
        let mut data = Vec::with_capacity(end - start + self.last_seq.len());
        if !has_seq {
            data.extend_from_slice(&self.last_seq);
        }
        let shift = data.len() as i64 - (self.base + start as u64) as i64;
        data.extend_from_slice(&self.es[start..end]);
        let pics = pics.into_iter().map(|p| ((p.at as i64 + shift) as usize, p.tr, p.kind, p.fields, p.pts, p.dts)).collect();
        Some(Gop { data, pics })
    }

    pub(crate) fn frame_ticks(&self) -> f64 {
        match self.rate {
            1 => 3753.75,
            2 => 3750.0,
            4 => 3003.0,
            5 => 3000.0,
            6 => 1800.0,
            7 => 1501.5,
            8 => 1500.0,
            _ => 3600.0,
        }
    }
}

// ---------------------------------------------------------------------------
// Ton
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum AKind {
    Mpeg,
    Ac3,
    /// Bits, Abtastrate, Kanäle und die drei Kopfbytes der Quelle
    Lpcm { bits: u8, rate: u32, ch: u8, hdr: [u8; 3] },
}

pub(crate) struct AudioStream {
    pub(crate) src: u8,
    pub(crate) kind: AKind,
    phys: u8,
    es: Vec<u8>,
    base: u64,
    anchors: VecDeque<(u64, i64)>,
    pos: usize,
    last_frame: Option<u64>,
    pub(crate) last_src: Option<i64>,
    pub(crate) dur: i64,
    /// Fertige Rahmen: Daten und Zeit in der Quelle
    pub(crate) frames: VecDeque<(Vec<u8>, i64)>,
    off: Option<i64>,
    last_out: Option<i64>,
    pub(crate) channels: u8,
    pub(crate) rate: u32,
    /// LPCM: Lage des ersten Rahmens bekannt
    aligned: bool,
}

impl AudioStream {
    pub(crate) fn new(src: u8, kind: AKind, phys: u8) -> Self {
        Self {
            src,
            kind,
            phys,
            es: Vec::new(),
            base: 0,
            anchors: VecDeque::new(),
            pos: 0,
            last_frame: None,
            last_src: None,
            dur: 2160,
            frames: VecDeque::new(),
            off: None,
            last_out: None,
            channels: 2,
            rate: 48_000,
            aligned: !matches!(kind, AKind::Lpcm { .. }),
        }
    }

    fn codec(&self) -> Codec {
        match self.kind {
            AKind::Mpeg => Codec::Mpeg,
            AKind::Ac3 => Codec::Ac3,
            AKind::Lpcm { .. } => Codec::Lpcm,
        }
    }

    /// LPCM: Rahmen von 1/600 s; Bytes je Rahmen
    fn lpcm_frame(&self) -> usize {
        let AKind::Lpcm { bits, rate, ch, .. } = self.kind else { return 0 };
        let samples = (rate / 600) as usize;
        let ch = usize::from(ch.max(1));
        match bits {
            16 => samples * 2 * ch,
            20 => samples / 2 * 5 * ch,
            _ => samples / 2 * 6 * ch,
        }
    }

    /// `first`: LPCM-Zeiger auf den ersten Rahmen im Paket (Abstand ab Nutzdaten)
    pub(crate) fn push(&mut self, payload: &[u8], pts: Option<i64>, first: Option<usize>) {
        let at = self.base + self.es.len() as u64;
        if !self.aligned {
            // LPCM: alles vor dem ersten bekannten Rahmenanfang entfällt
            let Some(skip) = first.filter(|f| *f < payload.len()) else { return };
            self.aligned = true;
            if let Some(p) = pts {
                self.anchors.push_back((at, p));
            }
            self.es.extend_from_slice(&payload[skip..]);
            return;
        }
        if let Some(p) = pts {
            let off = at + first.unwrap_or(0) as u64;
            self.anchors.push_back((off, p));
        }
        self.es.extend_from_slice(payload);
    }

    /// Zerlegt den Strom in vollständige Rahmen.
    pub(crate) fn parse(&mut self, eof: bool) {
        let codec = self.codec();
        let lpcm = self.lpcm_frame();
        loop {
            let pos = self.pos;
            let rest = self.es.len() - pos;
            let len = if lpcm > 0 {
                if rest < lpcm {
                    break;
                }
                lpcm
            } else {
                if rest < 6 {
                    break;
                }
                let Some(l) = frame_len(&self.es[pos..], codec).filter(|l| *l > 4) else {
                    self.pos += 1;
                    continue;
                };
                if rest < l + 6 && !eof {
                    break;
                }
                let ok = if rest >= l + 6 {
                    frame_len(&self.es[pos + l..], codec).is_some() && header_key(&self.es[pos..], codec) == header_key(&self.es[pos + l..], codec)
                } else {
                    rest == l
                };
                if !ok {
                    self.pos += 1;
                    continue;
                }
                if self.frames.is_empty() && self.last_src.is_none() {
                    self.read_format(pos);
                }
                l
            };
            let at = self.base + pos as u64;
            let mut t = None;
            while let Some(&(off, p)) = self.anchors.front() {
                if off > at {
                    break;
                }
                self.anchors.pop_front();
                if self.last_frame.is_none_or(|lf| off > lf) {
                    t = Some(p);
                }
            }
            let time = match (t, self.last_src) {
                (Some(p), _) => p,
                (None, Some(l)) => l + self.dur,
                // Noch kein Zeitstempel bekannt: Rahmen ohne Bezug entfällt
                (None, None) => {
                    self.pos += len;
                    continue;
                }
            };
            self.last_src = Some(time);
            self.last_frame = Some(at);
            self.frames.push_back((self.es[pos..pos + len].to_vec(), time));
            self.pos += len;
        }
        if self.pos > 0 {
            self.es.drain(..self.pos);
            self.base += self.pos as u64;
            self.pos = 0;
        }
    }

    /// Kanäle, Abtastrate und Rahmendauer aus dem ersten Rahmen
    fn read_format(&mut self, pos: usize) {
        let f = &self.es[pos..];
        match self.kind {
            AKind::Ac3 => {
                self.rate = [48_000, 44_100, 32_000].get(usize::from(f[4] >> 6)).copied().unwrap_or(48_000);
                let acmod = (f[6] >> 5) as usize;
                let mut ch = [2u8, 1, 2, 3, 3, 4, 4, 5][acmod];
                // LFE-Bit hinter den Mischangaben
                let mut bit = 3usize;
                if acmod & 1 != 0 && acmod != 1 {
                    bit += 2;
                }
                if acmod & 4 != 0 {
                    bit += 2;
                }
                if acmod == 2 {
                    bit += 2;
                }
                let byte = 6 + bit / 8;
                if f.get(byte).is_some_and(|b| b & (0x80 >> (bit % 8)) != 0) {
                    ch += 1;
                }
                self.channels = ch;
                self.dur = 1536 * 90_000 / i64::from(self.rate);
            }
            AKind::Mpeg => {
                let mpeg1 = (f[1] >> 3) & 3 == 3;
                let r = [44_100u32, 48_000, 32_000].get(usize::from((f[2] >> 2) & 3)).copied().unwrap_or(48_000);
                self.rate = if mpeg1 { r } else { r / 2 };
                self.channels = if f[3] >> 6 == 3 { 1 } else { 2 };
                let samples = if (f[1] >> 1) & 3 == 3 { 384 } else { 1152 };
                self.dur = samples * 90_000 / i64::from(self.rate);
            }
            AKind::Lpcm { .. } => {}
        }
    }

    /// Ausgabezeit eines Rahmens; bei einem Sprung in der Quelle geht es lückenlos weiter.
    fn out_time(&self, src: i64, video_off: i64) -> (i64, i64) {
        let off = self.off.unwrap_or(video_off);
        let mut t = src + off;
        let mut new_off = off;
        if let Some(lo) = self.last_out {
            if (t - (lo + self.dur)).abs() > JUMP {
                t = lo + self.dur;
                new_off = t - src;
            }
        }
        (t, new_off)
    }
}

// ---------------------------------------------------------------------------
// Umpacken
// ---------------------------------------------------------------------------

/// Eine geschriebene VOBU für die Navigationspakete und Tabellen.
struct VobuIndex {
    start: u32,
    len: u32,
    s_out: i64,
    e_out: i64,
    scr: u64,
    refs: [u32; 3],
    sync: [u16; 8],
}

struct Importer {
    out: BufWriter<File>,
    path: PathBuf,
    video: VideoParser,
    audio: Vec<AudioStream>,
    index: Vec<VobuIndex>,
    sectors: u32,
    v_off: Option<i64>,
    last_e_out: Option<i64>,
    first_s_out: Option<i64>,
    last_scr: u64,
}

impl Importer {
    fn new(path: &Path) -> Res<Self> {
        let f = crate::dvd::create_output(path)?;
        Ok(Self {
            out: BufWriter::with_capacity(4 << 20, f),
            path: path.to_path_buf(),
            video: VideoParser::default(),
            audio: Vec::new(),
            index: Vec::new(),
            sectors: 0,
            v_off: None,
            last_e_out: None,
            first_s_out: None,
            last_scr: 0,
        })
    }

    /// Liest Pakete aus `buf`; liefert, wie viele Bytes verbraucht sind.
    fn parse(&mut self, buf: &[u8], eof: bool) -> Res<usize> {
        let mut p = 0usize;
        while p + 4 <= buf.len() {
            if buf[p] != 0 || buf[p + 1] != 0 || buf[p + 2] != 1 {
                p += 1;
                continue;
            }
            let id = buf[p + 3];
            match id {
                0xBA => {
                    if p + 14 > buf.len() {
                        break;
                    }
                    if buf[p + 4] & 0xC0 != 0x40 {
                        return Err(tr!(
                            "Die Datei ist ein MPEG-1-Programmstrom (z. B. Video-CD). Geöffnet werden können MPEG-2-Dateien wie auf DVDs.",
                            "The file is an MPEG-1 program stream (e.g. Video CD). MPEG-2 files as used on DVDs can be opened."
                        )
                        .into());
                    }
                    p += 14 + usize::from(buf[p + 13] & 7);
                }
                0xB9 => p += 4,
                0xBB..=0xFF => {
                    if p + 6 > buf.len() {
                        break;
                    }
                    let l = 6 + usize::from(u16::from_be_bytes([buf[p + 4], buf[p + 5]]));
                    if p + l > buf.len() {
                        break;
                    }
                    self.pes(&buf[p..p + l])?;
                    p += l;
                }
                _ => p += 1,
            }
        }
        if eof {
            p = buf.len();
        }
        Ok(p)
    }

    fn pes(&mut self, pkt: &[u8]) -> Res<()> {
        let id = pkt[3];
        if !(id == 0xBD || (0xC0..=0xEF).contains(&id)) || pkt.len() < 9 || pkt[6] & 0xC0 != 0x80 {
            return Ok(());
        }
        let hdr = usize::from(pkt[8]);
        let start = 9 + hdr;
        if start > pkt.len() {
            return Ok(());
        }
        let pts = (pkt[7] & 0x80 != 0 && pkt.len() >= 14).then(|| read_ts(&pkt[9..]) as i64);
        let dts = (pkt[7] & 0xC0 == 0xC0 && pkt.len() >= 19).then(|| read_ts(&pkt[14..]) as i64);
        let payload = &pkt[start..];
        match id {
            0xE0 => {
                self.video.push(payload, pts, dts);
                for g in self.video.take_gops(false) {
                    self.write_gop(g, false)?;
                }
            }
            0xC0..=0xC7 => self.audio_in(id, AKind::Mpeg, payload, pts, None),
            0xBD if payload.len() > 4 => {
                let sub = payload[0];
                match sub {
                    0x80..=0x87 => self.audio_in(sub, AKind::Ac3, &payload[4..], pts, None),
                    0xA0..=0xA7 if payload.len() > 7 => {
                        let info = payload[5];
                        let bits = match info >> 6 {
                            0 => 16,
                            1 => 20,
                            _ => 24,
                        };
                        let rate = if (info >> 4) & 3 == 1 { 96_000 } else { 48_000 };
                        let kind = AKind::Lpcm { bits, rate, ch: (info & 7) + 1, hdr: [payload[4] & 0xE0, info, payload[6]] };
                        // Zeiger auf den ersten Rahmen zählt ab dem Byte nach dem Zeigerfeld
                        let ptr = usize::from(u16::from_be_bytes([payload[2], payload[3]]));
                        let first = ptr.checked_sub(4);
                        self.audio_in(sub, kind, &payload[7..], pts, first);
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn audio_in(&mut self, src: u8, kind: AKind, payload: &[u8], pts: Option<i64>, first: Option<usize>) {
        let i = match self.audio.iter().position(|a| a.src == src) {
            Some(i) => i,
            None => {
                if self.audio.len() >= 8 {
                    return;
                }
                let phys = self.audio.len() as u8;
                let mut a = AudioStream::new(src, kind, phys);
                if let AKind::Lpcm { rate, ch, .. } = kind {
                    a.rate = rate;
                    a.channels = ch;
                    a.dur = 150; // 1/600 s
                }
                self.audio.push(a);
                self.audio.len() - 1
            }
        };
        let a = &mut self.audio[i];
        a.push(payload, pts, first);
        a.parse(false);
    }

    /// Schreibt eine Bildgruppe als VOBU, samt dem Ton bis zu ihrem Ende.
    fn write_gop(&mut self, g: Gop, last: bool) -> Res<()> {
        if !self.video.mpeg2 {
            return Err(tr!(
                "Die Datei enthält MPEG-1-Video. Geöffnet werden können Dateien mit MPEG-2-Bild wie auf DVDs.",
                "The file contains MPEG-1 video. Files with MPEG-2 video as used on DVDs can be opened."
            )
            .into());
        }
        let ft = self.video.frame_ticks();
        let fields: u32 = g.pics.iter().map(|p| p.3).sum::<u32>().max(2);
        let dur = (f64::from(fields) * ft / 2.0).round() as i64;
        // Anzeigebeginn: erstes Bild mit Zeitstempel, zurückgerechnet über seine Anzeigeposition
        let s_src = g.pics.iter().find_map(|p| p.4.map(|t| t - (f64::from(p.1) * ft).round() as i64));
        let s_src = match (s_src, self.v_off, self.last_e_out) {
            (Some(s), _, _) => s,
            (None, Some(off), Some(e)) => e - off,
            _ => return Ok(()), // Anfang ohne Zeitstempel: überspringen
        };
        let off = match (self.v_off, self.last_e_out) {
            (Some(off), Some(e)) if (s_src + off - e).abs() <= JUMP => off,
            (_, Some(e)) => e - s_src,
            _ => START_PTS - s_src,
        };
        self.v_off = Some(off);
        let s_out = s_src + off;
        let e_out = s_out + dur;
        self.first_s_out.get_or_insert(s_out);
        self.last_e_out = Some(e_out);

        // Videopakete
        let mut video: Vec<[u8; SECTOR]> = Vec::new();
        let mut ends: Vec<usize> = Vec::new();
        let mut emitted = 0usize;
        let mut next_pic = 0usize;
        let hdr = pack_header(0);
        while emitted < g.data.len() {
            while next_pic < g.pics.len() && g.pics[next_pic].0 < emitted {
                next_pic += 1;
            }
            let mut ts = None;
            if let Some(p) = g.pics.get(next_pic) {
                if p.0 < emitted + SECTOR - 14 - 9 - 10 {
                    if let Some(pts) = p.4 {
                        let d = p.5.map(|d| (d + off).max(0) as u64);
                        ts = Some(((pts + off).max(0) as u64, d));
                    }
                }
            }
            let (s, take) = build_video_pack(&hdr, ts, &[], &g.data[emitted..]);
            emitted += take;
            ends.push(emitted);
            video.push(s);
        }

        // Ton bis zum Ende dieser Gruppe (am Ende der Datei: alles)
        let first_s = self.first_s_out.unwrap_or(s_out);
        let mut audio: Vec<(u8, [u8; SECTOR])> = Vec::new();
        for a in self.audio.iter_mut() {
            if last {
                a.parse(true);
            }
            let mut take: Vec<(Vec<u8>, i64)> = Vec::new();
            while let Some((_, src)) = a.frames.front() {
                let (t, new_off) = a.out_time(*src, off);
                if !last && t >= e_out {
                    break;
                }
                let (data, _) = a.frames.pop_front().unwrap();
                a.off = Some(new_off);
                // Ton vor dem ersten Bild wird nie gezeigt
                if t + a.dur <= first_s {
                    continue;
                }
                a.last_out = Some(t);
                take.push((data, t));
            }
            for s in audio_packs(a, &take) {
                audio.push((a.phys, s));
            }
        }

        // Verzahnen: Navigationspaket, dann Bild und Ton gleichmäßig verteilt
        let n_v = video.len();
        let n_a = audio.len();
        let mut sectors: Vec<[u8; SECTOR]> = Vec::with_capacity(1 + n_v + n_a);
        let mut sync = [0x3FFFu16; 8];
        let mut video_at: Vec<usize> = Vec::with_capacity(n_v);
        let mut nav = [0u8; SECTOR];
        nav.copy_from_slice(&nav_pack(0));
        sectors.push(nav);
        let (mut vi, mut ai) = (0usize, 0usize);
        while vi < n_v || ai < n_a {
            let take_video = ai >= n_a || (vi < n_v && (vi as f64 / n_v as f64) <= (ai as f64 / n_a.max(1) as f64));
            if take_video {
                video_at.push(sectors.len());
                sectors.push(video[vi]);
                vi += 1;
            } else {
                let (phys, s) = audio[ai];
                let k = usize::from(phys);
                if sync[k] == 0x3FFF {
                    sync[k] = sectors.len() as u16;
                }
                sectors.push(s);
                ai += 1;
            }
        }
        // Systemuhr: etwas vor dem ersten Bild, gleichmäßig weiter
        let mut scr = (((s_out - 27_000).max(0) as u64) * 300).max(self.last_scr + SCR_TICK);
        let nav_scr = scr;
        for s in sectors.iter_mut() {
            put_scr(s, scr);
            self.last_scr = scr;
            scr += SCR_TICK;
        }
        // Ende der ersten drei Bezugsbilder (I/P) in Sektoren der VOBU
        let mut refs = [0u32; 3];
        let mut r = 0;
        for (k, p) in g.pics.iter().enumerate() {
            if r >= 3 {
                break;
            }
            if p.2 == 1 || p.2 == 2 {
                let end = g.pics.get(k + 1).map(|q| q.0).unwrap_or(g.data.len());
                let idx = ends.partition_point(|e| *e < end).min(video_at.len().saturating_sub(1));
                refs[r] = video_at.get(idx).copied().unwrap_or(0) as u32;
                r += 1;
            }
        }
        let len = sectors.len() as u32;
        for s in &sectors {
            self.out.write_all(s).map_err(|e| trf!("Schreibfehler: {e}", "Write error: {e}"))?;
        }
        self.index.push(VobuIndex { start: self.sectors, len, s_out, e_out, scr: nav_scr, refs, sync });
        self.sectors += len;
        Ok(())
    }

    /// Schreibt Navigationspakete und das IFO des Titelsets `vts`; liefert dessen
    /// Angaben für VIDEO_TS.IFO (Attribute, Größe in Sektoren, PAL).
    fn finish(mut self, out_dir: &Path, vts: u8) -> Res<(Vec<u8>, u32, bool)> {
        let gops = self.video.take_gops(true);
        let n = gops.len();
        for (k, g) in gops.into_iter().enumerate() {
            self.write_gop(g, k + 1 == n)?;
        }
        if self.index.is_empty() {
            return Err(tr!("In der Datei wurde kein MPEG-2-Bild gefunden.", "No MPEG-2 video was found in the file.").into());
        }
        self.out.flush().map_err(|e| e.to_string())?;
        drop(self.out);

        // Navigationspakete mit allen Verweisen
        let pal = self.video.rate == 3;
        let first = self.first_s_out.unwrap_or(START_PTS);
        let times: Vec<u64> = self.index.iter().map(|v| ((v.s_out - first).max(0) / 90) as u64).collect();
        let starts: Vec<u32> = self.index.iter().map(|v| v.start).collect();
        let mut f = OpenOptions::new().write(true).open(&self.path).map_err(|e| format!("{}: {e}", self.path.display()))?;
        let n = self.index.len();
        for (i, v) in self.index.iter().enumerate() {
            let mut nav = nav_pack(v.scr);
            put32(&mut nav, PCI_OFF, v.start);
            put32(&mut nav, PCI_OFF + 12, v.s_out as u32);
            put32(&mut nav, PCI_OFF + 16, v.e_out as u32);
            put32(&mut nav, DSI_OFF, (v.scr / 300) as u32);
            put32(&mut nav, DSI_OFF + 4, v.start);
            put32(&mut nav, DSI_OFF + 8, v.len - 1);
            for (k, r) in v.refs.iter().enumerate() {
                put32(&mut nav, DSI_OFF + 12 + k * 4, *r);
            }
            put16(&mut nav, DSI_OFF + 24, 1);
            nav[DSI_OFF + 27] = 1;
            nav[DSI_OFF + 28..DSI_OFF + 32].copy_from_slice(&encode_time(times[i], pal));
            fill_sri(&mut nav, i, &starts, &times);
            for (k, s) in v.sync.iter().enumerate() {
                put16(&mut nav, DSI_OFF + DSI_SYNCI + k * 2, *s);
            }
            for sp in 0..32 {
                put32(&mut nav, DSI_OFF + DSI_SYNCI + 16 + sp * 4, 0x3FFF_FFFF);
            }
            f.seek(SeekFrom::Start(u64::from(v.start) * SECTOR as u64)).map_err(|e| e.to_string())?;
            f.write_all(&nav).map_err(|e| trf!("Schreibfehler: {e}", "Write error: {e}"))?;
        }
        drop(f);

        // IFO-Dateien: ein Titel, eine Zelle
        let last = &self.index[n - 1];
        let total_ms = ((last.e_out - first).max(0) / 90) as u64;
        let total = self.sectors;
        let attrs = make_attrs(&self.video, &self.audio);
        let mut pgc = APgc { pal, ..Default::default() };
        for a in &self.audio {
            pgc.audio_ctl[usize::from(a.phys)] = 0x8000 | (u16::from(a.phys) << 8);
        }
        pgc.cells.push(ACell { ms: total_ms, flags: [0; 4], first: 0, last_vobu: last.start, last: total - 1, vob: 1, cell: 1 });
        pgc.programs.push(1);
        pgc.post = vec![author::CMD_EXIT];
        let points: Vec<(u64, u32)> = times.iter().copied().zip(starts.iter().copied()).collect();
        let avts = AVts {
            attrs: attrs.clone(),
            pgcs: vec![pgc],
            menu: None,
            cells: vec![CellAddr { vob_id: 1, cell_id: 1, start: 0, last: total - 1 }],
            admap: starts,
            tmaps: vec![make_tmap(&points)],
            title_sectors: total,
        };
        let write = |name: &str, data: &[u8]| crate::dvd::write_output(&out_dir.join(name), data);
        let ifo = author::build_vts(&avts);
        write(&format!("VTS_{vts:02}_0.IFO"), &ifo)?;
        write(&format!("VTS_{vts:02}_0.BUP"), &ifo)?;
        let size = (ifo.len() / SECTOR) as u32 * 2 + total;
        Ok((attrs, size, pal))
    }
}

/// VIDEO_TS.IFO für umgepackte Dateien: je Titelset ein Titel, abgespielt nacheinander.
fn write_vmg(out_dir: &Path, sets: &[(Vec<u8>, u32, bool)]) -> Res<()> {
    let first = &sets[0];
    let titles = (1..=sets.len())
        .map(|n| TitleEntry { playback_type: 0x3C, angles: 1, chapters: 1, vts: n as u8, vts_ttn: 1, vts_sector: 0 })
        .collect();
    let mut vmg = AVmg {
        titles,
        vts_attrs: sets.iter().map(|s| s.0.clone()).collect(),
        first_play: vec![author::cmd_jump_tt(1)],
        menu: None,
        menu_video: [first.0[0], first.0[1]],
        pal: first.2,
    };
    let sizes: Vec<u32> = sets.iter().map(|s| s.1).collect();
    let probe = author::build_vmg(&vmg);
    author::set_vts_starts(&mut vmg, &sizes, (probe.len() / SECTOR) as u32 * 2);
    let bytes = author::build_vmg(&vmg);
    crate::dvd::write_output(&out_dir.join("VIDEO_TS.IFO"), &bytes)?;
    crate::dvd::write_output(&out_dir.join("VIDEO_TS.BUP"), &bytes)
}

/// Tonpakete für eine Folge vollständiger Rahmen (Daten, Ausgabezeit).
fn audio_packs(a: &AudioStream, frames: &[(Vec<u8>, i64)]) -> Vec<[u8; SECTOR]> {
    let mut stream: Vec<u8> = Vec::new();
    let mut starts: Vec<(usize, i64)> = Vec::new();
    for (d, t) in frames {
        starts.push((stream.len(), *t));
        stream.extend_from_slice(d);
    }
    let (id, private) = match a.kind {
        AKind::Mpeg => (0xC0 | a.phys, 0usize),
        AKind::Ac3 => (0xBD, 4),
        AKind::Lpcm { .. } => (0xBD, 7),
    };
    // LPCM: nur ganze Abtastgruppen je Paket
    let group = match a.kind {
        AKind::Lpcm { bits: 16, ch, .. } => 2 * usize::from(ch),
        AKind::Lpcm { bits: 20, ch, .. } => 5 * usize::from(ch),
        AKind::Lpcm { ch, .. } => 6 * usize::from(ch),
        _ => 1,
    };
    let room = (SECTOR - 14 - 9 - 5 - private) / group * group;
    let mut out = Vec::new();
    let mut pos = 0usize;
    let mut si = 0usize;
    while pos < stream.len() {
        let take = (stream.len() - pos).min(room);
        while si < starts.len() && starts[si].0 < pos {
            si += 1;
        }
        let inside: Vec<&(usize, i64)> = starts[si..].iter().take_while(|s| s.0 < pos + take).collect();
        let mut s = [0u8; SECTOR];
        s[..14].copy_from_slice(&pack_header(0));
        s[14..18].copy_from_slice(&[0, 0, 1, id]);
        s[20] = 0x81;
        let mut hdr_data = 0usize;
        if let Some(first) = inside.first() {
            s[21] = 0x80;
            write_ts(&mut s[23..], 2, first.1.max(0) as u64);
            hdr_data = 5;
        }
        let used_fixed = 23 + hdr_data + private + take;
        let free = SECTOR - used_fixed;
        let stuffing = if free < 7 { free } else { 0 };
        for b in &mut s[23 + hdr_data..23 + hdr_data + stuffing] {
            *b = 0xFF;
        }
        s[22] = (hdr_data + stuffing) as u8;
        let mut o = 23 + hdr_data + stuffing;
        let ptr = |extra: usize| inside.first().map(|f| (f.0 - pos + extra) as u16).unwrap_or(0);
        match a.kind {
            AKind::Ac3 => {
                s[o] = 0x80 | a.phys;
                s[o + 1] = inside.len() as u8;
                s[o + 2..o + 4].copy_from_slice(&ptr(1).to_be_bytes());
            }
            AKind::Lpcm { hdr, .. } => {
                s[o] = 0xA0 | a.phys;
                s[o + 1] = inside.len() as u8;
                s[o + 2..o + 4].copy_from_slice(&ptr(4).to_be_bytes());
                s[o + 4..o + 7].copy_from_slice(&hdr);
            }
            AKind::Mpeg => {}
        }
        o += private;
        s[o..o + take].copy_from_slice(&stream[pos..pos + take]);
        o += take;
        put16(&mut s, 18, (o - 20) as u16);
        if o < SECTOR {
            s[o..o + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
            put16(&mut s, o + 4, (SECTOR - o - 6) as u16);
            s[o + 6..].fill(0xFF);
        }
        out.push(s);
        pos += take;
    }
    out
}

/// Sprungtabelle (DSI_SRI): Abstände zu VOBUs in festen Zeitschritten vor und zurück.
fn fill_sri(nav: &mut [u8], i: usize, starts: &[u32], times: &[u64]) {
    let n = starts.len();
    let start = starts[i];
    let fwd = |half_secs: u64| -> u32 {
        let want = times[i] + half_secs * 500;
        let mut j = i;
        while j + 1 < n && times[j] < want {
            j += 1;
        }
        if j == i {
            SRI_END
        } else {
            (starts[j] - start) | 0x8000_0000
        }
    };
    let back = |half_secs: u64| -> u32 {
        let want = times[i].saturating_sub(half_secs * 500);
        let mut j = i;
        while j > 0 && times[j] > want {
            j -= 1;
        }
        if j == i {
            SRI_END
        } else {
            (start - starts[j]) | 0x8000_0000
        }
    };
    const STEPS: [u64; 19] = [240, 120, 60, 20, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1];
    let base = DSI_OFF + DSI_SRI;
    put32(nav, base, if i + 1 < n { (starts[i + 1] - start) | 0x8000_0000 } else { SRI_END });
    for (k, step) in STEPS.iter().enumerate() {
        put32(nav, base + 4 + k * 4, fwd(*step));
    }
    put32(nav, base + 80, if i + 1 < n { starts[i + 1] - start } else { SRI_END });
    put32(nav, base + 84, if i > 0 { start - starts[i - 1] } else { SRI_END });
    for (k, step) in STEPS.iter().rev().enumerate() {
        put32(nav, base + 88 + k * 4, back(*step));
    }
    put32(nav, base + 164, if i > 0 { (start - starts[i - 1]) | 0x8000_0000 } else { SRI_END });
}

/// Bild- und Tonangaben für die IFO-Dateien.
fn make_attrs(v: &VideoParser, audio: &[AudioStream]) -> Vec<u8> {
    let mut a = vec![0u8; author::ATTR_END - author::ATTR_START];
    let at = |o: usize| o - author::ATTR_START;
    let pal = v.rate == 3;
    let wide = v.aspect == 3 || v.aspect == 4;
    // MPEG-2, Norm, Seitenverhältnis; Auflösung
    let b0 = 0x40 | if pal { 0x10 } else { 0 } | if wide { 0x0C } else { 0 } | 0x01;
    let res = match (v.width, v.height) {
        (704, _) => 1,
        (352, h) if h >= 480 => 2,
        (352, _) => 3,
        _ => 0,
    };
    let video = [b0, res << 3];
    a[at(0x100)..at(0x102)].copy_from_slice(&video);
    a[at(0x200)..at(0x202)].copy_from_slice(&video);
    let mut sorted: Vec<&AudioStream> = audio.iter().collect();
    sorted.sort_by_key(|s| s.phys);
    put16(&mut a, at(0x202), sorted.len() as u16);
    for s in sorted {
        let o = at(0x204) + usize::from(s.phys) * 8;
        let (mode, quant) = match s.kind {
            AKind::Ac3 => (0u8, 0u8),
            AKind::Mpeg => (2, 0),
            AKind::Lpcm { bits, .. } => (4, match bits {
                16 => 0,
                20 => 1,
                _ => 2,
            }),
        };
        a[o] = mode << 5;
        let rate = if s.rate == 96_000 { 1 } else { 0 };
        a[o + 1] = (quant << 6) | (rate << 4) | (s.channels.clamp(1, 8) - 1);
    }
    a
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer II, 48 kHz, 192 kBit/s: 576 Bytes, 24 ms je Rahmen
    fn mpa(fill: u8) -> Vec<u8> {
        let mut f = vec![fill; 576];
        f[..4].copy_from_slice(&[0xFF, 0xFD, 0xA4, 0x00]);
        f
    }

    #[test]
    fn audio_frames_get_times_and_packets() {
        let mut a = AudioStream::new(0xC0, AKind::Mpeg, 0);
        let es: Vec<u8> = (1..=10u8).flat_map(mpa).collect();
        // Erstes Paket beginnt mitten in einem Rahmen: der Zeitstempel gilt für den nächsten
        a.push(&es[100..2000], Some(90_000), None);
        a.push(&es[2000..], Some(90_000 + 3 * 2160), None);
        a.parse(true);
        let times: Vec<i64> = a.frames.iter().map(|f| f.1).collect();
        assert_eq!(times.len(), 9);
        assert_eq!(times[0], 90_000);
        assert_eq!(times[3], 90_000 + 3 * 2160);
        let frames: Vec<(Vec<u8>, i64)> = a.frames.drain(..).collect();
        let packs = audio_packs(&a, &frames);
        let mut back = Vec::new();
        for s in &packs {
            assert_eq!(&s[..4], &[0, 0, 1, 0xBA]);
            let p = 14;
            let end = p + 6 + usize::from(u16::from_be_bytes([s[p + 4], s[p + 5]]));
            back.extend_from_slice(&s[p + 9 + usize::from(s[p + 8])..end]);
        }
        assert_eq!(back, es[576..].to_vec());
    }

    #[test]
    fn transport_stream_is_rejected() {
        let mut ts = vec![0u8; 188 * 4];
        for k in 0..4 {
            ts[k * 188] = 0x47;
        }
        assert!(check_container(&ts).is_err());
    }
}
