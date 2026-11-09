//! Baut IFO-Tabellen von Grund auf: für das Neu-Zusammenstellen und die Beispiel-DVD.

use crate::dvd::ifo::{encode_time, CellAddr, TitleEntry};
use crate::dvd::{put16, put32, SECTOR};

pub type Cmd = [u8; 8];

pub const CMD_EXIT: Cmd = [0x30, 0x01, 0, 0, 0, 0, 0, 0];

pub fn cmd_jump_tt(title: u8) -> Cmd {
    [0x30, 0x02, 0, 0, 0, title, 0, 0]
}

pub fn cmd_link_pgcn(pgc: u16) -> Cmd {
    let b = pgc.to_be_bytes();
    [0x20, 0x04, 0, 0, 0, 0, b[0], b[1]]
}

#[derive(Clone, Debug)]
pub struct ACell {
    pub ms: u64,
    /// Die ersten vier Bytes des Zelleneintrags (Modus, Standzeit, Befehl)
    pub flags: [u8; 4],
    pub first: u32,
    pub last_vobu: u32,
    pub last: u32,
    pub vob: u16,
    pub cell: u8,
}

#[derive(Clone, Debug)]
pub struct APgc {
    pub cells: Vec<ACell>,
    /// Erste Zelle jedes Programms (ab 1)
    pub programs: Vec<u8>,
    pub audio_ctl: [u16; 8],
    pub sub_ctl: [u32; 32],
    pub palette: [u8; 64],
    pub pre: Vec<Cmd>,
    pub post: Vec<Cmd>,
    pub pal: bool,
}

impl Default for APgc {
    fn default() -> Self {
        Self {
            cells: Vec::new(),
            programs: Vec::new(),
            audio_ctl: [0; 8],
            sub_ctl: [0; 32],
            palette: [0; 64],
            pre: Vec::new(),
            post: Vec::new(),
            pal: true,
        }
    }
}

fn pgc_bytes(p: &APgc) -> Vec<u8> {
    let nc = p.pre.len() + p.post.len();
    let cmd_off = 0xEC;
    let cmd_len = if nc > 0 { 8 + nc * 8 } else { 0 };
    let map_off = cmd_off + cmd_len;
    let map_len = (p.programs.len() + 1) & !1;
    let cell_off = map_off + map_len;
    let pos_off = cell_off + p.cells.len() * 24;
    let total = pos_off + p.cells.len() * 4;
    let mut d = vec![0u8; total];
    d[2] = p.programs.len() as u8;
    d[3] = p.cells.len() as u8;
    let ms: u64 = p.cells.iter().map(|c| c.ms).sum();
    d[4..8].copy_from_slice(&encode_time(ms, p.pal));
    for (i, v) in p.audio_ctl.iter().enumerate() {
        put16(&mut d, 0x0C + i * 2, *v);
    }
    for (i, v) in p.sub_ctl.iter().enumerate() {
        put32(&mut d, 0x1C + i * 4, *v);
    }
    d[0xA4..0xE4].copy_from_slice(&p.palette);
    if nc > 0 {
        put16(&mut d, 0xE4, cmd_off as u16);
        put16(&mut d, cmd_off, p.pre.len() as u16);
        put16(&mut d, cmd_off + 2, p.post.len() as u16);
        put16(&mut d, cmd_off + 6, (cmd_len - 1) as u16);
        for (i, c) in p.pre.iter().chain(p.post.iter()).enumerate() {
            d[cmd_off + 8 + i * 8..cmd_off + 16 + i * 8].copy_from_slice(c);
        }
    }
    if !p.programs.is_empty() {
        put16(&mut d, 0xE6, map_off as u16);
        d[map_off..map_off + p.programs.len()].copy_from_slice(&p.programs);
        put16(&mut d, 0xE8, cell_off as u16);
        put16(&mut d, 0xEA, pos_off as u16);
    }
    for (i, c) in p.cells.iter().enumerate() {
        let o = cell_off + i * 24;
        d[o..o + 4].copy_from_slice(&c.flags);
        d[o + 4..o + 8].copy_from_slice(&encode_time(c.ms, p.pal));
        put32(&mut d, o + 8, c.first);
        put32(&mut d, o + 12, 0);
        put32(&mut d, o + 16, c.last_vobu);
        put32(&mut d, o + 20, c.last);
        let b = pos_off + i * 4;
        put16(&mut d, b, c.vob);
        d[b + 3] = c.cell;
    }
    d
}

fn pad_sector(v: &mut Vec<u8>) {
    let rem = v.len() % SECTOR;
    if rem != 0 {
        v.resize(v.len() + SECTOR - rem, 0);
    }
}

fn cadt_bytes(cells: &[CellAddr]) -> Vec<u8> {
    let mut vobs: Vec<u16> = cells.iter().map(|c| c.vob_id).collect();
    vobs.sort_unstable();
    vobs.dedup();
    let mut d = vec![0u8; 8 + cells.len() * 12];
    put16(&mut d, 0, vobs.len() as u16);
    let last = (d.len() - 1) as u32;
    put32(&mut d, 4, last);
    for (i, c) in cells.iter().enumerate() {
        let e = 8 + i * 12;
        put16(&mut d, e, c.vob_id);
        d[e + 2] = c.cell_id;
        put32(&mut d, e + 4, c.start);
        put32(&mut d, e + 8, c.last);
    }
    d
}

fn admap_bytes(admap: &[u32]) -> Vec<u8> {
    let mut d = vec![0u8; 4 + admap.len() * 4];
    let last = (d.len() - 1) as u32;
    put32(&mut d, 0, last);
    for (i, v) in admap.iter().enumerate() {
        put32(&mut d, 4 + i * 4, *v);
    }
    d
}

/// Menüteil einer Disc oder eines Titelsets.
#[derive(Clone, Debug)]
pub struct AMenu {
    pub pgc: APgc,
    pub cells: Vec<CellAddr>,
    pub admap: Vec<u32>,
    pub sectors: u32,
    /// 0x83 = Hauptmenü eines Titelsets, 0x82 = Titelmenü der Disc
    pub category: u8,
    pub lang_flags: u8,
}

fn pgci_ut_bytes(m: &AMenu) -> Vec<u8> {
    let pgc = pgc_bytes(&m.pgc);
    // Kopf (8), eine Sprache (8), Sprachblock: Kopf (8) + ein Eintrag (8) + PGC
    let lu_len = 16 + pgc.len();
    let mut d = vec![0u8; 16 + lu_len];
    put16(&mut d, 0, 1);
    let last = (d.len() - 1) as u32;
    put32(&mut d, 4, last);
    d[8] = b'e';
    d[9] = b'n';
    d[11] = m.lang_flags;
    put32(&mut d, 12, 16);
    put16(&mut d, 16, 1);
    put32(&mut d, 20, (lu_len - 1) as u32);
    d[24] = m.category;
    put32(&mut d, 28, 16);
    d[32..32 + pgc.len()].copy_from_slice(&pgc);
    d
}

#[derive(Clone, Debug)]
pub struct TimeMap {
    pub unit_secs: u8,
    pub entries: Vec<u32>,
}

/// Beschreibung eines Titelsets.
#[derive(Clone, Debug, Default)]
pub struct AVts {
    /// Bereich 0x100..0x3D8 aus VTSI_MAT (Bild-, Ton- und Untertitel-Eigenschaften)
    pub attrs: Vec<u8>,
    pub pgcs: Vec<APgc>,
    pub menu: Option<AMenu>,
    pub cells: Vec<CellAddr>,
    pub admap: Vec<u32>,
    pub tmaps: Vec<TimeMap>,
    pub title_sectors: u32,
}

pub const ATTR_START: usize = 0x100;
pub const ATTR_END: usize = 0x3D8;

/// Baut VTS_xx_0.IFO.
pub fn build_vts(v: &AVts) -> Vec<u8> {
    // Kapitel-Tabelle: jeder Titel entspricht einer PGC, Programme sind Kapitel
    let mut ptt = vec![0u8; 8 + v.pgcs.len() * 4];
    put16(&mut ptt, 0, v.pgcs.len() as u16);
    for (i, p) in v.pgcs.iter().enumerate() {
        let off = ptt.len();
        put32(&mut ptt, 8 + i * 4, off as u32);
        for n in 0..p.programs.len() {
            ptt.extend_from_slice(&((i + 1) as u16).to_be_bytes());
            ptt.extend_from_slice(&((n + 1) as u16).to_be_bytes());
        }
    }
    let end = (ptt.len() - 1) as u32;
    put32(&mut ptt, 4, end);

    // Programmketten
    let head = 8 + v.pgcs.len() * 8;
    let blobs: Vec<Vec<u8>> = v.pgcs.iter().map(pgc_bytes).collect();
    let total: usize = head + blobs.iter().map(Vec::len).sum::<usize>();
    let mut pgcit = vec![0u8; head];
    put16(&mut pgcit, 0, v.pgcs.len() as u16);
    put32(&mut pgcit, 4, (total - 1) as u32);
    let mut off = head;
    for (i, b) in blobs.iter().enumerate() {
        pgcit[8 + i * 8] = 0x80 | ((i + 1) as u8);
        put32(&mut pgcit, 8 + i * 8 + 4, off as u32);
        off += b.len();
    }
    for b in &blobs {
        pgcit.extend_from_slice(b);
    }

    // Zeitkarten
    let head = 8 + v.tmaps.len() * 4;
    let mut tmap = vec![0u8; head];
    put16(&mut tmap, 0, v.tmaps.len() as u16);
    for (i, t) in v.tmaps.iter().enumerate() {
        let off = tmap.len();
        put32(&mut tmap, 8 + i * 4, off as u32);
        tmap.push(t.unit_secs);
        tmap.push(0);
        tmap.extend_from_slice(&(t.entries.len() as u16).to_be_bytes());
        for e in &t.entries {
            tmap.extend_from_slice(&e.to_be_bytes());
        }
    }
    let end = (tmap.len() - 1) as u32;
    put32(&mut tmap, 4, end);

    let mut d = vec![0u8; SECTOR];
    d[0..12].copy_from_slice(b"DVDVIDEO-VTS");
    put16(&mut d, 0x20, 0x0011);
    put32(&mut d, 0x80, 0x7FF);
    let n = v.attrs.len().min(ATTR_END - ATTR_START);
    d[ATTR_START..ATTR_START + n].copy_from_slice(&v.attrs[..n]);

    let place = |d: &mut Vec<u8>, table: &[u8], at: usize| {
        pad_sector(d);
        let sector = (d.len() / SECTOR) as u32;
        put32(d, at, sector);
        d.extend_from_slice(table);
    };

    place(&mut d, &ptt, 0xC8);
    place(&mut d, &pgcit, 0xCC);
    if let Some(m) = &v.menu {
        place(&mut d, &pgci_ut_bytes(m), 0xD0);
    }
    place(&mut d, &tmap, 0xD4);
    if let Some(m) = &v.menu {
        place(&mut d, &cadt_bytes(&m.cells), 0xD8);
        place(&mut d, &admap_bytes(&m.admap), 0xDC);
    }
    place(&mut d, &cadt_bytes(&v.cells), 0xE0);
    place(&mut d, &admap_bytes(&v.admap), 0xE4);
    pad_sector(&mut d);

    let ifo = (d.len() / SECTOR) as u32;
    let menu = v.menu.as_ref().map(|m| m.sectors).unwrap_or(0);
    put32(&mut d, 0x1C, ifo - 1);
    put32(&mut d, 0x0C, ifo * 2 + menu + v.title_sectors - 1);
    put32(&mut d, 0xC0, if menu > 0 { ifo } else { 0 });
    put32(&mut d, 0xC4, ifo + menu);
    d
}

#[derive(Clone, Debug, Default)]
pub struct AVmg {
    pub titles: Vec<TitleEntry>,
    /// Attribute der Titelsets (je Bereich 0x100..0x3D8 aus VTSI_MAT)
    pub vts_attrs: Vec<Vec<u8>>,
    pub first_play: Vec<Cmd>,
    pub menu: Option<AMenu>,
    /// Bildeigenschaften des Hauptmenüs (die zwei Bytes bei 0x100)
    pub menu_video: [u8; 2],
    pub pal: bool,
}

/// Baut VIDEO_TS.IFO.
pub fn build_vmg(v: &AVmg) -> Vec<u8> {
    let mut d = vec![0u8; SECTOR];
    d[0..12].copy_from_slice(b"DVDVIDEO-VMG");
    put16(&mut d, 0x20, 0x0011);
    put16(&mut d, 0x26, 1);
    put16(&mut d, 0x28, 1);
    d[0x2A] = 1;
    put16(&mut d, 0x3E, v.vts_attrs.len() as u16);
    d[0x40..0x4A].copy_from_slice(b"DVDMANAGER");
    put32(&mut d, 0x80, 0x7FF);
    d[0x100..0x102].copy_from_slice(&v.menu_video);

    let fp = pgc_bytes(&APgc { pre: v.first_play.clone(), pal: v.pal, ..Default::default() });
    put32(&mut d, 0x84, 0x400);
    d[0x400..0x400 + fp.len()].copy_from_slice(&fp);

    // Titelsuchtabelle
    let mut tt = vec![0u8; 8 + v.titles.len() * 12];
    put16(&mut tt, 0, v.titles.len() as u16);
    let last = (tt.len() - 1) as u32;
    put32(&mut tt, 4, last);
    for (i, t) in v.titles.iter().enumerate() {
        let e = 8 + i * 12;
        tt[e] = t.playback_type;
        tt[e + 1] = t.angles;
        put16(&mut tt, e + 2, t.chapters);
        tt[e + 6] = t.vts;
        tt[e + 7] = t.vts_ttn;
        put32(&mut tt, e + 8, t.vts_sector);
    }

    // Eigenschaften der Titelsets
    const ENTRY: usize = 0x308;
    let n = v.vts_attrs.len();
    let head = 8 + n * 4;
    let mut atrt = vec![0u8; head + n * ENTRY];
    put16(&mut atrt, 0, n as u16);
    let last = (atrt.len() - 1) as u32;
    put32(&mut atrt, 4, last);
    for (i, a) in v.vts_attrs.iter().enumerate() {
        let off = head + i * ENTRY;
        put32(&mut atrt, 8 + i * 4, off as u32);
        put32(&mut atrt, off, (ENTRY - 1) as u32);
        let count = a.len().min(ENTRY - 8);
        atrt[off + 8..off + 8 + count].copy_from_slice(&a[..count]);
    }

    let place = |d: &mut Vec<u8>, table: &[u8], at: usize| {
        pad_sector(d);
        let sector = (d.len() / SECTOR) as u32;
        put32(d, at, sector);
        d.extend_from_slice(table);
    };
    place(&mut d, &tt, 0xC4);
    if let Some(m) = &v.menu {
        place(&mut d, &pgci_ut_bytes(m), 0xC8);
    }
    place(&mut d, &atrt, 0xD0);
    if let Some(m) = &v.menu {
        place(&mut d, &cadt_bytes(&m.cells), 0xD8);
        place(&mut d, &admap_bytes(&m.admap), 0xDC);
    }
    pad_sector(&mut d);

    let ifo = (d.len() / SECTOR) as u32;
    let menu = v.menu.as_ref().map(|m| m.sectors).unwrap_or(0);
    put32(&mut d, 0x1C, ifo - 1);
    put32(&mut d, 0x0C, ifo * 2 + menu - 1);
    put32(&mut d, 0xC0, if menu > 0 { ifo } else { 0 });
    d
}

/// Trägt die Lage der Titelsets in die Titelsuchtabelle ein.
pub fn set_vts_starts(vmg: &mut AVmg, sizes_vts: &[u32], vmg_sectors: u32) {
    let mut starts = Vec::with_capacity(sizes_vts.len());
    let mut acc = vmg_sectors;
    for s in sizes_vts {
        starts.push(acc);
        acc += s;
    }
    for t in &mut vmg.titles {
        let idx = usize::from(t.vts).saturating_sub(1);
        if let Some(s) = starts.get(idx) {
            t.vts_sector = *s;
        }
    }
}
