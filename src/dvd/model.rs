//! Aufbereitete Sicht auf eine DVD: Titel, Menüs und Zellen samt Größen.

use std::collections::{BTreeMap, HashMap};

use super::ifo::{AudioAttr, Pgc, SubAttr, VideoAttr, Vmg, Vts};
use super::source::{Source, Space};
use super::vob::{classify, is_nav, is_scrambled, Nav, Stream};
use super::{Res, SECTOR};

/// Zelle eindeutig innerhalb einer Zone.
pub type CellKey = (Space, u16, u8);

/// Gemessene Größen je Datenstrom, in Sektoren (beim Stichprobenscan hochgerechnet).
#[derive(Clone, Copy, Debug, Default)]
pub struct CellStats {
    pub total: f64,
    pub nav: f64,
    pub video: f64,
    pub audio: [f64; 8],
    pub sub: [f64; 32],
    pub other: f64,
}

impl CellStats {
    pub fn add(&mut self, o: &CellStats) {
        self.total += o.total;
        self.nav += o.nav;
        self.video += o.video;
        self.other += o.other;
        for i in 0..8 {
            self.audio[i] += o.audio[i];
        }
        for i in 0..32 {
            self.sub[i] += o.sub[i];
        }
    }

    pub fn scale(&mut self, f: f64) {
        self.total *= f;
        self.nav *= f;
        self.video *= f;
        self.other *= f;
        for v in &mut self.audio {
            *v *= f;
        }
        for v in &mut self.sub {
            *v *= f;
        }
    }
}

/// Eine Tonspur, wie sie die Oberfläche zeigt.
#[derive(Clone, Debug)]
pub struct AudioTrack {
    pub logical: usize,
    pub phys: u8,
    pub attr: AudioAttr,
}

#[derive(Clone, Debug)]
pub struct SubTrack {
    pub logical: usize,
    pub phys: Vec<u8>,
    pub attr: SubAttr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Category {
    MainMovie,
    Extra,
}

#[derive(Clone, Debug)]
pub struct Chapter {
    pub start_ms: u64,
    pub first_cell_idx: usize,
}

/// Eine Zelle in Abspielreihenfolge innerhalb eines Titels.
#[derive(Clone, Debug)]
pub struct PlayCell {
    pub pgc: usize,
    pub cell_idx: usize,
    pub vob_id: u16,
    pub cell_id: u8,
    pub ms: u64,
    pub first_sector: u32,
    pub last_sector: u32,
}

#[derive(Clone, Debug)]
pub struct Title {
    pub number: usize,
    pub vts: u8,
    pub vts_ttn: u8,
    pub angles: u8,
    pub ms: u64,
    pub category: Category,
    pub chapters: Vec<Chapter>,
    pub cells: Vec<PlayCell>,
    pub pgcs: Vec<usize>,
    pub audio: Vec<AudioTrack>,
    pub subs: Vec<SubTrack>,
    pub video: VideoAttr,
}

/// Menübereich (Hauptmenü der Disc oder Menü eines Titelsets).
#[derive(Clone, Debug)]
pub struct MenuGroup {
    pub vts: u8,
    pub ms: u64,
    pub pgc_count: usize,
    pub sectors: u64,
    pub video: VideoAttr,
}

/// Zellen, auf die keine Programmkette verweist.
#[derive(Clone, Debug)]
pub struct UnusedGroup {
    pub vts: u8,
    pub cells: Vec<(u16, u8)>,
}

#[derive(Clone)]
pub struct Disc {
    pub source: Source,
    pub vmg: Vmg,
    pub vts: BTreeMap<u8, Vts>,
    pub titles: Vec<Title>,
    pub menus: Vec<MenuGroup>,
    pub unused: Vec<UnusedGroup>,
    /// Welche Zelle zu welchem Titel gehört (erster, also wichtigster Treffer).
    pub cell_owner: HashMap<CellKey, usize>,
    pub stats: HashMap<CellKey, CellStats>,
    pub analysed_full: bool,
    pub total_sectors: u64,
    pub scrambled: bool,
}

/// Fortschritts-Rückruf: (Anteil 0..1, Text).
pub type Progress<'a> = &'a mut dyn FnMut(f32, &str) -> bool;

impl Disc {
    /// Liest alle Tabellen. Noch kein Zugriff auf die Videodaten.
    pub fn open(source: Source) -> Res<Disc> {
        let vmg = Vmg::parse(source.read_file("VIDEO_TS.IFO")?)?;
        let mut vts = BTreeMap::new();
        for n in 1..=vmg.nr_vts.min(99) as u8 {
            if !source.has_vts(n) {
                continue;
            }
            let v = Vts::parse(n, source.read_file(&Source::ifo_name(n))?)?;
            vts.insert(n, v);
        }
        if vts.is_empty() {
            return Err(tr!("Die DVD enthält keine lesbaren Titelsets.", "The DVD contains no readable title sets.").into());
        }
        let mut titles = Vec::new();
        for (i, tt) in vmg.titles.iter().enumerate() {
            let Some(v) = vts.get(&tt.vts) else { continue };
            let Some(ptt) = v.ptt.get(tt.vts_ttn.saturating_sub(1) as usize) else { continue };
            titles.push(build_title(i + 1, tt.vts, tt.vts_ttn, tt.angles, ptt, v));
        }
        // Längster Titel = Hauptfilm, der Rest sind Extras
        if let Some(main) = titles.iter().enumerate().max_by_key(|(_, t)| t.ms).map(|(i, _)| i) {
            titles[main].category = Category::MainMovie;
        }
        titles.sort_by_key(|t| (t.category != Category::MainMovie, t.number));

        let mut cell_owner = HashMap::new();
        for (idx, t) in titles.iter().enumerate() {
            for c in &t.cells {
                cell_owner.entry((Space::title(t.vts), c.vob_id, c.cell_id)).or_insert(idx);
            }
        }

        let mut menus = Vec::new();
        let mut total = 0u64;
        {
            let r = source.reader(Space::menu(0))?;
            total += r.sectors();
            if r.sectors() > 0 {
                menus.push(MenuGroup {
                    vts: 0,
                    ms: vmg.menu_pgcs.iter().map(|p| p.ms).sum(),
                    pgc_count: vmg.menu_pgcs.len(),
                    sectors: r.sectors(),
                    video: vmg.video_menu,
                });
            }
        }
        let mut unused = Vec::new();
        for (n, v) in &vts {
            let r = source.reader(Space::menu(*n))?;
            total += r.sectors();
            if r.sectors() > 0 {
                menus.push(MenuGroup {
                    vts: *n,
                    ms: v.menu_pgcs.iter().map(|p| p.ms).sum(),
                    pgc_count: v.menu_pgcs.len(),
                    sectors: r.sectors(),
                    video: VideoAttr::parse(&v.raw[0x100..0x102]),
                });
            }
            let r = source.reader(Space::title(*n))?;
            total += r.sectors();
            let mut cells: Vec<(u16, u8)> = Vec::new();
            for c in &v.cells {
                let key = (Space::title(*n), c.vob_id, c.cell_id);
                if !cell_owner.contains_key(&key) && !cells.contains(&(c.vob_id, c.cell_id)) {
                    cells.push((c.vob_id, c.cell_id));
                }
            }
            if !cells.is_empty() {
                unused.push(UnusedGroup { vts: *n, cells });
            }
        }

        Ok(Disc {
            source,
            vmg,
            vts,
            titles,
            menus,
            unused,
            cell_owner,
            stats: HashMap::new(),
            analysed_full: false,
            total_sectors: total,
            scrambled: false,
        })
    }

    /// Analyse: misst, wie viel Platz Bild, Ton und Untertitel je Zelle belegen.
    /// `full` liest alle Daten, sonst genügt eine Stichprobe.
    pub fn analyse(&mut self, full: bool, progress: Progress) -> Res<bool> {
        let mut spaces: Vec<(Space, Vec<u32>, Vec<(u16, u8, u32, u32)>)> = Vec::new();
        let conv = |c: &[super::ifo::CellAddr]| -> Vec<(u16, u8, u32, u32)> {
            c.iter().map(|a| (a.vob_id, a.cell_id, a.start, a.last)).collect()
        };
        spaces.push((Space::menu(0), self.vmg.menu_admap.clone(), conv(&self.vmg.menu_cells)));
        for (n, v) in &self.vts {
            spaces.push((Space::menu(*n), v.menu_admap.clone(), conv(&v.menu_cells)));
            spaces.push((Space::title(*n), v.admap.clone(), conv(&v.cells)));
        }
        let budget: u64 = spaces.iter().map(|s| s.1.len() as u64).sum::<u64>().max(1);
        let mut seen = 0u64;
        let mut stats: HashMap<CellKey, CellStats> = HashMap::new();
        let mut scrambled = false;
        let mut buf = Vec::new();

        for (space, admap, cells) in &spaces {
            let mut reader = self.source.reader(*space)?;
            let total = reader.sectors();
            if total == 0 {
                continue;
            }
            // Schnellanalyse: wenige Stichproben genügen, denn Ton und Untertitel haben eine
            // nahezu feste Datenrate; der Rest folgt aus Zellgröße und Spieldauer.
            let sample = if full { usize::MAX } else if space.menu { 8 } else { 24 };
            let mut sample_secs = 0.0f64;
            let mut timed_samples = 0usize;
            // Welche Spuren es in welcher Zelle gibt, und wie lange die Stichproben je Spur liefen
            // Menüs tragen ihre Tonspuren oft nicht in die Programmkette ein; dort gilt alles.
            let streams = if space.menu { HashMap::new() } else { cell_streams(&self.vmg, &self.vts, *space) };
            let mut audio_secs = [0.0f64; 8];
            let mut sub_secs = [0.0f64; 32];
            let step = if admap.len() > sample { admap.len() as f64 / sample as f64 } else { 1.0 };
            let mut measured: HashMap<(u16, u8), CellStats> = HashMap::new();
            let mut overall = CellStats::default();
            let mut pos = 0.0f64;
            let mut last_i = usize::MAX;
            while (pos as usize) < admap.len() {
                let i = pos as usize;
                pos += step;
                if i == last_i {
                    continue;
                }
                last_i = i;
                let start = u64::from(admap[i]);
                let end = admap.get(i + 1).map(|v| u64::from(*v)).unwrap_or(total);
                if end <= start || start >= total {
                    continue;
                }
                let count = (end - start).min(16384) as usize;
                reader.read(start, count, &mut buf)?;
                let mut st = CellStats::default();
                let mut id = None;
                for s in buf.chunks_exact(SECTOR) {
                    st.total += 1.0;
                    match classify(s) {
                        Stream::Nav => {
                            st.nav += 1.0;
                            if id.is_none() && is_nav(s) {
                                id = Some((Nav::vob_id(s), Nav::cell_id(s)));
                            }
                        }
                        Stream::Video => {
                            st.video += 1.0;
                            scrambled |= is_scrambled(s);
                        }
                        Stream::Audio(a) => {
                            st.audio[a as usize] += 1.0;
                            scrambled |= is_scrambled(s);
                        }
                        Stream::Sub(a) => st.sub[a as usize] += 1.0,
                        _ => st.other += 1.0,
                    }
                }
                if let Some(first) = buf.get(..SECTOR) {
                    let (s, e) = (super::be32(first, super::vob::PCI_OFF + 12), super::be32(first, super::vob::PCI_OFF + 16));
                    if e > s && e - s < 900_000 {
                        let secs = f64::from(e - s) / 90_000.0;
                        sample_secs += secs;
                        timed_samples += 1;
                        let allowed = id.and_then(|k| streams.get(&k));
                        for k in 0..8 {
                            if allowed.is_none_or(|a| a.0[k]) {
                                audio_secs[k] += secs;
                            }
                        }
                        for k in 0..32 {
                            if allowed.is_none_or(|a| a.1[k]) {
                                sub_secs[k] += secs;
                            }
                        }
                    }
                }
                overall.add(&st);
                if let Some(k) = id {
                    measured.entry(k).or_default().add(&st);
                }
                seen += 1;
                if seen % 16 == 0 {
                    let frac = seen as f32 / budget as f32;
                    let done_frac = if full { frac } else { frac.min(1.0).sqrt() };
                    if !progress(done_frac.min(0.99), tr!("Analysiere die DVD …", "Analysing the DVD …")) {
                        return Err(tr!("Abgebrochen", "Cancelled").into());
                    }
                }
            }

            // Pro Zelle: tatsächliche Länge aus der Adresstabelle, Aufteilung aus der Messung.
            let mut sizes: HashMap<(u16, u8), f64> = HashMap::new();
            for (vob, cell, start, last) in cells {
                *sizes.entry((*vob, *cell)).or_default() += f64::from(last.saturating_sub(*start) + 1);
            }
            let cell_ms = cell_durations(&self.vmg, &self.vts, *space);
            let mean_vobu = if timed_samples > 0 { sample_secs / timed_samples as f64 } else { 0.5 };
            for (key, size) in sizes {
                if !full && sample_secs > 0.0 && overall.total > 0.0 {
                    // VOBUs der Zelle zählen, Spieldauer aus der Tabelle
                    let mut vobus = 0usize;
                    for (vob, cell, start, last) in cells {
                        if (*vob, *cell) == key {
                            let a = admap.partition_point(|x| x < start);
                            let b = admap.partition_point(|x| x <= last);
                            vobus += b.saturating_sub(a);
                        }
                    }
                    let secs = match cell_ms.get(&key) {
                        Some(ms) if *ms > 0 => *ms as f64 / 1000.0,
                        _ => vobus as f64 * mean_vobu,
                    };
                    let mut st = CellStats { total: size, nav: vobus as f64, ..Default::default() };
                    let allowed = streams.get(&key);
                    for k in 0..8 {
                        if audio_secs[k] > 0.0 && allowed.is_none_or(|a| a.0[k]) {
                            st.audio[k] = overall.audio[k] / audio_secs[k] * secs;
                        }
                    }
                    for k in 0..32 {
                        if sub_secs[k] > 0.0 && allowed.is_none_or(|a| a.1[k]) {
                            st.sub[k] = overall.sub[k] / sub_secs[k] * secs;
                        }
                    }
                    let fixed = st.nav + st.audio.iter().sum::<f64>() + st.sub.iter().sum::<f64>();
                    if fixed < size * 0.97 {
                        st.video = size - fixed;
                        stats.insert((*space, key.0, key.1), st);
                        continue;
                    }
                }
                let base = measured.get(&key).copied().unwrap_or(overall);
                let mut st = base;
                if st.total > 0.0 {
                    st.scale(size / st.total);
                } else {
                    st.total = size;
                    st.video = size;
                }
                stats.insert((*space, key.0, key.1), st);
            }
        }
        self.stats = stats;
        self.analysed_full = full;
        self.scrambled = scrambled;
        Ok(scrambled)
    }

    /// Prüft stichprobenartig auf CSS-Kennung, schon vor der eigentlichen Analyse.
    pub fn check_scrambled(&self) -> bool {
        let mut buf = Vec::new();
        for (n, v) in &self.vts {
            let Ok(mut reader) = self.source.reader(Space::title(*n)) else { continue };
            let count = v.admap.len();
            for k in 0..3usize {
                if count == 0 {
                    break;
                }
                let at = v.admap[k * count / 3];
                if reader.read(u64::from(at), 16, &mut buf).is_err() {
                    continue;
                }
                if buf.chunks_exact(SECTOR).any(is_scrambled) {
                    return true;
                }
            }
        }
        false
    }

    pub fn stats_for(&self, key: &CellKey) -> CellStats {
        self.stats.get(key).copied().unwrap_or_default()
    }

    /// Größen eines Titels; Zellen zählen hier auch, wenn sie geteilt sind.
    pub fn title_stats(&self, t: &Title) -> CellStats {
        let mut s = CellStats::default();
        let mut seen: Vec<(u16, u8)> = Vec::new();
        for c in &t.cells {
            let k = (c.vob_id, c.cell_id);
            if seen.contains(&k) {
                continue;
            }
            seen.push(k);
            s.add(&self.stats_for(&(Space::title(t.vts), k.0, k.1)));
        }
        s
    }

    pub fn menu_stats(&self, vts: u8) -> CellStats {
        let mut s = CellStats::default();
        let space = Space::menu(vts);
        for (k, v) in &self.stats {
            if k.0 == space {
                s.add(v);
            }
        }
        s
    }

    pub fn unused_stats(&self, u: &UnusedGroup) -> CellStats {
        let mut s = CellStats::default();
        for (vob, cell) in &u.cells {
            s.add(&self.stats_for(&(Space::title(u.vts), *vob, *cell)));
        }
        s
    }

    pub fn is_pal(&self) -> bool {
        self.vts.values().next().map(|v| v.video.pal).unwrap_or(true)
    }
}

fn build_title(number: usize, vts_no: u8, ttn: u8, angles: u8, ptt: &[(u16, u16)], v: &Vts) -> Title {
    // Die Kapitel nennen die Programmketten des Titels; meist ist es genau eine.
    let mut pgcs: Vec<usize> = Vec::new();
    for (pgcn, _) in ptt {
        let idx = (*pgcn as usize).saturating_sub(1);
        if idx < v.pgcs.len() && !pgcs.contains(&idx) {
            pgcs.push(idx);
        }
    }
    // Weitere über "nächste PGC" verkettete Ketten gehören ebenfalls dazu.
    if let Some(&last) = pgcs.last() {
        let mut cur = last;
        for _ in 0..v.pgcs.len() {
            let next = v.pgcs[cur].next_pgc as usize;
            if next == 0 || next > v.pgcs.len() || pgcs.contains(&(next - 1)) {
                break;
            }
            pgcs.push(next - 1);
            cur = next - 1;
        }
    }

    let mut cells = Vec::new();
    let mut cell_start_ms = Vec::new();
    let mut ms = 0u64;
    let mut pgc_first_cell: HashMap<usize, usize> = HashMap::new();
    for &pi in &pgcs {
        let p = &v.pgcs[pi];
        pgc_first_cell.insert(pi, cells.len());
        for (ci, c) in p.cells.iter().enumerate() {
            // Blockwinkel: nur der erste Winkel zählt für die Dauer
            let angle_extra = c.block_mode >= 2;
            cell_start_ms.push(ms);
            cells.push(PlayCell {
                pgc: pi,
                cell_idx: ci,
                vob_id: c.vob_id,
                cell_id: c.cell_id,
                ms: c.ms,
                first_sector: c.first_sector,
                last_sector: c.last_sector,
            });
            if !angle_extra {
                ms += c.ms;
            }
        }
    }

    let mut chapters = Vec::new();
    for (pgcn, pgn) in ptt {
        let pi = (*pgcn as usize).saturating_sub(1);
        let Some(p) = v.pgcs.get(pi) else { continue };
        let Some(entry) = p.program_map.get((*pgn as usize).saturating_sub(1)) else { continue };
        let base = *pgc_first_cell.get(&pi).unwrap_or(&0);
        let idx = base + (*entry as usize).saturating_sub(1);
        if idx < cells.len() {
            chapters.push(Chapter { start_ms: cell_start_ms[idx], first_cell_idx: idx });
        }
    }
    if chapters.is_empty() && !cells.is_empty() {
        chapters.push(Chapter { start_ms: 0, first_cell_idx: 0 });
    }

    let main_pgc: Option<&Pgc> = pgcs.first().and_then(|i| v.pgcs.get(*i));
    let mut audio = Vec::new();
    let mut subs = Vec::new();
    if let Some(p) = main_pgc {
        for (i, attr) in v.audio.iter().enumerate() {
            let ctl = p.audio_ctl[i];
            if ctl & 0x8000 != 0 {
                audio.push(AudioTrack { logical: i, phys: ((ctl >> 8) & 7) as u8, attr: attr.clone() });
            }
        }
        for (i, attr) in v.subs.iter().enumerate() {
            let ctl = p.sub_ctl[i];
            if ctl & 0x8000_0000 != 0 {
                let mut phys = Vec::new();
                for s in [(ctl >> 24) & 0x1F, (ctl >> 16) & 0x1F, (ctl >> 8) & 0x1F, ctl & 0x1F] {
                    if !phys.contains(&(s as u8)) {
                        phys.push(s as u8);
                    }
                }
                subs.push(SubTrack { logical: i, phys, attr: attr.clone() });
            }
        }
    }

    Title {
        number,
        vts: vts_no,
        vts_ttn: ttn,
        angles,
        ms,
        category: Category::Extra,
        chapters,
        cells,
        pgcs,
        audio,
        subs,
        video: v.video,
    }
}

/// Spieldauer jeder Zelle einer Zone laut Programmketten (Millisekunden).
fn cell_durations(vmg: &Vmg, vts: &BTreeMap<u8, Vts>, space: Space) -> HashMap<(u16, u8), u64> {
    let pgcs: &[super::ifo::Pgc] = if space.vts == 0 {
        &vmg.menu_pgcs
    } else if let Some(v) = vts.get(&space.vts) {
        if space.menu { &v.menu_pgcs } else { &v.pgcs }
    } else {
        &[]
    };
    let mut out = HashMap::new();
    for p in pgcs {
        for c in &p.cells {
            let e = out.entry((c.vob_id, c.cell_id)).or_insert(0u64);
            *e = (*e).max(c.ms);
        }
    }
    out
}

/// Physische Ton- und Untertitelspuren je Zelle, laut den Programmketten, die sie abspielen.
fn cell_streams(vmg: &Vmg, vts: &BTreeMap<u8, Vts>, space: Space) -> HashMap<(u16, u8), ([bool; 8], [bool; 32])> {
    let pgcs: &[super::ifo::Pgc] = if space.vts == 0 {
        &vmg.menu_pgcs
    } else if let Some(v) = vts.get(&space.vts) {
        if space.menu { &v.menu_pgcs } else { &v.pgcs }
    } else {
        &[]
    };
    let mut out: HashMap<(u16, u8), ([bool; 8], [bool; 32])> = HashMap::new();
    for p in pgcs {
        let mut audio = [false; 8];
        let mut subs = [false; 32];
        for c in p.audio_ctl {
            if c & 0x8000 != 0 {
                audio[usize::from((c >> 8) & 7)] = true;
            }
        }
        for c in p.sub_ctl {
            if c & 0x8000_0000 != 0 {
                for s in [(c >> 24) & 0x1F, (c >> 16) & 0x1F, (c >> 8) & 0x1F, c & 0x1F] {
                    subs[s as usize] = true;
                }
            }
        }
        for c in &p.cells {
            let e = out.entry((c.vob_id, c.cell_id)).or_insert(([false; 8], [false; 32]));
            for k in 0..8 {
                e.0[k] |= audio[k];
            }
            for k in 0..32 {
                e.1[k] |= subs[k];
            }
        }
    }
    out
}

/// Zeit in lesbarer Form.
pub fn fmt_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
}
