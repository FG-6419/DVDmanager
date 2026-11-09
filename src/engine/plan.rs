//! Einstellungen und Größenrechnung: Was bleibt erhalten, wie stark wird verkleinert,
//! und passt alles auf den Rohling?

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::dvd::model::{CellKey, CellStats, Disc};
use crate::dvd::source::Space;
use crate::dvd::SECTOR;
use crate::mpeg2::requant::VideoOpts;

/// Geschätzter Restanteil, wenn ein Abschnitt nur als Standbild bleibt.
pub const STILL_RATIO: f64 = 0.12;
/// Zielgröße, wenn keine Grenze gelten soll.
pub const NO_TARGET: u64 = 1 << 40;
/// Reserve gegen Abweichungen beim Verkleinern.
pub const SAFETY: f64 = 0.992;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum VideoMode {
    /// Unverändert übernehmen
    Keep,
    /// So weit verkleinern, dass alles passt
    Auto,
    /// Fester Anteil (0.05..1.0)
    Custom(f32),
    /// Nur noch ein Standbild je Zelle
    Still,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Smoothing {
    Off,
    Low,
    Medium,
    High,
}

impl Smoothing {
    pub fn cutoff(self) -> u8 {
        match self {
            Smoothing::Off => 64,
            Smoothing::Low => 44,
            Smoothing::Medium => 30,
            Smoothing::High => 18,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Smoothing::Off => tr!("Aus", "Off"),
            Smoothing::Low => tr!("Leicht", "Light"),
            Smoothing::Medium => tr!("Mittel", "Medium"),
            Smoothing::High => tr!("Stark", "Strong"),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemId {
    Title(usize),
    Menu(u8),
    Unused(u8),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ItemSettings {
    pub video: VideoMode,
    pub smoothing: Smoothing,
    pub audio: [bool; 8],
    pub subs: [bool; 32],
}

impl Default for ItemSettings {
    fn default() -> Self {
        Self { video: VideoMode::Keep, smoothing: Smoothing::Off, audio: [true; 8], subs: [true; 32] }
    }
}

/// Schnitt innerhalb eines Eintrags. Zeiten zählen ab dem Beginn des ersten gewählten Kapitels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cut {
    pub start_ms: Option<u64>,
    pub end_ms: Option<u64>,
    /// Bildgenau schneiden; sonst am nächsten Schlüsselbild (Anfang der VOBU).
    pub exact: bool,
}

impl Cut {
    pub fn is_active(&self) -> bool {
        self.start_ms.is_some() || self.end_ms.is_some()
    }

    /// Schneidet der Schnitt wirklich etwas ab? Marken genau am Rand (vorbelegt) zählen nicht.
    pub fn is_trim(&self, total: u64) -> bool {
        self.start() > CUT_EDGE_MS || self.end().saturating_add(CUT_EDGE_MS) < total
    }

    /// Anfang und Ende, wobei Marken am Rand als "kein Schnitt" gelten.
    pub fn effective(&self, total: u64) -> (u64, u64) {
        let start = if self.start() <= CUT_EDGE_MS { 0 } else { self.start() };
        let end = if self.end().saturating_add(CUT_EDGE_MS) >= total { u64::MAX } else { self.end() };
        (start, end)
    }

    /// Vorbelegung: ganzer gewählter Bereich.
    pub fn whole(total: u64, exact: bool) -> Self {
        Self { start_ms: Some(0), end_ms: Some(total), exact }
    }

    pub fn start(&self) -> u64 {
        self.start_ms.unwrap_or(0)
    }

    pub fn end(&self) -> u64 {
        self.end_ms.unwrap_or(u64::MAX)
    }
}

/// Abstand zum Rand, ab dem eine Marke als Schnitt zählt (eine halbe Bilddauer).
pub const CUT_EDGE_MS: u64 = 20;

/// Ein Eintrag beim Neu-Zusammenstellen.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReauthorEntry {
    pub title: usize,
    pub first_chapter: usize,
    /// Einschließlich; Index in den Kapiteln des Titels.
    pub last_chapter: usize,
    pub settings: ItemSettings,
    pub cut: Cut,
    /// Eigene Kapitelmarken (Millisekunden, gleiche Zeitachse wie der Schnitt, also ab
    /// dem Beginn des ersten gewählten Kapitels). Kapitel 1 beginnt immer am Anfang und
    /// steht nicht in der Liste. `None`: die Kapitel der DVD bleiben.
    #[serde(default)]
    pub chapter_marks: Option<Vec<u64>>,
    /// Ein- und Ausblenden am Anfang und Ende des Eintrags
    #[serde(default)]
    pub fade: Fades,
}

/// Art einer Blende.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FadeKind {
    #[default]
    None,
    /// Aus Schwarz einblenden bzw. nach Schwarz abblenden
    Black,
    /// Aus Weiß bzw. nach Weiß
    White,
}

/// Blenden eines Eintrags.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Fades {
    pub fade_in: FadeKind,
    pub fade_out: FadeKind,
    /// Dauer jeder Blende in Millisekunden (0 = Standard)
    pub ms: u32,
}

/// Standarddauer einer Blende.
pub const FADE_DEFAULT_MS: u32 = 1_000;

impl Fades {
    pub fn is_active(&self) -> bool {
        self.fade_in != FadeKind::None || self.fade_out != FadeKind::None
    }

    pub fn duration_ms(&self) -> f64 {
        f64::from(if self.ms == 0 { FADE_DEFAULT_MS } else { self.ms })
    }

    /// Sichtbarkeit des Bildes (1 = voll, 0 = ganz Schwarz/Weiß) und Zielfarbe an einer
    /// Stelle `t` der Zeitachse des Eintrags, wenn der sichtbare Bereich `start..end` ist.
    /// `None`, wenn dort keine Blende wirkt.
    pub fn at(&self, t: f64, start: f64, end: f64) -> Option<(f32, FadeKind)> {
        let d = self.duration_ms().min(((end - start) / 2.0).max(1.0));
        let mut out: Option<(f32, FadeKind)> = None;
        if self.fade_in != FadeKind::None && t < start + d {
            let a = ((t - start) / d).clamp(0.0, 1.0) as f32;
            out = Some((a, self.fade_in));
        }
        if self.fade_out != FadeKind::None && t > end - d {
            let a = ((end - t) / d).clamp(0.0, 1.0) as f32;
            if out.is_none_or(|o| a < o.0) {
                out = Some((a, self.fade_out));
            }
        }
        out
    }
}

/// Mindestabstand zwischen zwei Kapitelmarken.
pub const MIN_CHAPTER_GAP_MS: u64 = 1_000;

impl ReauthorEntry {
    pub fn new(title: usize, first_chapter: usize, last_chapter: usize, settings: ItemSettings) -> Self {
        Self { title, first_chapter, last_chapter, settings, cut: Cut::default(), chapter_marks: None, fade: Fades::default() }
    }

    /// Kapitelanfänge der DVD im gewählten Bereich (ohne das erste Kapitel), auf der
    /// Zeitachse des Eintrags.
    pub fn original_marks(&self, disc: &Disc) -> Vec<u64> {
        let Some(t) = disc.titles.get(self.title) else { return Vec::new() };
        let base = t.chapters.get(self.first_chapter).map(|c| c.start_ms).unwrap_or(0);
        t.chapters
            .iter()
            .take(self.last_chapter + 1)
            .skip(self.first_chapter + 1)
            .map(|c| c.start_ms.saturating_sub(base))
            .collect()
    }

    /// Sortiert, entfernt doppelte und zu dicht liegende Marken sowie Marken am Anfang.
    pub fn normalize_marks(marks: &mut Vec<u64>) {
        marks.sort_unstable();
        let mut out: Vec<u64> = Vec::with_capacity(marks.len());
        for &m in marks.iter() {
            let last = out.last().copied().unwrap_or(0);
            if m >= MIN_CHAPTER_GAP_MS && m >= last + MIN_CHAPTER_GAP_MS {
                out.push(m);
            }
        }
        *marks = out;
    }

    /// Eigene Kapitelmarken auf der Zeitachse der Ausgabe (nach dem Schnitt).
    /// Marken außerhalb des geschnittenen Bereichs entfallen.
    pub fn output_marks(&self, disc: &Disc) -> Option<Vec<u64>> {
        let marks = self.chapter_marks.as_ref()?;
        let total = entry_duration_ms(disc, self);
        let (start, end) = self.cut.effective(total);
        let end = end.min(total);
        let mut out: Vec<u64> =
            marks.iter().filter(|m| **m > start && **m < end).map(|m| m - start).collect();
        Self::normalize_marks(&mut out);
        Some(out)
    }

    /// Anzahl der Kapitel, die der Eintrag in der neuen DVD hat.
    pub fn chapter_count(&self, disc: &Disc) -> usize {
        match self.output_marks(disc) {
            Some(m) => m.len() + 1,
            None => self.last_chapter + 1 - self.first_chapter.min(self.last_chapter),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode {
    FullDisc,
    Reauthor,
}

#[derive(Clone, Debug)]
pub struct Plan {
    pub mode: Mode,
    pub target_sectors: u64,
    pub items: HashMap<ItemId, ItemSettings>,
    pub reauthor: Vec<ReauthorEntry>,
    pub remove_puo: bool,
    pub region_free: bool,
    /// VOB-Dateien wie auf einer normalen DVD in Teile von knapp 1 GB zerlegen.
    pub split_vob: bool,
    /// Bildmodus für alles, was noch nicht eigens eingestellt ist (Standard: Original).
    pub default_video: VideoMode,
}

/// Eine einmal aufgelöste Entscheidung für eine Zelle.
#[derive(Clone, Copy, Debug)]
pub struct CellDecision {
    pub mode: VideoMode,
    pub smooth_cutoff: u8,
    pub keep_audio: [bool; 8],
    pub keep_sub: [bool; 32],
}

impl CellDecision {
    pub fn keep_all() -> Self {
        Self {
            mode: VideoMode::Keep,
            smooth_cutoff: 64,
            keep_audio: [true; 8],
            keep_sub: [true; 32],
        }
    }

    pub fn from_settings(s: &ItemSettings) -> Self {
        Self {
            mode: s.video,
            smooth_cutoff: s.smoothing.cutoff(),
            keep_audio: s.audio,
            keep_sub: s.subs,
        }
    }

    pub fn video_opts(&self, auto_ratio: f32) -> VideoOpts {
        match self.mode {
            VideoMode::Keep => VideoOpts { ratio: 1.0, smooth_cutoff: 64, still: false },
            VideoMode::Auto => VideoOpts { ratio: auto_ratio, smooth_cutoff: self.smooth_cutoff, still: false },
            VideoMode::Custom(r) => VideoOpts { ratio: r, smooth_cutoff: self.smooth_cutoff, still: false },
            VideoMode::Still => VideoOpts { ratio: STILL_RATIO as f32, smooth_cutoff: 64, still: true },
        }
    }

    fn fixed_sectors(&self, s: &CellStats) -> f64 {
        let mut v = s.nav;
        for i in 0..8 {
            if self.keep_audio[i] {
                v += s.audio[i];
            }
        }
        for i in 0..32 {
            if self.keep_sub[i] {
                v += s.sub[i];
            }
        }
        v
    }
}

#[derive(Clone, Debug, Default)]
pub struct Estimate {
    pub video: f64,
    pub audio: f64,
    pub subs: f64,
    pub overhead: f64,
    pub total: f64,
    pub source_total: f64,
    /// Anteil, auf den "Automatisch" verkleinert
    pub auto_ratio: f32,
    pub fits: bool,
    pub auto_video_in: f64,
}

impl Plan {
    pub fn new(target_sectors: u64) -> Self {
        Self {
            mode: Mode::FullDisc,
            target_sectors,
            items: HashMap::new(),
            reauthor: Vec::new(),
            remove_puo: false,
            region_free: false,
            split_vob: true,
            default_video: VideoMode::Keep,
        }
    }

    /// Plan für die Ausgabe gewählter Titel in eine einzige Datei. selected bezieht sich
    /// auf die Titel der DVD oder, beim Neu-Zusammenstellen, auf die Einträge der Liste.
    pub fn for_single_file(&self, disc: &Disc, selected: &[bool], keep_target: bool) -> Plan {
        let is_on = |i: usize| selected.get(i).copied().unwrap_or(false);
        let entries: Vec<ReauthorEntry> = match self.mode {
            Mode::FullDisc => disc
                .titles
                .iter()
                .enumerate()
                .filter(|(i, _)| is_on(*i))
                .map(|(i, t)| ReauthorEntry {
                    title: i,
                    first_chapter: 0,
                    last_chapter: t.chapters.len().saturating_sub(1),
                    settings: self.settings(ItemId::Title(i)),
                    cut: Cut::default(),
                    chapter_marks: None,
                    fade: Fades::default(),
                })
                .collect(),
            Mode::Reauthor => self
                .reauthor
                .iter()
                .enumerate()
                .filter(|(i, _)| is_on(*i))
                .map(|(_, e)| e.clone())
                .collect(),
        };
        let mut plan = self.clone();
        plan.mode = Mode::Reauthor;
        plan.reauthor = entries;
        if !keep_target {
            plan.target_sectors = NO_TARGET;
        }
        plan
    }

    pub fn settings(&self, id: ItemId) -> ItemSettings {
        self.items.get(&id).cloned().unwrap_or(ItemSettings { video: self.default_video, ..Default::default() })
    }

    /// Behält in allen Titeln nur Ton und Untertitel einer Sprache, sofern es sie gibt.
    pub fn keep_only_language(&mut self, disc: &Disc, lang: &str) {
        for (i, t) in disc.titles.iter().enumerate() {
            let id = ItemId::Title(i);
            let mut s = self.settings(id);
            if t.audio.iter().any(|a| a.attr.lang == lang) {
                for a in &t.audio {
                    s.audio[a.phys as usize] = a.attr.lang == lang;
                }
            }
            if t.subs.iter().any(|a| a.attr.lang == lang) {
                for a in &t.subs {
                    for p in &a.phys {
                        s.subs[*p as usize] = a.attr.lang == lang;
                    }
                }
            }
            self.items.insert(id, s);
        }
    }

    /// Entscheidung für eine Zelle im Ganze-DVD-Modus.
    pub fn decide(&self, disc: &Disc, key: &CellKey) -> CellDecision {
        let (space, vob, cell) = *key;
        if space.menu {
            return CellDecision::from_settings_menu(&self.settings(ItemId::Menu(space.vts)));
        }
        if let Some(&owner) = disc.cell_owner.get(&(space, vob, cell)) {
            return CellDecision::from_settings(&self.settings(ItemId::Title(owner)));
        }
        CellDecision::from_settings_menu(&self.settings(ItemId::Unused(space.vts)))
    }

    /// Standardwerte: Hauptfilm-Sprachfilter übernehmen wir aus der Oberfläche.
    pub fn estimate(&self, disc: &Disc) -> Estimate {
        let mut e = Estimate { auto_ratio: 1.0, ..Default::default() };
        let mut fixed_video = 0.0;
        let mut auto_video = 0.0;
        let mut account = |stats: &CellStats, d: &CellDecision, e: &mut Estimate| {
            e.source_total += stats.total;
            e.overhead += stats.nav;
            for i in 0..8 {
                if d.keep_audio[i] {
                    e.audio += stats.audio[i];
                }
            }
            for i in 0..32 {
                if d.keep_sub[i] {
                    e.subs += stats.sub[i];
                }
            }
            match d.mode {
                VideoMode::Keep => fixed_video += stats.video,
                VideoMode::Auto => auto_video += stats.video,
                VideoMode::Custom(r) => fixed_video += stats.video * f64::from(r),
                VideoMode::Still => fixed_video += stats.video * STILL_RATIO,
            }
        };

        match self.mode {
            Mode::FullDisc => {
                for (key, stats) in &disc.stats {
                    let d = self.decide(disc, key);
                    account(stats, &d, &mut e);
                }
                e.overhead += ifo_overhead(disc);
            }
            Mode::Reauthor => {
                for entry in &self.reauthor {
                    let d = CellDecision::from_settings(&entry.settings);
                    let keep = entry_kept_fraction(disc, entry);
                    for key in reauthor_cells(disc, entry) {
                        let mut stats = disc.stats_for(&key);
                        stats.scale(keep);
                        account(&stats, &d, &mut e);
                    }
                    e.overhead += 40.0;
                }
                e.overhead += 10.0;
            }
        }

        let target = self.target_sectors as f64 * SAFETY;
        let fixed = e.overhead + e.audio + e.subs + fixed_video;
        let room = target - fixed;
        e.auto_video_in = auto_video;
        let ratio = if auto_video > 0.0 { (room / auto_video).clamp(0.05, 1.0) } else { 1.0 };
        e.auto_ratio = ratio as f32;
        e.video = fixed_video + auto_video * ratio;
        e.total = e.video + e.audio + e.subs + e.overhead;
        e.fits = e.total <= self.target_sectors as f64 + 0.5;
        e
    }

    /// Plan-Werte passend zum, was gerade ausgewählt ist.
    pub fn out_stats_for_item(&self, stats: &CellStats, s: &ItemSettings, auto_ratio: f32) -> f64 {
        let d = CellDecision::from_settings(s);
        let v = match s.video {
            VideoMode::Keep => stats.video,
            VideoMode::Auto => stats.video * f64::from(auto_ratio),
            VideoMode::Custom(r) => stats.video * f64::from(r),
            VideoMode::Still => stats.video * STILL_RATIO,
        };
        d.fixed_sectors(stats) + v
    }
}

impl CellDecision {
    /// Menüs und ungenutzte Bereiche behalten immer alle Spuren.
    pub fn from_settings_menu(s: &ItemSettings) -> Self {
        Self {
            mode: s.video,
            smooth_cutoff: s.smoothing.cutoff(),
            keep_audio: [true; 8],
            keep_sub: [true; 32],
        }
    }
}

/// Sektoren der Tabellen (IFO und Sicherungskopie).
pub fn ifo_overhead(disc: &Disc) -> f64 {
    let mut n = disc.vmg.raw.len() * 2;
    for v in disc.vts.values() {
        n += v.raw.len() * 2;
    }
    (n / SECTOR) as f64
}

/// Zellen eines Neu-Zusammenstellen-Eintrags in Abspielreihenfolge (ohne Zusatzwinkel).
pub fn reauthor_cells(disc: &Disc, entry: &ReauthorEntry) -> Vec<CellKey> {
    let mut out = Vec::new();
    for (_, _, key) in reauthor_play_cells(disc, entry) {
        out.push(key);
    }
    out
}

/// (PGC-Index, Zellenindex in der PGC, Schlüssel)
pub fn reauthor_play_cells(disc: &Disc, entry: &ReauthorEntry) -> Vec<(usize, usize, CellKey)> {
    let Some(t) = disc.titles.get(entry.title) else { return Vec::new() };
    let Some(v) = disc.vts.get(&t.vts) else { return Vec::new() };
    let first = t.chapters.get(entry.first_chapter).map(|c| c.first_cell_idx).unwrap_or(0);
    let end = t.chapters.get(entry.last_chapter + 1).map(|c| c.first_cell_idx).unwrap_or(t.cells.len());
    let mut out = Vec::new();
    for pc in t.cells.iter().take(end).skip(first) {
        let Some(cell) = v.pgcs.get(pc.pgc).and_then(|p| p.cells.get(pc.cell_idx)) else { continue };
        if cell.block_mode >= 2 {
            continue; // Weitere Kamerawinkel entfallen beim Neu-Zusammenstellen
        }
        out.push((pc.pgc, pc.cell_idx, (Space::title(t.vts), pc.vob_id, pc.cell_id)));
    }
    out
}

/// Dauer eines Eintrags vor dem Schnitt (ohne zusätzliche Kamerawinkel).
pub fn entry_duration_ms(disc: &Disc, entry: &ReauthorEntry) -> u64 {
    let Some(t) = disc.titles.get(entry.title) else { return 0 };
    let mut ms = 0;
    for (pgc, cell, _) in reauthor_play_cells(disc, entry) {
        if let Some(c) = t.cells.iter().find(|c| c.pgc == pgc && c.cell_idx == cell) {
            ms += c.ms;
        }
    }
    ms
}

/// Anteil, der nach dem Schnitt übrig bleibt.
pub fn entry_kept_fraction(disc: &Disc, entry: &ReauthorEntry) -> f64 {
    let total = entry_duration_ms(disc, entry);
    if !entry.cut.is_trim(total) {
        return 1.0;
    }
    if total == 0 {
        return 1.0;
    }
    let end = entry.cut.end().min(total);
    let start = entry.cut.start().min(end);
    (end - start) as f64 / total as f64
}

/// Gängige Rohlingsgrößen.
pub fn dvd5_sectors() -> u64 {
    2_295_104
}

pub fn dvd9_sectors() -> u64 {
    4_171_712
}

pub fn fmt_size(sectors: f64) -> String {
    let bytes = sectors * SECTOR as f64;
    let mb = bytes / (1024.0 * 1024.0);
    if mb >= 1024.0 {
        crate::i18n::decimal(format!("{:.2} GB", mb / 1024.0))
    } else if mb >= 10.0 {
        format!("{mb:.0} MB")
    } else {
        crate::i18n::decimal(format!("{mb:.1} MB"))
    }
}
