//! Lesen und Anpassen der IFO-Tabellen einer Video-DVD.

use super::{be16, be32, put32, Res, SECTOR};

#[derive(Clone, Copy, Debug, Default)]
pub struct DvdTime {
    pub ms: u64,
    pub fps: f32,
}

fn bcd(b: u8) -> u64 {
    u64::from(b >> 4) * 10 + u64::from(b & 15)
}

pub fn parse_time(d: &[u8], o: usize) -> DvdTime {
    let Some(b) = d.get(o..o + 4) else { return DvdTime::default() };
    let fps = match b[3] >> 6 {
        1 => 25.0,
        3 => 29.97,
        _ => 25.0,
    };
    let frames = bcd(b[3] & 0x3F) as f64;
    let ms = (bcd(b[0]) * 3600 + bcd(b[1]) * 60 + bcd(b[2])) * 1000 + (frames * 1000.0 / f64::from(fps)) as u64;
    DvdTime { ms, fps }
}

fn to_bcd(v: u32) -> u8 {
    (((v / 10) << 4) | (v % 10)) as u8
}

pub fn encode_time(ms: u64, pal: bool) -> [u8; 4] {
    let secs = ms / 1000;
    let fps = if pal { 25.0 } else { 29.97 };
    let frames = (((ms % 1000) as f64) * fps / 1000.0) as u32;
    [
        to_bcd((secs / 3600) as u32),
        to_bcd(((secs / 60) % 60) as u32),
        to_bcd((secs % 60) as u32),
        to_bcd(frames) | if pal { 0x40 } else { 0xC0 },
    ]
}

#[derive(Clone, Debug, Default)]
pub struct AudioAttr {
    pub coding: u8,
    pub channels: u8,
    pub lang: String,
    pub ext: u8,
    pub sample_rate_khz: u16,
}

impl AudioAttr {
    pub fn parse(d: &[u8]) -> Self {
        let coding = d[0] >> 5;
        let lang_type = (d[0] >> 2) & 3;
        let lang = if lang_type == 1 { lang_code(d[2], d[3]) } else { String::new() };
        let rate = if (d[1] >> 4) & 3 == 1 { 96 } else { 48 };
        Self { coding, channels: (d[1] & 7) + 1, lang, ext: d[5], sample_rate_khz: rate }
    }

    pub fn codec_name(&self) -> &'static str {
        match self.coding {
            0 => "Dolby Digital",
            2 => "MPEG",
            3 => "MPEG-2 ext",
            4 => "LPCM",
            6 => "DTS",
            _ => "Audio",
        }
    }

    pub fn ext_name(&self) -> &'static str {
        match self.ext {
            1 => "",
            2 => tr!("für Sehbehinderte", "for the visually impaired"),
            3 => tr!("Regiekommentar", "Director's commentary"),
            4 => tr!("Regiekommentar", "Director's commentary"),
            _ => "",
        }
    }

    pub fn channels_name(&self) -> String {
        match self.channels {
            1 => "Mono".into(),
            2 => "Stereo".into(),
            6 => "5.1".into(),
            7 => "6.1".into(),
            8 => "7.1".into(),
            n => trf!("{n} Kanäle", "{n} channels"),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct SubAttr {
    pub lang: String,
    pub ext: u8,
}

impl SubAttr {
    pub fn parse(d: &[u8]) -> Self {
        let lang = if d[0] & 3 == 1 { lang_code(d[2], d[3]) } else { String::new() };
        Self { lang, ext: d[5] }
    }

    pub fn ext_name(&self) -> &'static str {
        match self.ext {
            2 | 3 => tr!("groß", "large"),
            5 | 6 | 7 => tr!("Hörgeschädigte", "hearing impaired"),
            9 | 10 | 11 => "erzwungen",
            13 | 14 | 15 => tr!("Kommentar", "Commentary"),
            _ => "",
        }
    }
}

fn lang_code(a: u8, b: u8) -> String {
    if a.is_ascii_alphabetic() && b.is_ascii_alphabetic() {
        format!("{}{}", a as char, b as char).to_lowercase()
    } else {
        String::new()
    }
}

/// Anzeigename einer Sprache.
pub fn lang_name(code: &str) -> String {
    let n = match code {
        "de" => tr!("Deutsch", "German"),
        "en" => tr!("Englisch", "English"),
        "fr" => tr!("Französisch", "French"),
        "es" => tr!("Spanisch", "Spanish"),
        "it" => tr!("Italienisch", "Italian"),
        "nl" => tr!("Niederländisch", "Dutch"),
        "pt" => tr!("Portugiesisch", "Portuguese"),
        "pl" => tr!("Polnisch", "Polish"),
        "ru" => tr!("Russisch", "Russian"),
        "tr" => tr!("Türkisch", "Turkish"),
        "cs" => tr!("Tschechisch", "Czech"),
        "hu" => tr!("Ungarisch", "Hungarian"),
        "sv" => tr!("Schwedisch", "Swedish"),
        "da" => tr!("Dänisch", "Danish"),
        "no" => tr!("Norwegisch", "Norwegian"),
        "fi" => tr!("Finnisch", "Finnish"),
        "el" => tr!("Griechisch", "Greek"),
        "ja" => tr!("Japanisch", "Japanese"),
        "ko" => tr!("Koreanisch", "Korean"),
        "zh" => tr!("Chinesisch", "Chinese"),
        "ar" => tr!("Arabisch", "Arabic"),
        "he" | "iw" => tr!("Hebräisch", "Hebrew"),
        "hi" => "Hindi",
        "th" => "Thai",
        "" => tr!("Unbekannt", "Unknown"),
        other => return other.to_uppercase(),
    };
    n.to_string()
}

#[derive(Clone, Copy, Debug, Default)]
pub struct VideoAttr {
    pub pal: bool,
    pub wide: bool,
    pub width: u32,
    pub height: u32,
    pub mpeg1: bool,
}

impl VideoAttr {
    pub fn parse(d: &[u8]) -> Self {
        let b0 = d[0];
        let b1 = d[1];
        let pal = (b0 >> 4) & 3 == 1;
        let wide = (b0 >> 2) & 3 == 3;
        let height = if pal { 576 } else { 480 };
        let size = (b1 >> 2) & 3;
        let width = match size {
            0 => 720,
            1 => 704,
            _ => 352,
        };
        let h = if size == 3 { height / 2 } else { height };
        Self { pal, wide, width, height: h, mpeg1: b0 >> 6 == 0 }
    }

    pub fn describe(&self) -> String {
        format!(
            "{} {}×{} {}",
            if self.pal { "PAL" } else { "NTSC" },
            self.width,
            self.height,
            if self.wide { "16:9" } else { "4:3" }
        )
    }
}

#[derive(Clone, Copy, Debug, Default)]
pub struct CellPlay {
    pub first_sector: u32,
    pub first_ilvu_end: u32,
    pub last_vobu_start: u32,
    pub last_sector: u32,
    pub ms: u64,
    pub interleaved: bool,
    pub block_mode: u8,
    pub vob_id: u16,
    pub cell_id: u8,
}

#[derive(Clone, Debug, Default)]
pub struct Pgc {
    pub offset: usize,
    pub ms: u64,
    pub fps: f32,
    pub audio_ctl: [u16; 8],
    pub sub_ctl: [u32; 32],
    pub program_map: Vec<u8>,
    pub cells: Vec<CellPlay>,
    pub next_pgc: u16,
    /// Farbtabelle der Untertitel: 16 Einträge 0x00YYCrCb
    pub palette: [u32; 16],
}

/// Farbe der Untertitel-Palette (0x00YYCrCb) als RGB (0xRRGGBB).
pub fn ycrcb_to_rgb(c: u32) -> u32 {
    let y = f64::from((c >> 16) & 0xFF);
    let cr = f64::from((c >> 8) & 0xFF) - 128.0;
    let cb = f64::from(c & 0xFF) - 128.0;
    let y = 1.164 * (y - 16.0);
    let clip = |v: f64| v.round().clamp(0.0, 255.0) as u32;
    let r = clip(y + 1.596 * cr);
    let g = clip(y - 0.813 * cr - 0.391 * cb);
    let b = clip(y + 2.018 * cb);
    (r << 16) | (g << 8) | b
}

pub fn parse_pgc(d: &[u8], off: usize) -> Option<Pgc> {
    if off + 0xEC > d.len() {
        return None;
    }
    let nr_programs = d[off + 2] as usize;
    let nr_cells = d[off + 3] as usize;
    let time = parse_time(d, off + 4);
    let mut p = Pgc { offset: off, ms: time.ms, fps: time.fps, ..Default::default() };
    for i in 0..8 {
        p.audio_ctl[i] = be16(d, off + 0x0C + i * 2);
    }
    for i in 0..32 {
        p.sub_ctl[i] = be32(d, off + 0x1C + i * 4);
    }
    p.next_pgc = be16(d, off + 0x9C);
    for i in 0..16 {
        p.palette[i] = be32(d, off + 0xA4 + i * 4);
    }
    let pm = be16(d, off + 0xE6) as usize;
    let cp = be16(d, off + 0xE8) as usize;
    let cs = be16(d, off + 0xEA) as usize;
    if pm != 0 {
        for i in 0..nr_programs {
            p.program_map.push(*d.get(off + pm + i)?);
        }
    }
    if cp != 0 && cs != 0 {
        for i in 0..nr_cells {
            let c = off + cp + i * 24;
            let b = off + cs + i * 4;
            if c + 24 > d.len() || b + 4 > d.len() {
                return None;
            }
            let t = parse_time(d, c + 4);
            p.cells.push(CellPlay {
                first_sector: be32(d, c + 8),
                first_ilvu_end: be32(d, c + 12),
                last_vobu_start: be32(d, c + 16),
                last_sector: be32(d, c + 20),
                ms: t.ms,
                interleaved: d[c] & 0x04 != 0,
                block_mode: d[c] >> 6,
                vob_id: be16(d, b),
                cell_id: d[b + 3],
            });
        }
    }
    Some(p)
}

/// Eintrag der Zellen-Adresstabelle.
#[derive(Clone, Copy, Debug)]
pub struct CellAddr {
    pub vob_id: u16,
    pub cell_id: u8,
    pub start: u32,
    pub last: u32,
}

fn parse_cadt(d: &[u8], sector: u32) -> Vec<CellAddr> {
    let mut out = Vec::new();
    if sector == 0 {
        return out;
    }
    let o = sector as usize * SECTOR;
    if o + 8 > d.len() {
        return out;
    }
    let last = be32(d, o + 4) as usize;
    let n = last.saturating_sub(7) / 12;
    for i in 0..n {
        let e = o + 8 + i * 12;
        if e + 12 > d.len() {
            break;
        }
        out.push(CellAddr {
            vob_id: be16(d, e),
            cell_id: d[e + 2],
            start: be32(d, e + 4),
            last: be32(d, e + 8),
        });
    }
    out
}

fn parse_admap(d: &[u8], sector: u32) -> Vec<u32> {
    let mut out = Vec::new();
    if sector == 0 {
        return out;
    }
    let o = sector as usize * SECTOR;
    if o + 4 > d.len() {
        return out;
    }
    let last = be32(d, o) as usize;
    let n = last.saturating_sub(3) / 4;
    out.reserve(n);
    for i in 0..n {
        let e = o + 4 + i * 4;
        if e + 4 > d.len() {
            break;
        }
        out.push(be32(d, e));
    }
    out
}

/// Alle Programmketten einer PGCIT-Tabelle ab Byte-Offset `base`.
fn parse_pgcit(d: &[u8], base: usize) -> Vec<Pgc> {
    let mut out = Vec::new();
    if base + 8 > d.len() {
        return out;
    }
    let n = be16(d, base) as usize;
    for i in 0..n {
        let e = base + 8 + i * 8;
        let off = be32(d, e + 4) as usize;
        if let Some(p) = parse_pgc(d, base + off) {
            out.push(p);
        } else {
            out.push(Pgc::default());
        }
    }
    out
}

/// Byte-Offsets aller PGCs in einer Menü-Sprachentabelle.
fn pgci_ut_offsets(d: &[u8], sector: u32) -> Vec<usize> {
    let mut out = Vec::new();
    if sector == 0 {
        return out;
    }
    let base = sector as usize * SECTOR;
    if base + 8 > d.len() {
        return out;
    }
    let lus = be16(d, base) as usize;
    for i in 0..lus {
        let lu = base + be32(d, base + 8 + i * 8 + 4) as usize;
        if lu + 8 > d.len() {
            continue;
        }
        let n = be16(d, lu) as usize;
        for j in 0..n {
            let off = lu + be32(d, lu + 8 + j * 8 + 4) as usize;
            if off < d.len() && !out.contains(&off) {
                out.push(off);
            }
        }
    }
    out
}

fn parse_menu_pgcs(d: &[u8], sector: u32) -> Vec<Pgc> {
    pgci_ut_offsets(d, sector).into_iter().filter_map(|o| parse_pgc(d, o)).collect()
}

#[derive(Clone, Debug, Default)]
pub struct TitleEntry {
    pub playback_type: u8,
    pub angles: u8,
    pub chapters: u16,
    pub vts: u8,
    pub vts_ttn: u8,
    pub vts_sector: u32,
}

/// Inhalt von VIDEO_TS.IFO.
#[derive(Clone, Debug, Default)]
pub struct Vmg {
    pub raw: Vec<u8>,
    pub nr_vts: u16,
    pub region_mask: u8,
    pub titles: Vec<TitleEntry>,
    pub tt_srpt_sector: u32,
    pub cadt_sector: u32,
    pub admap_sector: u32,
    pub pgci_ut_sector: u32,
    pub first_play_offset: u32,
    pub menu_vob_start: u32,
    pub menu_cells: Vec<CellAddr>,
    pub menu_admap: Vec<u32>,
    pub menu_pgcs: Vec<Pgc>,
    pub first_play: Option<Pgc>,
    pub video_menu: VideoAttr,
}

impl Vmg {
    pub fn parse(raw: Vec<u8>) -> Res<Self> {
        if raw.len() < SECTOR || &raw[0..12] != b"DVDVIDEO-VMG" {
            return Err(tr!("VIDEO_TS.IFO ist beschädigt oder keine DVD-Datei.", "VIDEO_TS.IFO is damaged or not a DVD file.").into());
        }
        let tt = be32(&raw, 0xC4);
        let mut titles = Vec::new();
        let base = tt as usize * SECTOR;
        if base + 8 <= raw.len() {
            let n = be16(&raw, base) as usize;
            for i in 0..n {
                let e = base + 8 + i * 12;
                if e + 12 > raw.len() {
                    break;
                }
                titles.push(TitleEntry {
                    playback_type: raw[e],
                    angles: raw[e + 1],
                    chapters: be16(&raw, e + 2),
                    vts: raw[e + 6],
                    vts_ttn: raw[e + 7],
                    vts_sector: be32(&raw, e + 8),
                });
            }
        }
        let first_play_offset = be32(&raw, 0x84);
        let first_play = if first_play_offset > 0 { parse_pgc(&raw, first_play_offset as usize) } else { None };
        let cadt_sector = be32(&raw, 0xD8);
        let admap_sector = be32(&raw, 0xDC);
        let pgci_ut_sector = be32(&raw, 0xC8);
        Ok(Self {
            nr_vts: be16(&raw, 0x3E),
            region_mask: raw[0x23],
            tt_srpt_sector: tt,
            cadt_sector,
            admap_sector,
            pgci_ut_sector,
            first_play_offset,
            menu_vob_start: be32(&raw, 0xC0),
            menu_cells: parse_cadt(&raw, cadt_sector),
            menu_admap: parse_admap(&raw, admap_sector),
            menu_pgcs: parse_menu_pgcs(&raw, pgci_ut_sector),
            first_play,
            video_menu: VideoAttr::parse(&raw[0x100..0x102]),
            titles,
            raw,
        })
    }
}

/// Inhalt von VTS_xx_0.IFO.
#[derive(Clone, Debug, Default)]
pub struct Vts {
    pub raw: Vec<u8>,
    pub vts: u8,
    pub video: VideoAttr,
    pub audio: Vec<AudioAttr>,
    pub subs: Vec<SubAttr>,
    pub pgcs: Vec<Pgc>,
    pub menu_pgcs: Vec<Pgc>,
    /// Kapitel je Titel: (PGC-Nummer, Programmnummer), beide ab 1 gezählt.
    pub ptt: Vec<Vec<(u16, u16)>>,
    pub cells: Vec<CellAddr>,
    pub menu_cells: Vec<CellAddr>,
    pub admap: Vec<u32>,
    pub menu_admap: Vec<u32>,
}

impl Vts {
    pub fn parse(vts: u8, raw: Vec<u8>) -> Res<Self> {
        if raw.len() < SECTOR || &raw[0..12] != b"DVDVIDEO-VTS" {
            return Err(trf!("Die Tabelle von Titelset {vts} ist beschädigt.", "The table of title set {vts} is damaged."));
        }
        let video = VideoAttr::parse(&raw[0x200..0x202]);
        let nr_audio = (be16(&raw, 0x202) as usize).min(8);
        let audio = (0..nr_audio).map(|i| AudioAttr::parse(&raw[0x204 + i * 8..0x204 + i * 8 + 8])).collect();
        let nr_sub = (be16(&raw, 0x254) as usize).min(32);
        let subs = (0..nr_sub).map(|i| SubAttr::parse(&raw[0x256 + i * 6..0x256 + i * 6 + 6])).collect();

        let pgcit = be32(&raw, 0xCC) as usize * SECTOR;
        let pgcs = parse_pgcit(&raw, pgcit);

        let mut ptt = Vec::new();
        let ptt_base = be32(&raw, 0xC8) as usize * SECTOR;
        if ptt_base + 8 <= raw.len() {
            let n = be16(&raw, ptt_base) as usize;
            let end = be32(&raw, ptt_base + 4) as usize + 1;
            let offs: Vec<usize> = (0..n).map(|i| be32(&raw, ptt_base + 8 + i * 4) as usize).collect();
            for i in 0..n {
                let start = offs[i];
                let stop = if i + 1 < n { offs[i + 1] } else { end };
                let mut chapters = Vec::new();
                let mut p = start;
                while p + 4 <= stop {
                    let at = ptt_base + p;
                    if at + 4 > raw.len() {
                        break;
                    }
                    chapters.push((be16(&raw, at), be16(&raw, at + 2)));
                    p += 4;
                }
                ptt.push(chapters);
            }
        }

        Ok(Self {
            vts,
            video,
            audio,
            subs,
            pgcs,
            menu_pgcs: parse_menu_pgcs(&raw, be32(&raw, 0xD0)),
            ptt,
            cells: parse_cadt(&raw, be32(&raw, 0xE0)),
            menu_cells: parse_cadt(&raw, be32(&raw, 0xD8)),
            admap: parse_admap(&raw, be32(&raw, 0xE4)),
            menu_admap: parse_admap(&raw, be32(&raw, 0xDC)),
            raw,
        })
    }
}

/// Abbildung alter auf neue Sektoren einer VOB-Zone.
#[derive(Clone, Debug, Default)]
pub struct SectorMap {
    /// (alter Start, altes Ende, neuer Start, neues Ende) je VOBU, aufsteigend.
    pub vobus: Vec<(u32, u32, u32, u32)>,
    pub new_total: u32,
}

impl SectorMap {
    fn find(&self, old: u32) -> Option<usize> {
        if self.vobus.is_empty() {
            return None;
        }
        let i = self.vobus.partition_point(|v| v.0 <= old);
        if i == 0 {
            return Some(0);
        }
        Some(i - 1)
    }

    pub fn map_start(&self, old: u32) -> u32 {
        match self.find(old) {
            Some(i) => self.vobus[i].2,
            None => 0,
        }
    }

    pub fn map_end(&self, old: u32) -> u32 {
        match self.find(old) {
            Some(i) => self.vobus[i].3,
            None => 0,
        }
    }

    pub fn contains(&self, old: u32) -> bool {
        self.find(old).map(|i| old >= self.vobus[i].0 && old <= self.vobus[i].1).unwrap_or(false)
    }
}

fn patch_cadt(d: &mut [u8], sector: u32, map: &SectorMap) {
    if sector == 0 {
        return;
    }
    let o = sector as usize * SECTOR;
    if o + 8 > d.len() {
        return;
    }
    let n = (be32(d, o + 4) as usize).saturating_sub(7) / 12;
    for i in 0..n {
        let e = o + 8 + i * 12;
        if e + 12 > d.len() {
            break;
        }
        let (s, l) = (be32(d, e + 4), be32(d, e + 8));
        put32(d, e + 4, map.map_start(s));
        put32(d, e + 8, map.map_end(l));
    }
}

fn patch_admap(d: &mut [u8], sector: u32, map: &SectorMap) {
    if sector == 0 {
        return;
    }
    let o = sector as usize * SECTOR;
    if o + 4 > d.len() {
        return;
    }
    let n = (be32(d, o) as usize).saturating_sub(3) / 4;
    for i in 0..n {
        let e = o + 4 + i * 4;
        if e + 4 > d.len() {
            break;
        }
        let v = be32(d, e);
        put32(d, e, map.map_start(v));
    }
}

/// Passt Zelladressen und Ton-/Untertitelfreigabe in einer PGC an.
fn patch_pgc(d: &mut [u8], off: usize, map: &SectorMap, remove_puo: bool) {
    if off + 0xEC > d.len() {
        return;
    }
    let nr_cells = d[off + 3] as usize;
    let cp = be16(d, off + 0xE8) as usize;
    if remove_puo {
        put32(d, off + 8, 0);
    }
    if cp == 0 {
        return;
    }
    for i in 0..nr_cells {
        let c = off + cp + i * 24;
        if c + 24 > d.len() {
            break;
        }
        let (a, b, e, l) = (be32(d, c + 8), be32(d, c + 12), be32(d, c + 16), be32(d, c + 20));
        put32(d, c + 8, map.map_start(a));
        if b != 0 {
            put32(d, c + 12, map.map_end(b));
        }
        put32(d, c + 16, map.map_start(e));
        put32(d, c + 20, map.map_end(l));
    }
}

/// Schaltet Ton- und Untertitelspuren einer PGC ab, die nicht mehr vorhanden sind.
pub fn disable_streams(d: &mut [u8], off: usize, keep_audio: &[bool; 8], keep_sub: &[bool; 32]) {
    if off + 0xEC > d.len() {
        return;
    }
    for i in 0..8 {
        let at = off + 0x0C + i * 2;
        let v = be16(d, at);
        if v & 0x8000 != 0 {
            let phys = ((v >> 8) & 7) as usize;
            if !keep_audio[phys] {
                super::put16(d, at, 0);
            }
        }
    }
    for i in 0..32 {
        let at = off + 0x1C + i * 4;
        let v = be32(d, at);
        if v & 0x8000_0000 != 0 {
            let phys = [
                ((v >> 24) & 0x1F) as usize,
                ((v >> 16) & 0x1F) as usize,
                ((v >> 8) & 0x1F) as usize,
                (v & 0x1F) as usize,
            ];
            if !phys.iter().any(|&p| keep_sub[p]) {
                put32(d, at, 0);
            }
        }
    }
}

/// Größe einer IFO in Sektoren.
pub fn ifo_sectors(raw: &[u8]) -> u32 {
    raw.len().div_ceil(SECTOR) as u32
}

/// Ergänzt VTS-IFO um neue Größen und Adressen (Ganze-DVD-Modus).
pub fn patch_vts(
    d: &mut [u8],
    menu: &SectorMap,
    title: &SectorMap,
    remove_puo: bool,
) {
    // Menü
    let cadt_m = be32(d, 0xD8);
    let admap_m = be32(d, 0xDC);
    let pgci_ut = be32(d, 0xD0);
    patch_cadt(d, cadt_m, menu);
    patch_admap(d, admap_m, menu);
    for o in pgci_ut_offsets(d, pgci_ut) {
        patch_pgc(d, o, menu, remove_puo);
    }

    // Titel
    let cadt = be32(d, 0xE0);
    let admap = be32(d, 0xE4);
    patch_cadt(d, cadt, title);
    patch_admap(d, admap, title);
    let pgcit = be32(d, 0xCC) as usize * SECTOR;
    if pgcit + 8 <= d.len() {
        let n = be16(d, pgcit) as usize;
        for i in 0..n {
            let off = pgcit + be32(d, pgcit + 8 + i * 8 + 4) as usize;
            patch_pgc(d, off, title, remove_puo);
        }
    }

    // Zeitkarten
    let tmap = be32(d, 0xD4) as usize * SECTOR;
    if tmap != 0 && tmap + 8 <= d.len() {
        let n = be16(d, tmap) as usize;
        for i in 0..n {
            let off = tmap + be32(d, tmap + 8 + i * 4) as usize;
            if off + 4 > d.len() {
                continue;
            }
            let entries = be16(d, off + 2) as usize;
            for k in 0..entries {
                let e = off + 4 + k * 4;
                if e + 4 > d.len() {
                    break;
                }
                let v = be32(d, e);
                let flag = v & 0x8000_0000;
                put32(d, e, flag | title.map_start(v & 0x7FFF_FFFF));
            }
        }
    }

    // Lage der Zonen
    let ifo = ifo_sectors(d);
    let menu_size = menu.new_total;
    let title_size = title.new_total;
    put32(d, 0xC0, if menu_size > 0 { ifo } else { 0 });
    put32(d, 0xC4, if title_size > 0 { ifo + menu_size } else { 0 });
    put32(d, 0x0C, ifo * 2 + menu_size + title_size - 1);
    put32(d, 0x1C, ifo - 1);
}

/// Ergänzt die VMG-IFO (Ganze-DVD-Modus).
pub fn patch_vmg(d: &mut [u8], menu: &SectorMap, vts_starts: &[u32], remove_puo: bool, region_free: bool) {
    let cadt_m = be32(d, 0xD8);
    let admap_m = be32(d, 0xDC);
    let pgci_ut = be32(d, 0xC8);
    patch_cadt(d, cadt_m, menu);
    patch_admap(d, admap_m, menu);
    for o in pgci_ut_offsets(d, pgci_ut) {
        patch_pgc(d, o, menu, remove_puo);
    }
    let fp = be32(d, 0x84) as usize;
    if remove_puo && fp != 0 {
        put32(d, fp + 8, 0);
    }
    let ifo = ifo_sectors(d);
    put32(d, 0xC0, if menu.new_total > 0 { ifo } else { 0 });
    put32(d, 0x0C, ifo * 2 + menu.new_total - 1);
    put32(d, 0x1C, ifo - 1);
    if region_free {
        d[0x23] = 0;
    }
    // Titelsuchtabelle
    let tt = be32(d, 0xC4) as usize * SECTOR;
    if tt + 8 <= d.len() {
        let n = be16(d, tt) as usize;
        for i in 0..n {
            let e = tt + 8 + i * 12;
            if e + 12 > d.len() {
                break;
            }
            let vts = d[e + 6] as usize;
            if vts >= 1 && vts <= vts_starts.len() {
                put32(d, e + 8, vts_starts[vts - 1]);
            }
            if remove_puo {
                d[e] &= !0x03;
            }
        }
    }
}

/// Liefert alle Byte-Offsets der Titel-PGCs einer VTS.
pub fn title_pgc_offsets(d: &[u8]) -> Vec<usize> {
    let pgcit = be32(d, 0xCC) as usize * SECTOR;
    let mut out = Vec::new();
    if pgcit + 8 <= d.len() {
        let n = be16(d, pgcit) as usize;
        for i in 0..n {
            out.push(pgcit + be32(d, pgcit + 8 + i * 8 + 4) as usize);
        }
    }
    out
}
