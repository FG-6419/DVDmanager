//! Packt einen MPEG-2-Programmstrom (Bild, alle Tonspuren, DVD-Untertitel) ohne Neukodierung
//! in eine Matroska-Datei (MKV) und trägt Kapitel und Sprachen ein. VOB- und MPEG-Dateien
//! können keine Kapitel enthalten; MKV schon, und VLC, Kodi, MPC und viele Fernseher zeigen
//! sie an. Untertitel bleiben Bilder (VobSub), wie auf der DVD.

use std::collections::VecDeque;
use std::fs::File;
use std::io::{BufWriter, Read, Seek, SeekFrom, Write};
use std::path::Path;

use crate::dvd::vob::read_ts;
use crate::dvd::Res;
use crate::engine::import::{AKind, AudioStream, Gop, VideoParser};

// ---------------------------------------------------------------------------
// EBML
// ---------------------------------------------------------------------------

const EBML: u32 = 0x1A45_DFA3;
const SEGMENT: u32 = 0x1853_8067;
const SEEK_HEAD: u32 = 0x114D_9B74;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POS: u32 = 0x53AC;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CHAPTERS: u32 = 0x1043_A770;
const CLUSTER: u32 = 0x1F43_B675;
const CUES: u32 = 0x1C53_BB6B;
const DURATION: u32 = 0x4489;

fn id_bytes(id: u32) -> Vec<u8> {
    let b = id.to_be_bytes();
    let skip = b.iter().take_while(|x| **x == 0).count().min(3);
    b[skip..].to_vec()
}

/// Länge als EBML-Zahl, so kurz wie möglich.
fn vint(n: u64) -> Vec<u8> {
    let mut len = 1;
    while len < 8 && n >= (1u64 << (7 * len)) - 1 {
        len += 1;
    }
    let mut out = vec![0u8; len];
    for (i, b) in out.iter_mut().rev().enumerate() {
        *b = (n >> (8 * i)) as u8;
    }
    out[0] |= 0x80 >> (len - 1);
    out
}

fn el(id: u32, body: &[u8]) -> Vec<u8> {
    let mut v = id_bytes(id);
    v.extend(vint(body.len() as u64));
    v.extend_from_slice(body);
    v
}

fn uint(id: u32, v: u64) -> Vec<u8> {
    let b = v.to_be_bytes();
    let skip = b.iter().take_while(|x| **x == 0).count().min(7);
    el(id, &b[skip..])
}

/// Zahl mit fester Länge von acht Bytes (wird später überschrieben).
fn uint8(id: u32, v: u64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

fn float(id: u32, v: f64) -> Vec<u8> {
    el(id, &v.to_be_bytes())
}

fn text(id: u32, s: &str) -> Vec<u8> {
    el(id, s.as_bytes())
}

// ---------------------------------------------------------------------------
// Lesen des Programmstroms
// ---------------------------------------------------------------------------

/// Ein DVD-Untertitel (SPU), über mehrere Pakete zusammengesetzt.
#[derive(Default)]
struct SubAsm {
    cur: Option<(i64, Vec<u8>)>,
    done: VecDeque<(i64, Vec<u8>)>,
}

impl SubAsm {
    fn push(&mut self, data: &[u8], pts: Option<i64>) {
        if let Some(t) = pts {
            // Neuer Untertitel; ein unvollständiger alter entfällt
            self.cur = Some((t, Vec::new()));
        }
        let Some((t, buf)) = self.cur.as_mut() else { return };
        buf.extend_from_slice(data);
        let need = if buf.len() >= 2 { usize::from(u16::from_be_bytes([buf[0], buf[1]])) } else { usize::MAX };
        if buf.len() >= need {
            buf.truncate(need);
            let t = *t;
            let (_, b) = self.cur.take().unwrap();
            self.done.push_back((t, b));
        }
    }
}

/// Anzeigedauer eines DVD-Untertitels aus seinen Steuerfolgen (Millisekunden).
fn spu_duration_ms(spu: &[u8]) -> Option<u64> {
    if spu.len() < 4 {
        return None;
    }
    let mut at = usize::from(u16::from_be_bytes([spu[2], spu[3]]));
    for _ in 0..16 {
        if at + 4 > spu.len() {
            return None;
        }
        let date = u64::from(u16::from_be_bytes([spu[at], spu[at + 1]]));
        let next = usize::from(u16::from_be_bytes([spu[at + 2], spu[at + 3]]));
        let mut p = at + 4;
        while p < spu.len() {
            let cmd = spu[p];
            p += 1;
            match cmd {
                0x02 => return Some(date * 1024 / 90),
                0x00 | 0x01 => {}
                0x03 | 0x04 => p += 2,
                0x05 => p += 6,
                0x06 => p += 4,
                0x07 => {
                    let l = spu.get(p..p + 2).map(|b| usize::from(u16::from_be_bytes([b[0], b[1]]))).unwrap_or(0);
                    p += l.max(2);
                }
                _ => break,
            }
        }
        if next == at {
            return None;
        }
        at = next;
    }
    None
}

/// Bild, alle Tonspuren und alle Untertitel eines Programmstroms.
struct Demux {
    video: VideoParser,
    audio: Vec<AudioStream>,
    subs: std::collections::BTreeMap<u8, SubAsm>,
    gops: VecDeque<Gop>,
}

impl Demux {
    fn new() -> Self {
        Self { video: VideoParser::default(), audio: Vec::new(), subs: Default::default(), gops: VecDeque::new() }
    }

    /// Liest Pakete aus `buf`; liefert, wie viele Bytes verbraucht sind.
    fn feed(&mut self, buf: &[u8], eof: bool) -> usize {
        let mut p = 0usize;
        while p + 4 <= buf.len() {
            if buf[p] != 0 || buf[p + 1] != 0 || buf[p + 2] != 1 {
                p += 1;
                continue;
            }
            match buf[p + 3] {
                0xBA => {
                    if p + 14 > buf.len() {
                        break;
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
                    self.pes(&buf[p..p + l]);
                    p += l;
                }
                _ => p += 1,
            }
        }
        if eof {
            self.gops.extend(self.video.take_gops(true));
            for a in self.audio.iter_mut() {
                a.parse(true);
            }
            p = buf.len();
        }
        p
    }

    fn pes(&mut self, pkt: &[u8]) {
        let id = pkt[3];
        if !(id == 0xBD || (0xC0..=0xEF).contains(&id)) || pkt.len() < 9 || pkt[6] & 0xC0 != 0x80 {
            return;
        }
        let start = 9 + usize::from(pkt[8]);
        if start > pkt.len() {
            return;
        }
        let pts = (pkt[7] & 0x80 != 0 && pkt.len() >= 14).then(|| read_ts(&pkt[9..]) as i64);
        let dts = (pkt[7] & 0xC0 == 0xC0 && pkt.len() >= 19).then(|| read_ts(&pkt[14..]) as i64);
        let payload = &pkt[start..];
        match id {
            0xE0 => {
                self.video.push(payload, pts, dts);
                self.gops.extend(self.video.take_gops(false));
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
                        let ptr = usize::from(u16::from_be_bytes([payload[2], payload[3]]));
                        self.audio_in(sub, kind, &payload[7..], pts, ptr.checked_sub(4));
                    }
                    0x20..=0x3F => self.subs.entry(sub & 0x1F).or_default().push(&payload[1..], pts),
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn audio_in(&mut self, src: u8, kind: AKind, payload: &[u8], pts: Option<i64>, first: Option<usize>) {
        let i = match self.audio.iter().position(|a| a.src == src) {
            Some(i) => i,
            None => {
                if self.audio.len() >= 8 {
                    return;
                }
                let mut a = AudioStream::new(src, kind, src & 7);
                if let AKind::Lpcm { rate, ch, .. } = kind {
                    a.rate = rate;
                    a.channels = ch;
                    a.dur = 150;
                }
                self.audio.push(a);
                self.audio.len() - 1
            }
        };
        let a = &mut self.audio[i];
        a.push(payload, pts, first);
        a.parse(false);
    }
}

// ---------------------------------------------------------------------------
// Schreiben
// ---------------------------------------------------------------------------

/// Anzeigezeiten der Bilder einer Gruppe (in Dekodierreihenfolge) sowie Anfang und Ende.
/// Die Zeit eines Bildes ergibt sich aus den Halbbildern aller Bilder, die vor ihm
/// angezeigt werden; so stimmt sie auch bei Pulldown (3:2).
fn picture_times(g: &Gop, ft: f64, fallback: Option<i64>) -> Option<(Vec<i64>, i64, i64)> {
    let mut order: Vec<usize> = (0..g.pics.len()).collect();
    order.sort_by_key(|i| g.pics[*i].1);
    let mut offs = vec![0i64; g.pics.len()];
    let mut fields = 0u32;
    for i in &order {
        offs[*i] = (f64::from(fields) * ft / 2.0).round() as i64;
        fields += g.pics[*i].3;
    }
    let start = g.pics.iter().enumerate().find_map(|(i, p)| p.4.map(|t| t - offs[i])).or(fallback)?;
    let end = start + (f64::from(fields.max(2)) * ft / 2.0).round() as i64;
    Some((offs.iter().map(|o| start + o).collect(), start, end))
}

/// DVD-LPCM (20/24 Bit, eigenwillig gepackt) zu 24 Bit Big-Endian, abwechselnd je Kanal.
fn lpcm_to_24(data: &[u8], bits: u8, ch: u8) -> Vec<u8> {
    let ch = usize::from(ch.max(1));
    let low = if bits == 24 { 2 } else { 1 };
    let group = 2 * ch * 2 + ch * low;
    let mut out = Vec::with_capacity(data.len() / group * 2 * ch * 3);
    for g in data.chunks_exact(group) {
        let tail = &g[4 * ch..];
        for s in 0..2 {
            for c in 0..ch {
                let k = s * ch + c;
                let hi = [g[2 * k], g[2 * k + 1]];
                let lo = if bits == 24 {
                    tail[k]
                } else {
                    // 20 Bit: je Kanal ein Byte mit den unteren vier Bits beider Werte
                    let b = tail[c];
                    (if s == 0 { b & 0xF0 } else { b << 4 }) & 0xF0
                };
                out.extend_from_slice(&[hi[0], hi[1], lo]);
            }
        }
    }
    out
}

struct AudioFormat {
    codec: &'static str,
    rate: u32,
    channels: u8,
    bits: Option<u8>,
    /// LPCM 20/24 Bit muss umgepackt werden
    repack: Option<(u8, u8)>,
}

fn audio_format(a: &AudioStream) -> AudioFormat {
    match a.kind {
        AKind::Ac3 => AudioFormat { codec: "A_AC3", rate: a.rate, channels: a.channels, bits: None, repack: None },
        AKind::Mpeg => AudioFormat { codec: "A_MPEG/L2", rate: a.rate, channels: a.channels, bits: None, repack: None },
        AKind::Lpcm { bits: 16, rate, ch, .. } => AudioFormat { codec: "A_PCM/INT/BIG", rate, channels: ch, bits: Some(16), repack: None },
        AKind::Lpcm { bits, rate, ch, .. } => AudioFormat { codec: "A_PCM/INT/BIG", rate, channels: ch, bits: Some(24), repack: Some((bits, ch)) },
    }
}

/// Sequenzkopf samt Erweiterungen am Anfang der ersten Gruppe (für CodecPrivate).
fn sequence_header(g: &Gop) -> Vec<u8> {
    let end = g.pics.first().map(|p| p.0).unwrap_or(g.data.len());
    let d = &g.data[..end];
    // Bis zum Gruppenkopf (00 00 01 B8), falls vorhanden
    let gop_at = d.windows(4).position(|w| w == [0, 0, 1, 0xB8]).unwrap_or(d.len());
    d[..gop_at].to_vec()
}

struct Out {
    w: BufWriter<File>,
    pos: u64,
}

impl Out {
    fn put(&mut self, b: &[u8]) -> Res<()> {
        self.w.write_all(b).map_err(|e| trf!("Schreibfehler: {e}", "Write error: {e}"))?;
        self.pos += b.len() as u64;
        Ok(())
    }
}

/// Angaben zu den Spuren, die nicht im Datenstrom stehen (aus den IFO-Dateien).
#[derive(Clone, Debug, Default)]
pub struct MkvMeta {
    /// Sprache je Tonspur (physische Nummer 0..7, Sprachkürzel wie „de“)
    pub audio_lang: [String; 8],
    /// Untertitelspuren (physische Nummer 0..31) mit Sprachkürzel, in dieser Reihenfolge
    pub subs: Vec<(u8, String)>,
    /// Farbtabelle der Untertitel als RGB
    pub palette: Option<[u32; 16]>,
}

/// Zweibuchstabiges Sprachkürzel (ISO 639-1) als dreibuchstabiges (ISO 639-2/B) für Matroska.
fn lang3(code: &str) -> &'static str {
    match code {
        "de" => "ger",
        "en" => "eng",
        "fr" => "fre",
        "es" => "spa",
        "it" => "ita",
        "nl" => "dut",
        "pl" => "pol",
        "ru" => "rus",
        "tr" => "tur",
        "pt" => "por",
        "sv" => "swe",
        "da" => "dan",
        "fi" => "fin",
        "no" => "nor",
        "cs" => "cze",
        "hu" => "hun",
        "el" => "gre",
        "ja" => "jpn",
        "zh" => "chi",
        "ko" => "kor",
        "he" => "heb",
        "ar" => "ara",
        "hi" => "hin",
        _ => "und",
    }
}

/// Sprache einer Spur: Matroska-Kürzel (drei Buchstaben) und BCP-47 (zwei).
fn lang_els(code: &str) -> Vec<u8> {
    let mut v = text(0x22_B59C, lang3(code));
    if code.len() == 2 {
        v.extend(text(0x22_B59D, code));
    }
    v
}

/// Packt `src` (MPEG-2-Programmstrom bzw. VOB) in die MKV-Datei `dst`, mit Bild, allen
/// Tonspuren und allen Untertiteln (VobSub). `chapters`: Kapitelanfänge als
/// 90-kHz-Zeitstempel des Programmstroms. Liefert die Größe in Bytes.
/// `progress` bekommt den Anteil 0..1 und liefert false zum Abbrechen.
pub fn remux(src: &Path, dst: &Path, chapters: &[i64], meta: &MkvMeta, progress: &mut dyn FnMut(f32) -> bool) -> Res<u64> {
    let total = std::fs::metadata(src).map_err(|e| format!("{}: {e}", src.display()))?.len().max(1);

    // 1) Formate am Anfang lesen
    let mut probe = Demux::new();
    {
        let mut f = File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
        let mut buf = Vec::new();
        let mut chunk = vec![0u8; 1 << 20];
        let mut read = 0u64;
        loop {
            let n = f.read(&mut chunk).map_err(|e| e.to_string())?;
            read += n as u64;
            buf.extend_from_slice(&chunk[..n]);
            let used = probe.feed(&buf, n == 0);
            buf.drain(..used);
            // Alle Tonspuren stehen in den ersten Megabytes; jede braucht einen Rahmen für ihr Format.
            let audio_known = !probe.audio.is_empty() && probe.audio.iter().all(|a| !a.frames.is_empty());
            if n == 0 || (!probe.gops.is_empty() && audio_known && read > 8 << 20) || (read > 48 << 20 && !probe.gops.is_empty()) {
                break;
            }
        }
    }
    let first = probe.gops.front().ok_or(tr!("In der Datei wurde kein MPEG-2-Bild gefunden.", "No MPEG-2 video was found in the file."))?;
    let v = &probe.video;
    let ft = v.frame_ticks();
    let seq = sequence_header(first);
    // Tonspuren nach ihrer Nummer; Spur 1 ist das Bild, dann der Ton, dann die Untertitel.
    let mut audio_tracks: Vec<(u8, AudioFormat)> = probe.audio.iter().filter(|a| !a.frames.is_empty()).map(|a| (a.src, audio_format(a))).collect();
    audio_tracks.sort_by_key(|(src, _)| src & 7);
    let audio_track_of = |src: u8| audio_tracks.iter().position(|(s, _)| *s == src).map(|k| k as u64 + 2);
    let sub_track_of = |id: u8| meta.subs.iter().position(|(s, _)| *s == id).map(|k| (k + audio_tracks.len()) as u64 + 2);
    let base = picture_times(first, ft, None).map(|t| t.1).unwrap_or(0);

    // 2) Kopf schreiben
    let f = crate::dvd::create_output(dst)?;
    let mut out = Out { w: BufWriter::with_capacity(4 << 20, f), pos: 0 };
    let header = [
        uint(0x4286, 1),
        uint(0x42F7, 1),
        uint(0x42F2, 4),
        uint(0x42F3, 8),
        text(0x4282, "matroska"),
        uint(0x4287, 4),
        uint(0x4285, 2),
    ]
    .concat();
    out.put(&el(EBML, &header))?;
    // Segment mit Platzhalter für die Länge (acht Bytes)
    out.put(&id_bytes(SEGMENT))?;
    let seg_size_at = out.pos;
    out.put(&[0x01, 0, 0, 0, 0, 0, 0, 0])?;
    let seg_start = out.pos;

    // Info, Spuren und Kapitel vorab bauen, damit der Wegweiser ihre Lage kennt
    let info_body = [
        uint(0x2A_D7B1, 1_000_000),
        float(DURATION, 0.0),
        text(0x4D80, concat!("DVDmanager ", env!("CARGO_PKG_VERSION"))),
        text(0x5741, concat!("DVDmanager ", env!("CARGO_PKG_VERSION"))),
    ]
    .concat();
    let info = el(INFO, &info_body);

    let ratio = match v.aspect {
        2 => 4.0 / 3.0,
        3 => 16.0 / 9.0,
        4 => 2.21,
        _ => f64::from(v.width.max(1)) / f64::from(v.height.max(1)),
    };
    let video_el = [
        uint(0xB0, u64::from(v.width)),
        uint(0xBA, u64::from(v.height)),
        uint(0x54B0, (f64::from(v.height) * ratio).round() as u64),
        uint(0x54BA, u64::from(v.height)),
    ]
    .concat();
    let mut track_v = [
        uint(0xD7, 1),
        uint(0x73C5, 1),
        uint(0x83, 1),
        uint(0x9C, 0),
        text(0x86, "V_MPEG2"),
        uint(0x23_E383, (ft * 1e9 / 90_000.0).round() as u64),
        text(0x22_B59C, "und"),
    ]
    .concat();
    if !seq.is_empty() {
        track_v.extend(el(0x63A2, &seq));
    }
    track_v.extend(el(0xE0, &video_el));
    let mut tracks_body = el(0xAE, &track_v);
    for (k, (src, a)) in audio_tracks.iter().enumerate() {
        let n = k as u64 + 2;
        let lang = meta.audio_lang[usize::from(src & 7)].as_str();
        let mut audio_el = [float(0xB5, f64::from(a.rate)), uint(0x9F, u64::from(a.channels))].concat();
        if let Some(b) = a.bits {
            audio_el.extend(uint(0x6264, u64::from(b)));
        }
        let mut track_a = [uint(0xD7, n), uint(0x73C5, n), uint(0x83, 2), uint(0x9C, 0), uint(0x88, u64::from(k == 0)), text(0x86, a.codec)].concat();
        track_a.extend(lang_els(lang));
        if !lang.is_empty() {
            track_a.extend(text(0x536E, &crate::dvd::ifo::lang_name(lang)));
        }
        track_a.extend(el(0xE1, &audio_el));
        tracks_body.extend(el(0xAE, &track_a));
    }
    // Untertitel als VobSub: die Bilder bleiben, wie sie sind; Größe und Farben im Kopf.
    let palette = meta.palette.unwrap_or([0x000000, 0xFFFFFF, 0x808080, 0x000000, 0xFFFFFF, 0x808080, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    let idx_head = format!(
        "size: {}x{}\npalette: {}\n",
        v.width,
        v.height,
        palette.iter().map(|c| format!("{c:06x}")).collect::<Vec<_>>().join(", ")
    );
    for (id, lang) in &meta.subs {
        let Some(n) = sub_track_of(*id) else { continue };
        let mut track_s = [uint(0xD7, n), uint(0x73C5, n), uint(0x83, 0x11), uint(0x9C, 1), uint(0x88, 0), text(0x86, "S_VOBSUB")].concat();
        track_s.extend(lang_els(lang));
        if !lang.is_empty() {
            track_s.extend(text(0x536E, &crate::dvd::ifo::lang_name(lang)));
        }
        track_s.extend(el(0x63A2, idx_head.as_bytes()));
        tracks_body.extend(el(0xAE, &track_s));
    }
    let tracks = el(TRACKS, &tracks_body);

    let chapters_el = if chapters.is_empty() {
        Vec::new()
    } else {
        let mut ed = [uint(0x45BC, 1), uint(0x45DB, 1)].concat();
        let mut times: Vec<i64> = chapters.iter().map(|c| (c - base).max(0)).collect();
        times.sort_unstable();
        // Kapitel 1 beginnt immer am Anfang; zu dicht liegende Marken entfallen.
        times.insert(0, 0);
        times.dedup_by(|b, a| *b - *a < 90_000);
        for (k, t) in times.iter().enumerate() {
            let name = trf!("Kapitel {}", "Chapter {}", k + 1);
            let display = [text(0x85, &name), text(0x437C, "und")].concat();
            let atom = [
                uint(0x73C4, k as u64 + 1),
                uint(0x91, (*t as u64) * 100_000 / 9),
                uint(0x98, 0),
                uint(0x4598, 1),
                el(0x80, &display),
            ]
            .concat();
            ed.extend(el(0xB6, &atom));
        }
        el(CHAPTERS, &el(0x45B9, &ed))
    };

    // Wegweiser: Info, Spuren, Kapitel, Cues (Lage der Cues wird am Ende eingetragen).
    // Lagen haben feste acht Bytes, die Länge des Wegweisers steht also vorher fest.
    let seek = |id: u32, pos: u64| el(SEEK, &[el(SEEK_ID, &id_bytes(id)), uint8(SEEK_POS, pos)].concat());
    let mut ids = vec![INFO, TRACKS];
    if !chapters_el.is_empty() {
        ids.push(CHAPTERS);
    }
    ids.push(CUES);
    let head_len = el(SEEK_HEAD, &ids.iter().map(|id| seek(*id, 0)).collect::<Vec<_>>().concat()).len() as u64;
    let info_pos = head_len;
    let tracks_pos = info_pos + info.len() as u64;
    let chapters_pos = tracks_pos + tracks.len() as u64;
    let entries: Vec<Vec<u8>> = ids
        .iter()
        .map(|id| match *id {
            INFO => seek(INFO, info_pos),
            TRACKS => seek(TRACKS, tracks_pos),
            CHAPTERS => seek(CHAPTERS, chapters_pos),
            _ => seek(CUES, 0),
        })
        .collect();
    let head = el(SEEK_HEAD, &entries.concat());
    // Die Lage der Cues sind die letzten acht Bytes des Wegweisers
    let cues_pos_at = seg_start + head.len() as u64 - 8;
    out.put(&head)?;
    let duration_at = out.pos + (info.len() - info_body.len()) as u64 + uint(0x2A_D7B1, 1_000_000).len() as u64 + 3;
    out.put(&info)?;
    out.put(&tracks)?;
    out.put(&chapters_el)?;

    // 3) Gruppen und Ton in Clustern
    let mut d = Demux::new();
    let mut f = File::open(src).map_err(|e| format!("{}: {e}", src.display()))?;
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 4 << 20];
    let mut read = 0u64;
    let mut cues: Vec<(u64, u64)> = Vec::new();
    let mut last_end: Option<i64> = None;
    let mut end_time = 0i64;
    let mut pending: VecDeque<(Gop, Vec<i64>, i64, i64)> = VecDeque::new();
    loop {
        let n = f.read(&mut chunk).map_err(|e| e.to_string())?;
        read += n as u64;
        let eof = n == 0;
        buf.extend_from_slice(&chunk[..n]);
        let used = d.feed(&buf, eof);
        buf.drain(..used);
        while let Some(g) = d.gops.pop_front() {
            if g.pics.is_empty() {
                continue;
            }
            let Some((times, start, end)) = picture_times(&g, ft, last_end) else { continue };
            last_end = Some(end);
            pending.push_back((g, times, start, end));
        }
        // Eine Gruppe wird geschrieben, sobald der Ton bis zu ihrem Ende gelesen ist.
        while let Some((_, _, _, end)) = pending.front() {
            // Endet der Ton früher als das Bild, nicht unbegrenzt Gruppen sammeln.
            let audio_ready = eof
                || pending.len() > 60
                || d.audio.iter().filter(|a| audio_track_of(a.src).is_some()).all(|a| a.last_src.is_some_and(|t| t >= *end));
            if !audio_ready {
                break;
            }
            let (g, times, start, end) = pending.pop_front().unwrap();
            let last = eof && pending.is_empty();
            let cluster_ms = (start - base).max(0) / 90;
            let mut body = uint(0xE7, cluster_ms as u64);
            for (k, p) in g.pics.iter().enumerate() {
                let from = if k == 0 { 0 } else { p.0 };
                let to = g.pics.get(k + 1).map(|q| q.0).unwrap_or(g.data.len());
                let t = (times[k] - base) / 90;
                let flags = if p.2 == 1 { 0x80 } else if p.2 == 3 { 0x01 } else { 0x00 };
                body.extend(block(1, t - cluster_ms, flags, &g.data[from..to]));
            }
            for a in d.audio.iter_mut() {
                let Some(track) = audio_track_of(a.src) else {
                    // Spur ohne Eintrag im Kopf (erst später aufgetaucht): verwerfen
                    a.frames.clear();
                    continue;
                };
                let repack = audio_tracks.iter().find(|(s, _)| *s == a.src).and_then(|(_, f)| f.repack);
                let lpcm = matches!(a.kind, AKind::Lpcm { .. });
                // LPCM-Rahmen dauern nur 1/600 s; je 20 lückenlose kommen in einen Block.
                let mut group: Option<(i64, i64, Vec<u8>, usize)> = None;
                let emit = |body: &mut Vec<u8>, t: i64, payload: &[u8]| {
                    let rel = ((t - base) / 90 - cluster_ms).clamp(-32_768, 32_767);
                    body.extend(block(track as u8, rel, 0x80, payload));
                };
                while let Some((_, t)) = a.frames.front() {
                    if !last && *t >= end {
                        break;
                    }
                    let (data, t) = a.frames.pop_front().unwrap();
                    if t < base {
                        continue;
                    }
                    end_time = end_time.max(t + a.dur - base);
                    let payload = match repack {
                        Some((bits, ch)) => lpcm_to_24(&data, bits, ch),
                        None => data,
                    };
                    if !lpcm {
                        emit(&mut body, t, &payload);
                        continue;
                    }
                    match group.as_mut() {
                        Some((_, next, buf, n)) if (*next - t).abs() <= a.dur / 4 && *n < 20 => {
                            buf.extend_from_slice(&payload);
                            *next = t + a.dur;
                            *n += 1;
                        }
                        _ => {
                            if let Some((t0, _, buf, _)) = group.take() {
                                emit(&mut body, t0, &buf);
                            }
                            group = Some((t, t + a.dur, payload, 1));
                        }
                    }
                }
                if let Some((t0, _, buf, _)) = group.take() {
                    emit(&mut body, t0, &buf);
                }
            }
            // Untertitel bis zum Ende dieser Gruppe, mit ihrer Anzeigedauer
            for (id, s) in d.subs.iter_mut() {
                let track = sub_track_of(*id);
                while let Some((t, _)) = s.done.front() {
                    if !last && *t >= end {
                        break;
                    }
                    let (t, spu) = s.done.pop_front().unwrap();
                    let Some(track) = track.filter(|_| t >= base) else { continue };
                    let rel = ((t - base) / 90 - cluster_ms).clamp(-32_768, 32_767);
                    let mut group = block_body(track as u8, rel, 0, &spu);
                    group = el(0xA1, &group);
                    if let Some(ms) = spu_duration_ms(&spu) {
                        group.extend(uint(0x9B, ms));
                    }
                    body.extend(el(0xA0, &group));
                }
            }
            end_time = end_time.max(end - base);
            cues.push((cluster_ms as u64, out.pos - seg_start));
            out.put(&el(CLUSTER, &body))?;
        }
        if !progress(read as f32 / total as f32) {
            return Err(crate::engine::backup::CANCELLED.to_string());
        }
        if eof {
            break;
        }
    }

    // 4) Cues, dann Längen und Lagen nachtragen
    let cues_pos = out.pos - seg_start;
    let mut cue_body = Vec::new();
    for (t, pos) in &cues {
        let tp = [uint(0xF7, 1), uint(0xF1, *pos)].concat();
        cue_body.extend(el(0xBB, &[uint(0xB3, *t), el(0xB7, &tp)].concat()));
    }
    out.put(&el(CUES, &cue_body))?;
    let seg_len = out.pos - seg_start;
    out.w.flush().map_err(|e| e.to_string())?;
    let mut file = out.w.into_inner().map_err(|e| e.to_string())?;
    let mut patch = |at: u64, b: &[u8]| -> Res<()> {
        file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
        file.write_all(b).map_err(|e| e.to_string())
    };
    let mut seg = seg_len.to_be_bytes();
    seg[0] = 0x01;
    patch(seg_size_at, &seg)?;
    patch(cues_pos_at, &cues_pos.to_be_bytes())?;
    patch(duration_at, &(end_time as f64 / 90.0).to_be_bytes())?;
    file.flush().map_err(|e| e.to_string())?;
    Ok(seg_start + seg_len)
}

/// Einfacher Block: Spur, Zeit relativ zum Cluster (ms), Flags, Daten.
fn block(track: u8, rel_ms: i64, flags: u8, data: &[u8]) -> Vec<u8> {
    el(0xA3, &block_body(track, rel_ms, flags, data))
}

/// Inhalt eines Blocks (ohne Kennung): Spur, Zeit, Flags, Daten.
fn block_body(track: u8, rel_ms: i64, flags: u8, data: &[u8]) -> Vec<u8> {
    let mut body = Vec::with_capacity(data.len() + 4);
    body.push(0x80 | track);
    body.extend_from_slice(&(rel_ms.clamp(-32_768, 32_767) as i16).to_be_bytes());
    body.push(flags);
    body.extend_from_slice(data);
    body
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ebml_numbers() {
        assert_eq!(vint(0), vec![0x80]);
        assert_eq!(vint(126), vec![0xFE]);
        assert_eq!(vint(127), vec![0x40, 0x7F]);
        assert_eq!(vint(300), vec![0x41, 0x2C]);
        assert_eq!(uint(0x4286, 1), vec![0x42, 0x86, 0x81, 0x01]);
        assert_eq!(id_bytes(0xA3), vec![0xA3]);
        assert_eq!(id_bytes(SEGMENT), vec![0x18, 0x53, 0x80, 0x67]);
    }

    #[test]
    fn lpcm_24_bit_is_unpacked() {
        // Stereo, zwei Abtastwerte: oben 1122 3344 5566 7788, unten AA BB CC DD
        let g = [0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0xAA, 0xBB, 0xCC, 0xDD];
        let out = lpcm_to_24(&g, 24, 2);
        assert_eq!(out, vec![0x11, 0x22, 0xAA, 0x33, 0x44, 0xBB, 0x55, 0x66, 0xCC, 0x77, 0x88, 0xDD]);
    }
}
