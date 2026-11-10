//! Das Herzstück: wandelt VOBU für VOBU um, führt Buch über die neuen Sektoren
//! und korrigiert anschließend alle Verweise in den Navigationspaketen.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use crate::dvd::ifo::SectorMap;
use crate::dvd::source::SpaceReader;
use crate::dvd::vob::*;
use crate::dvd::{be16, be32, put16, put32, Res, SECTOR};
use crate::mpeg2::headers::{PIC_B, STRUCT_FRAME};
use crate::mpeg2::requant::{EsOut, PicRec, Requant, VideoOpts};

use super::plan::CellDecision;

/// Maximale VOB-Teilgröße: 1 GiB minus ein Sektor.
pub const VOB_PART_SECTORS: u64 = 524_287;
const SCR_TICK: u64 = 43_886;

/// Liefert VOBUs (NAV-Paket bis vor das nächste NAV-Paket) aus einem Bereich.
pub struct VobuReader {
    reader: SpaceReader,
    pos: u64,
    end: u64,
    chunk: Vec<u8>,
    chunk_start: u64,
    chunk_count: u64,
}

impl VobuReader {
    pub fn new(reader: SpaceReader, start: u64, end: u64) -> Self {
        let end = end.min(reader.sectors());
        Self { reader, pos: start, end, chunk: Vec::new(), chunk_start: 0, chunk_count: 0 }
    }

    pub fn seek(&mut self, start: u64, end: u64) {
        self.pos = start;
        self.end = end.min(self.reader.sectors());
    }

    pub fn position(&self) -> u64 {
        self.pos
    }

    fn sector(&mut self, n: u64) -> Res<&[u8]> {
        if n < self.chunk_start || n >= self.chunk_start + self.chunk_count {
            let want = (self.end.saturating_sub(n)).clamp(1, 512) as usize;
            let got = self.reader.read(n, want, &mut self.chunk)?;
            self.chunk_start = n;
            self.chunk_count = got as u64;
            if got == 0 {
                return Err(tr!("Unerwartetes Ende der VOB-Daten", "Unexpected end of VOB data").into());
            }
        }
        let o = ((n - self.chunk_start) as usize) * SECTOR;
        Ok(&self.chunk[o..o + SECTOR])
    }

    /// Nächste VOBU als (Startsektor, Daten). Ungültige Sektoren davor werden übersprungen.
    pub fn next(&mut self) -> Res<Option<(u64, Vec<u8>)>> {
        while self.pos < self.end {
            let p = self.pos;
            if is_nav(self.sector(p)?) {
                break;
            }
            self.pos += 1;
        }
        if self.pos >= self.end {
            return Ok(None);
        }
        let start = self.pos;
        let mut data = Vec::with_capacity(256 * SECTOR);
        data.extend_from_slice(self.sector(start)?);
        self.pos += 1;
        while self.pos < self.end {
            let p = self.pos;
            let s = self.sector(p)?;
            if is_nav(s) {
                break;
            }
            data.extend_from_slice(s);
            self.pos += 1;
        }
        Ok(Some((start, data)))
    }
}

/// Schreibt einen Sektorraum in eine oder mehrere VOB-Dateien.
pub struct SpaceWriter {
    dir: PathBuf,
    base: String,
    /// Menü-VOBs bestehen immer aus nur einem Teil.
    single: bool,
    split: bool,
    part_no: u32,
    file: Option<File>,
    in_part: u64,
    total: u64,
    paths: Vec<PathBuf>,
    part_sizes: Vec<u64>,
}

impl SpaceWriter {
    /// `base` z. B. "VTS_01_"; bei `single` ist der ganze Name fest.
    pub fn new(dir: &Path, base: &str, single: bool, split: bool) -> Self {
        Self {
            dir: dir.to_path_buf(),
            base: base.to_string(),
            single,
            split,
            part_no: 0,
            file: None,
            in_part: 0,
            total: 0,
            paths: Vec::new(),
            part_sizes: Vec::new(),
        }
    }

    pub fn total(&self) -> u64 {
        self.total
    }

    pub fn paths(&self) -> &[PathBuf] {
        &self.paths
    }

    fn open_next(&mut self) -> Res<()> {
        if let Some(f) = self.file.as_mut() {
            f.flush().map_err(|e| e.to_string())?;
        }
        self.part_no += 1;
        let name = if self.single { self.base.clone() } else { format!("{}{}.VOB", self.base, self.part_no) };
        let path = self.dir.join(&name);
        let f = File::create(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        self.file = Some(f);
        self.paths.push(path);
        self.part_sizes.push(0);
        self.in_part = 0;
        Ok(())
    }

    pub fn write(&mut self, sectors: &[u8]) -> Res<()> {
        let mut rest = sectors;
        while !rest.is_empty() {
            let cap = if self.single || !self.split { u64::MAX } else { VOB_PART_SECTORS };
            if self.file.is_none() || self.in_part >= cap {
                if self.part_no >= 9 && !self.single {
                    return Err(tr!("Die Ausgabe ist für neun VOB-Teile zu groß.", "The output is too large for nine VOB parts.").into());
                }
                self.open_next()?;
            }
            let avail = cap - self.in_part;
            let n = ((rest.len() / SECTOR) as u64).min(avail) as usize;
            let (a, b) = rest.split_at(n * SECTOR);
            self.file.as_mut().unwrap().write_all(a).map_err(|e| {
                if e.raw_os_error() == Some(112) {
                    tr!("Auf dem Zieldatenträger ist nicht genug Platz.", "There is not enough space on the target drive.").to_string()
                } else {
                    trf!("Schreibfehler: {e}", "Write error: {e}")
                }
            })?;
            self.in_part += n as u64;
            self.total += n as u64;
            if let Some(last) = self.part_sizes.last_mut() {
                *last += n as u64;
            }
            rest = b;
        }
        Ok(())
    }

    pub fn finish(&mut self) -> Res<()> {
        if let Some(f) = self.file.take() {
            let mut f = f;
            f.flush().map_err(|e| e.to_string())?;
        }
        Ok(())
    }

    /// Ändert einen bereits geschriebenen Sektor.
    pub fn patch_all(&self, map: &SectorMap, mut f: impl FnMut(&mut [u8], usize)) -> Res<()> {
        // Sortiert nach neuem Start, damit die Platte ruhig bleibt.
        let mut order: Vec<usize> = (0..map.vobus.len()).collect();
        order.sort_by_key(|i| map.vobus[*i].2);
        let mut open: Option<(usize, File)> = None;
        let mut buf = vec![0u8; SECTOR];
        for i in order {
            let new_start = u64::from(map.vobus[i].2);
            let mut remain = new_start;
            let mut part = 0usize;
            for (idx, size) in self.part_sizes.iter().enumerate() {
                if remain < *size {
                    part = idx;
                    break;
                }
                remain -= *size;
            }
            if open.as_ref().map(|o| o.0) != Some(part) {
                let file = OpenOptions::new()
                    .read(true)
                    .write(true)
                    .open(&self.paths[part])
                    .map_err(|e| format!("{}: {e}", self.paths[part].display()))?;
                open = Some((part, file));
            }
            let file = &mut open.as_mut().unwrap().1;
            let at = remain * SECTOR as u64;
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            file.read_exact(&mut buf).map_err(|e| e.to_string())?;
            f(&mut buf, i);
            file.seek(SeekFrom::Start(at)).map_err(|e| e.to_string())?;
            file.write_all(&buf).map_err(|e| e.to_string())?;
        }
        Ok(())
    }
}

/// Eine umgewandelte VOBU.
pub struct VobuOut {
    pub data: Vec<u8>,
    /// Sektoren, die Video belegten (Eingabe)
    pub video_in: u64,
    pub video_out: u64,
    pub kept_other: u64,
}

fn map_rel(old_to_new: &[i32], old: usize) -> Option<u32> {
    if old >= old_to_new.len() {
        return None;
    }
    let mut i = old as isize;
    while i >= 0 {
        if old_to_new[i as usize] >= 0 {
            return Some(old_to_new[i as usize] as u32);
        }
        i -= 1;
    }
    None
}

fn patch_sync(nav: &mut [u8], old_to_new: &[i32], d: &CellDecision) {
    for k in 0..8usize {
        let at = DSI_OFF + DSI_SYNCI + k * 2;
        let v = be16(nav, at);
        if !d.keep_audio[k] {
            put16(nav, at, 0);
        } else if v != 0 && (v & 0x3FFF) != 0x3FFF {
            if v & 0x8000 != 0 {
                put16(nav, at, 0x3FFF);
            } else {
                let new = old_to_new
                    .get(v as usize)
                    .copied()
                    .filter(|n| *n >= 0)
                    .map(|n| n as u16)
                    .unwrap_or(0x3FFF);
                put16(nav, at, new);
            }
        }
    }
    for k in 0..32usize {
        let at = DSI_OFF + DSI_SYNCI + 16 + k * 4;
        let v = be32(nav, at);
        if !d.keep_sub[k] {
            put32(nav, at, 0);
        } else if v != 0 && (v & 0x3FFF_FFFF) != 0x3FFF_FFFF {
            if v & 0x8000_0000 != 0 {
                put32(nav, at, 0x3FFF_FFFF);
            } else {
                let new = old_to_new
                    .get(v as usize)
                    .copied()
                    .filter(|n| *n >= 0)
                    .map(|n| n as u32)
                    .unwrap_or(0x3FFF_FFFF);
                put32(nav, at, new);
            }
        }
    }
}

/// Wandelt eine einzelne VOBU um.
pub fn transcode_vobu(input: &[u8], d: &CellDecision, opts: VideoOpts, rq: &mut Requant) -> VobuOut {
    let count = input.len() / SECTOR;
    let all_audio = d.keep_audio.iter().all(|k| *k);
    let all_subs = d.keep_sub.iter().all(|k| *k);
    let mut video_in = 0u64;
    let mut other_kept = 0u64;

    if opts.is_passthrough() {
        // Nur Spuren filtern; das Video bleibt Byte für Byte wie es ist.
        let mut out = Vec::with_capacity(input.len());
        let mut old_to_new = vec![-1i32; count];
        let mut video_out = 0u64;
        for (i, s) in input.chunks_exact(SECTOR).enumerate() {
            let keep = match classify(s) {
                Stream::Audio(a) => d.keep_audio[a as usize],
                Stream::Sub(a) => d.keep_sub[a as usize],
                Stream::Video => {
                    video_in += 1;
                    video_out += 1;
                    true
                }
                _ => true,
            };
            if keep {
                old_to_new[i] = (out.len() / SECTOR) as i32;
                out.extend_from_slice(s);
                other_kept += 1;
            }
        }
        let new_count = out.len() / SECTOR;
        {
            let nav = &mut out[..SECTOR];
            put32(nav, DSI_OFF + 8, (new_count - 1) as u32);
            for k in 0..3usize {
                let at = DSI_OFF + 12 + k * 4;
                let v = be32(nav, at);
                if v != 0 {
                    let n = map_rel(&old_to_new, v as usize).unwrap_or(0);
                    put32(nav, at, n);
                }
            }
            if !(all_audio && all_subs) {
                patch_sync(nav, &old_to_new, d);
            }
        }
        return VobuOut { data: out, video_in, video_out, kept_other: other_kept.saturating_sub(video_out) };
    }

    // Videostrom einsammeln
    struct VPack {
        sector: usize,
        es_start: usize,
        es_end: usize,
        pts: Option<u64>,
        dts: Option<u64>,
    }
    let mut es = Vec::with_capacity(input.len());
    let mut packs: Vec<VPack> = Vec::new();
    let mut first_ext: Vec<u8> = Vec::new();
    for (i, s) in input.chunks_exact(SECTOR).enumerate() {
        if classify(s) == Stream::Video {
            if let Some(p) = parse_video(s) {
                video_in += 1;
                if packs.is_empty() {
                    first_ext = p.ext.to_vec();
                }
                let es_start = es.len();
                es.extend_from_slice(p.payload);
                packs.push(VPack { sector: i, es_start, es_end: es.len(), pts: p.pts, dts: p.dts });
            }
        }
    }

    rq.set_opts(opts);
    let processed: EsOut = rq.process(&es);
    let new_es = &processed.es;

    // Zeitstempel: Ein Paket trägt den Stempel des ersten darin beginnenden Bildes.
    let mut pic_ts: Vec<(usize, Option<(u64, Option<u64>)>)> =
        processed.pics.iter().map(|p| (p.out_start, None)).collect();
    for pack in &packs {
        let Some(pts) = pack.pts else { continue };
        let idx = processed.pics.partition_point(|p| p.in_start < pack.es_start);
        if let Some(pic) = processed.pics.get(idx) {
            if pic.in_start < pack.es_end && pic_ts[idx].1.is_none() {
                pic_ts[idx].1 = Some((pts, pack.dts));
            }
        }
    }

    // Pakete neu aufbauen
    let mut out = Vec::with_capacity(input.len());
    let mut old_to_new = vec![-1i32; count];
    let mut emitted = 0usize;
    let mut last_scr: Option<u64> = None;
    let mut es_marks: Vec<(usize, usize, usize)> = Vec::with_capacity(packs.len());
    let mut video_out = 0u64;
    let mut next_pack = 0usize;
    let last_video_sector = packs.last().map(|p| p.sector);

    let bump_scr = |last: &mut Option<u64>, orig: u64| -> u64 {
        let scr = match *last {
            Some(l) if orig < l + SCR_TICK => l + SCR_TICK,
            _ => orig,
        };
        *last = Some(scr);
        scr
    };

    for (i, s) in input.chunks_exact(SECTOR).enumerate() {
        match classify(s) {
            Stream::Video if packs.get(next_pack).map(|p| p.sector) == Some(i) => {
                let pack = &packs[next_pack];
                next_pack += 1;
                let is_last = last_video_sector == Some(i);
                let target = if is_last { new_es.len() } else { processed.map_pos(pack.es_end) };
                loop {
                    let remaining = new_es.len() - emitted;
                    if remaining == 0 {
                        break;
                    }
                    let ext: &[u8] = if emitted == 0 { &first_ext } else { &[] };
                    let base_room = SECTOR - 14 - 9 - ext.len();
                    // Beginnt im Paket ein Bild?
                    let idx = pic_ts.partition_point(|p| p.0 < emitted);
                    let mut ts = None;
                    if let Some((at, Some(t))) = pic_ts.get(idx) {
                        let len = if t.1.is_some() { 10 } else { 5 };
                        if *at < emitted + base_room - len {
                            ts = Some(*t);
                        }
                    }
                    let room = base_room - ts.map(|t| if t.1.is_some() { 10 } else { 5 }).unwrap_or(0);
                    if !is_last && target.saturating_sub(emitted) < room {
                        break;
                    }
                    let scr = bump_scr(&mut last_scr, get_scr(s));
                    let mut hdr = [0u8; 14];
                    hdr.copy_from_slice(&s[..14]);
                    put_scr(&mut hdr, scr);
                    let (sector, take) = build_video_pack(&hdr, ts, ext, &new_es[emitted..]);
                    let out_idx = out.len() / SECTOR;
                    es_marks.push((out_idx, emitted, emitted + take));
                    emitted += take;
                    out.extend_from_slice(&sector);
                    video_out += 1;
                    if !is_last {
                        break;
                    }
                }
            }
            Stream::Nav => {
                last_scr = Some(get_scr(s));
                old_to_new[i] = (out.len() / SECTOR) as i32;
                out.extend_from_slice(s);
            }
            Stream::Audio(a) if d.keep_audio[a as usize] => {
                push_other(&mut out, s, &mut old_to_new, i, &bump_scr, &mut last_scr);
                other_kept += 1;
            }
            Stream::Sub(a) if d.keep_sub[a as usize] => {
                push_other(&mut out, s, &mut old_to_new, i, &bump_scr, &mut last_scr);
                other_kept += 1;
            }
            _ => {}
        }
    }

    let new_count = out.len() / SECTOR;
    {
        let nav = &mut out[..SECTOR];
        put32(nav, DSI_OFF + 8, (new_count - 1) as u32);
        let refs = reference_ends(&processed.pics, &es_marks);
        for k in 0..3usize {
            let at = DSI_OFF + 12 + k * 4;
            if be32(nav, at) != 0 {
                put32(nav, at, refs.get(k).copied().unwrap_or(new_count as u32 - 1));
            }
        }
        patch_sync(nav, &old_to_new, d);
    }
    VobuOut { data: out, video_in, video_out, kept_other: other_kept }
}

fn push_other(
    out: &mut Vec<u8>,
    s: &[u8],
    old_to_new: &mut [i32],
    i: usize,
    bump: &impl Fn(&mut Option<u64>, u64) -> u64,
    last: &mut Option<u64>,
) {
    let orig = get_scr(s);
    let scr = bump(last, orig);
    old_to_new[i] = (out.len() / SECTOR) as i32;
    let at = out.len();
    out.extend_from_slice(s);
    if scr != orig {
        put_scr(&mut out[at..at + SECTOR], scr);
    }
}

/// Sektor-Offsets, in denen die ersten drei Referenzbilder enden.
fn reference_ends(pics: &[PicRec], marks: &[(usize, usize, usize)]) -> Vec<u32> {
    let mut out = Vec::with_capacity(3);
    let mut i = 0;
    while i < pics.len() && out.len() < 3 {
        let p = &pics[i];
        if p.coding_type == PIC_B {
            i += 1;
            continue;
        }
        let mut end = p.out_end;
        // Zwei Halbbilder ergeben ein Bild
        if p.structure != STRUCT_FRAME {
            if let Some(n) = pics.get(i + 1) {
                if n.structure != STRUCT_FRAME && n.coding_type != PIC_B {
                    end = n.out_end;
                    i += 1;
                }
            }
        }
        let byte = end.saturating_sub(1);
        let idx = marks.partition_point(|m| m.2 <= byte);
        if let Some(m) = marks.get(idx) {
            out.push(m.0 as u32);
        }
        i += 1;
    }
    out
}

/// Vorzeichen-Betrag-Offset (oberstes Bit = rückwärts) umrechnen.
fn map_signed(v: u32, old_nav: u32, new_nav: u32, map: &SectorMap) -> u32 {
    if v == 0 || v == 0x7FFF_FFFF || v == 0xFFFF_FFFF {
        return v;
    }
    let back = v & 0x8000_0000 != 0;
    let off = v & 0x7FFF_FFFF;
    let target = if back { old_nav.saturating_sub(off) } else { old_nav.saturating_add(off) };
    if !map.contains(target) {
        return 0x7FFF_FFFF;
    }
    let new_target = map.map_start(target);
    if new_target >= new_nav {
        new_target - new_nav
    } else {
        0x8000_0000 | (new_nav - new_target)
    }
}

/// Zweiter Durchgang: alle Verweise in einem Navigationspaket umrechnen.
pub fn patch_nav(s: &mut [u8], vobu: (u32, u32, u32, u32), map: &SectorMap, remove_puo: bool, strip_angles: bool) {
    let (old_start, _old_end, new_start, new_end) = vobu;

    // Sektornummer (ein evtl. fester Abstand zur Zone bleibt erhalten)
    let pci_lbn = be32(s, PCI_OFF);
    put32(s, PCI_OFF, new_start.wrapping_add(pci_lbn.wrapping_sub(old_start)));
    let dsi_lbn = be32(s, DSI_OFF + 4);
    put32(s, DSI_OFF + 4, new_start.wrapping_add(dsi_lbn.wrapping_sub(old_start)));
    put32(s, DSI_OFF + 8, new_end - new_start);

    if remove_puo {
        put32(s, PCI_OFF + 8, 0);
    }

    // Sprungtabelle
    for k in 0..42usize {
        let at = DSI_OFF + DSI_SRI + k * 4;
        let v = be32(s, at);
        let off = v & 0x3FFF_FFFF;
        if off == SRI_END || off == 0 {
            continue;
        }
        let flags = v & 0xC000_0000;
        let forward = k <= 20;
        let target = if forward { old_start.saturating_add(off) } else { old_start.saturating_sub(off) };
        if !map.contains(target) {
            put32(s, at, SRI_END);
            continue;
        }
        let nt = map.map_start(target);
        let diff = if nt >= new_start { nt - new_start } else { new_start - nt };
        put32(s, at, flags | diff);
    }

    if strip_angles {
        // Nur noch ein Winkel: Verschachtelungsangaben und Tasten entfallen.
        for b in &mut s[DSI_OFF + DSI_SML_PBI..DSI_OFF + DSI_SRI] {
            *b = 0;
        }
        for b in &mut s[PCI_OFF + PCI_NSML_AGLI..PCI_OFF + PCI_NSML_AGLI + 36] {
            *b = 0;
        }
        put16(s, PCI_OFF + PCI_HLI, 0);
        return;
    }

    // Verschachtelung (Kamerawinkel, nahtlose Verzweigungen)
    let pbi = DSI_OFF + DSI_SML_PBI;
    let ilvu_ea = be32(s, pbi + 2);
    if ilvu_ea != 0 {
        let old_end = old_start.saturating_add(ilvu_ea);
        put32(s, pbi + 2, map.map_end(old_end).saturating_sub(new_start));
    }
    let ilvu_sa = be32(s, pbi + 6);
    if ilvu_sa != 0 {
        let target = old_start.wrapping_add(ilvu_sa);
        put32(s, pbi + 6, map.map_start(target).wrapping_sub(new_start));
    }
    let ilvu_size = be16(s, pbi + 10);
    if ilvu_size != 0 && ilvu_sa != 0 {
        let start = old_start.wrapping_add(ilvu_sa);
        let end = start.saturating_add(u32::from(ilvu_size) - 1);
        let n = map.map_end(end).saturating_sub(map.map_start(start)) + 1;
        put16(s, pbi + 10, n.min(0xFFFF) as u16);
    }

    for k in 0..9usize {
        let at = DSI_OFF + DSI_SML_AGLI + k * 6;
        let v = be32(s, at);
        let size = be16(s, at + 4);
        if v == 0 || v == 0x7FFF_FFFF || v == 0xFFFF_FFFF {
            continue;
        }
        let back = v & 0x8000_0000 != 0;
        let off = v & 0x7FFF_FFFF;
        let start = if back { old_start.saturating_sub(off) } else { old_start.saturating_add(off) };
        put32(s, at, map_signed(v, old_start, new_start, map));
        if size != 0 {
            let end = start.saturating_add(u32::from(size) - 1);
            let n = map.map_end(end).saturating_sub(map.map_start(start)) + 1;
            put16(s, at + 4, n.min(0xFFFF) as u16);
        }
    }

    for k in 0..9usize {
        let at = PCI_OFF + PCI_NSML_AGLI + k * 4;
        let v = be32(s, at);
        put32(s, at, map_signed(v, old_start, new_start, map));
    }
}

/// Macht die Zeitstempel fortlaufend, wenn mehrere Abschnitte in einer Datei landen.
/// Auf der DVD beginnt jeder Abschnitt seine Uhr neu; in einer einzelnen Datei würden
/// solche Sprünge Dauer und Vorspulen im Player durcheinanderbringen.
/// Die Datei beginnt so, dass das erste Bild nach einer halben Sekunde erscheint.
const START_PTS: i64 = 45_000;

#[derive(Default)]
pub struct Retimer {
    offset: i64,
    last_end: Option<i64>,
    last_scr: Option<i64>,
}

impl Retimer {
    /// Bearbeitet eine fertige VOBU (das erste Paket ist das Navigationspaket).
    pub fn apply(&mut self, data: &mut [u8]) {
        if data.len() < SECTOR {
            return;
        }
        let (s_ptm, e_ptm) = (i64::from(be32(data, PCI_OFF + 12)), i64::from(be32(data, PCI_OFF + 16)));
        let first_scr = (get_scr(data) / 300) as i64;
        let has_ptm = s_ptm != 0 || e_ptm != 0;

        // Erste VOBU: Zeitachse bei null beginnen lassen. Die Originalzeiten können
        // beliebig groß sein (spätere Stelle im Film, manche DVDs starten ihre Uhr hoch);
        // nahe am Überlauf der 33-Bit-Uhr liefe die Datei sonst rückwärts.
        // Am Kapitelanfang mitten im Film liegt der Ton oft vor dem ersten Bild (er wird
        // später gemultiplext als das Bild); der früheste Zeitstempel bestimmt den Anfang,
        // sonst fiele der Ton unter null und liefe über.
        if self.last_scr.is_none() && self.last_end.is_none() {
            // Mehr als 5 s Vorlauf wäre ein Überlauf der Quelluhr, kein echter Vorlauf.
            let earliest = min_ts(data).filter(|m| s_ptm - m < 450_000).map_or(s_ptm, |m| m.min(s_ptm));
            let by_pts = if has_ptm { START_PTS - earliest } else { -first_scr };
            // Die Systemuhr darf nicht unter null fallen
            self.offset = by_pts.max(-first_scr);
        }

        let scr_back = self.last_scr.is_some_and(|l| first_scr + self.offset <= l);
        let pts_jump = has_ptm && self.last_end.is_some_and(|e| (s_ptm + self.offset - e).abs() > 45_000);
        if scr_back || pts_jump {
            let by_pts = if has_ptm { self.last_end.map(|e| e - s_ptm) } else { None };
            let by_scr = self.last_scr.map(|l| l + 1 - first_scr);
            self.offset = match (by_pts, by_scr) {
                (Some(a), Some(b)) => a.max(b),
                (Some(a), None) => a,
                (None, Some(b)) => b,
                (None, None) => self.offset,
            };
        }

        if self.offset != 0 {
            for s in data.chunks_exact_mut(SECTOR) {
                shift_pack(s, self.offset);
            }
        }
        if has_ptm {
            self.last_end = Some(e_ptm + self.offset);
        }
        if let Some(last) = data.chunks_exact(SECTOR).rev().find(|s| is_pack(s)) {
            self.last_scr = Some((get_scr(last) / 300) as i64);
        }
    }
}

impl Retimer {
    /// Abschlusspaket mit der Endzeit der Datei. Viele DVDs tragen Zeitstempel nur am
    /// Anfang jeder Bildgruppe; ist die letzte groß, steht in den letzten 200 KB keiner,
    /// und VLC schätzt die Länge dann aus der Datenrate (ergibt Stunden statt Minuten).
    /// Das Paket enthält nur Füllnullen, Decoder überspringen es.
    pub fn end_pack(&self) -> Option<Vec<u8>> {
        let (end, scr) = (self.last_end?, self.last_scr?);
        let mut s = vec![0u8; SECTOR];
        s[..4].copy_from_slice(&[0, 0, 1, 0xBA]);
        put_scr(&mut s, (scr.max(0) as u64 + 1) * 300);
        s[10..13].copy_from_slice(&[0x01, 0x89, 0xC3]); // 10,08 Mbit/s wie auf der DVD
        s[13] = 0xF8;
        // Bild-PES: Kopf mit PTS, vier Füllnullen
        s[14..18].copy_from_slice(&[0, 0, 1, 0xE0]);
        put16(&mut s, 18, 3 + 5 + 4);
        s[20] = 0x81;
        s[21] = 0x80;
        s[22] = 5;
        write_ts(&mut s[23..], 2, end.rem_euclid(TS_WRAP) as u64);
        // Rest: Füllpaket
        let pad = 32;
        s[pad..pad + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
        put16(&mut s, pad + 4, (SECTOR - pad - 6) as u16);
        s[pad + 6..].fill(0xFF);
        Some(s)
    }
}

const TS_WRAP: i64 = 1 << 33;

/// Kleinster PTS/DTS aller Bild- und Tonpakete einer VOBU.
fn min_ts(data: &[u8]) -> Option<i64> {
    let mut min: Option<i64> = None;
    for s in data.chunks_exact(SECTOR) {
        if !is_pack(s) || is_nav(s) {
            continue;
        }
        let h = pack_header_len(s);
        if h + 9 > s.len() || !matches!(s[h + 3], 0xE0 | 0xBD | 0xC0..=0xC7) {
            continue;
        }
        let flags = s[h + 7] >> 6;
        let mut take = |at: usize| {
            if at + 5 <= s.len() {
                let v = read_ts(&s[at..]) as i64;
                min = Some(min.map_or(v, |m| m.min(v)));
            }
        };
        if flags >= 2 {
            take(h + 9);
        }
        if flags == 3 {
            take(h + 14);
        }
    }
    min
}

fn shift_ts(s: &mut [u8], at: usize, offset: i64) {
    let prefix = s[at] >> 4;
    let ts = (read_ts(&s[at..]) as i64 + offset).rem_euclid(TS_WRAP);
    write_ts(&mut s[at..], prefix, ts as u64);
}

/// Verschiebt alle Zeitangaben eines Pakets um offset (in 90-kHz-Einheiten).
fn shift_pack(s: &mut [u8], offset: i64) {
    if !is_pack(s) {
        return;
    }
    let scr = (get_scr(s) as i64 + offset * 300).max(0) as u64;
    let nav = is_nav(s);
    if nav {
        Nav::set_scr(s, scr);
        for field in [12usize, 16, 20] {
            let v = i64::from(be32(s, PCI_OFF + field));
            if v != 0 {
                put32(s, PCI_OFF + field, (v + offset).max(0) as u32);
            }
        }
        return;
    }
    put_scr(s, scr);
    let h = pack_header_len(s);
    if h + 9 > s.len() || !matches!(s[h + 3], 0xE0 | 0xBD | 0xC0..=0xC7) {
        return;
    }
    let flags = s[h + 7] >> 6;
    if flags >= 2 && h + 14 <= s.len() {
        shift_ts(s, h + 9, offset);
    }
    if flags == 3 && h + 19 <= s.len() {
        shift_ts(s, h + 14, offset);
    }
}

/// Kopiert eine Zone ohne NAV-Pakete unverändert (sollte praktisch nie vorkommen).
pub fn copy_raw(reader: &mut SpaceReader, writer: &mut SpaceWriter) -> Res<()> {
    let total = reader.sectors();
    let mut pos = 0;
    let mut buf = Vec::new();
    while pos < total {
        let n = reader.read(pos, 512, &mut buf)?;
        if n == 0 {
            break;
        }
        writer.write(&buf)?;
        pos += n as u64;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Leeres Paket mit Systemuhr.
    fn pack(scr: u64) -> Vec<u8> {
        let mut s = vec![0u8; SECTOR];
        s[..4].copy_from_slice(&[0, 0, 1, 0xBA]);
        s[13] = 0xF8;
        put_scr(&mut s, scr * 300);
        s
    }

    /// Paket eines Elementarstroms mit PTS.
    fn pes(scr: u64, id: u8, pts: u64) -> Vec<u8> {
        let mut s = pack(scr);
        s[14..18].copy_from_slice(&[0, 0, 1, id]);
        put16(&mut s, 18, (SECTOR - 20) as u16);
        s[20] = 0x81;
        s[21] = 0x80;
        s[22] = 5;
        write_ts(&mut s[23..], 2, pts);
        s
    }

    fn nav(scr: u64, s_ptm: u32, e_ptm: u32) -> Vec<u8> {
        let mut s = pack(scr);
        s[14..18].copy_from_slice(&[0, 0, 1, 0xBB]);
        s[0x26..0x2A].copy_from_slice(&[0, 0, 1, 0xBF]);
        s[0x400..0x404].copy_from_slice(&[0, 0, 1, 0xBF]);
        put32(&mut s, PCI_OFF + 12, s_ptm);
        put32(&mut s, PCI_OFF + 16, e_ptm);
        s
    }

    #[test]
    fn retimer_keeps_leading_audio_above_zero() {
        // Kapitel mitten im Film: der Ton beginnt 0,8 s vor dem ersten Bild.
        let s_ptm = 5_000_000u64;
        let mut v = nav(s_ptm - 80_000, s_ptm as u32, s_ptm as u32 + 45_000);
        v.extend(pes(s_ptm - 79_000, 0xE0, s_ptm));
        v.extend(pes(s_ptm - 78_000, 0xC0, s_ptm - 72_000));
        let mut r = Retimer::default();
        r.apply(&mut v);
        let ts = |k: usize| read_ts(&v[k * SECTOR + 23..]);
        let (video, audio) = (ts(1), ts(2));
        assert!(audio < video && audio > 0 && video < 90_000 * 2, "Bild {video}, Ton {audio}");
        assert_eq!(video - audio, 72_000);
    }
}