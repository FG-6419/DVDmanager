//! Führt das Backup aus: ganze DVD oder neu zusammengestellt, als Ordner, ISO oder
//! fertiges Abbild für den Windows-Brenner.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::time::Instant;

use crate::dvd::ifo::{self, CellAddr, SectorMap, TitleEntry};
use crate::dvd::iso;
use crate::dvd::model::{CellKey, Disc};
use crate::dvd::source::{FileRef, Source, Space};
use crate::dvd::vob::Nav;
use crate::dvd::{Res, SECTOR};
use crate::engine::author::{self, ACell, APgc, AVmg, AVts, TimeMap};
use crate::engine::plan::{
    ifo_overhead, CellDecision, Estimate, Mode, Plan, ReauthorEntry, VideoMode, SAFETY, STILL_RATIO,
};
use crate::engine::cut::{entry_stream, StreamEvent};
use crate::engine::inspect::{inspect, CheckReport, InspectOpts};
use crate::engine::transcode::{copy_raw, patch_nav, transcode_vobu, Retimer, SpaceWriter, VobuReader};
use crate::dvd::ifo::encode_time;
use crate::dvd::vob::DSI_OFF;
use crate::mpeg2::requant::Requant;

#[derive(Clone, Debug, PartialEq)]
pub enum Output {
    /// Zielordner; darin entsteht VIDEO_TS.
    Folder(PathBuf),
    Iso(PathBuf),
    /// ISO anlegen und danach den Windows-Brenner für dieses Laufwerk öffnen.
    Burn(PathBuf),
    /// Die Titel aus plan.reauthor nacheinander in eine einzige VOB-Datei schreiben.
    Vob(PathBuf),
    /// Wie Vob, aber als schlichter MPEG-2-Programmstrom: Bild und eine Tonspur,
    /// ohne Navigationspakete, Untertitel und Füllpakete. udio: gewünschte Spur.
    Mpeg { path: PathBuf, audio: Option<u8> },
    /// Wie Mpeg, aber als Matroska-Datei (MKV) mit Kapiteln.
    Mkv { path: PathBuf, audio: Option<u8> },
}

pub struct BackupRequest {
    pub disc: Arc<Disc>,
    pub plan: Plan,
    pub output: Output,
    pub deep_analysis: bool,
}

#[derive(Clone, Debug)]
pub enum BackupMsg {
    Stage(String),
    Progress {
        fraction: f32,
        sectors_out: u64,
        ratio_in: f64,
        current: String,
    },
    Done(BackupSummary),
    Failed(String),
    Cancelled,
}

#[derive(Clone, Debug)]
pub struct BackupSummary {
    pub path: PathBuf,
    pub sectors: u64,
    pub target_sectors: u64,
    pub source_sectors: f64,
    pub seconds: f64,
    pub fits: bool,
    pub burn_started: bool,
    /// Prüfung der Ausgabe, falls gewünscht
    pub check: Option<CheckReport>,
    /// Gespeichertes Übersichtsbild
    pub sheet: Option<PathBuf>,
    /// Hinweis, wenn Prüfung oder Übersichtsbild nicht geklappt haben
    pub extra_error: Option<String>,
    /// Anzahl geschriebener Dateien (Export als Einzeldateien); `path` ist dann der Ordner.
    pub files: usize,
}

/// Ergebnis von Prüfung und Übersichtsbild.
#[derive(Default)]
struct PostResult {
    check: Option<CheckReport>,
    sheet: Option<PathBuf>,
    error: Option<String>,
}

/// Liest die fertige Ausgabe noch einmal: Prüfung und Übersichtsbild.
fn post_process(plan: &Plan, groups: &[Vec<PathBuf>], continuous: bool, sheet_path: PathBuf, tx: &Sender<BackupMsg>, cancel: &AtomicBool) -> PostResult {
    let sheet = plan.sheet_secs.map(|s| (s, sheet_path));
    if !plan.verify && sheet.is_none() {
        return PostResult::default();
    }
    let stage = match (plan.verify, sheet.is_some()) {
        (true, true) => tr!("Ausgabe prüfen und Übersichtsbild erstellen", "Checking output and creating overview picture"),
        (true, false) => tr!("Ausgabe prüfen", "Checking output"),
        _ => tr!("Übersichtsbild erstellen", "Creating overview picture"),
    };
    let _ = tx.send(BackupMsg::Stage(stage.into()));
    let opts = InspectOpts { verify: plan.verify, continuous, sheet };
    let mut last = Instant::now();
    let mut progress = |f: f32| {
        if last.elapsed().as_millis() > 200 {
            last = Instant::now();
            let _ = tx.send(BackupMsg::Progress { fraction: f, sectors_out: 0, ratio_in: 1.0, current: stage.into() });
        }
        !cancel.load(Ordering::Relaxed)
    };
    match inspect(groups, &opts, &mut progress) {
        Ok((check, sheet)) => PostResult { check, sheet, error: None },
        Err(e) if cancel.load(Ordering::Relaxed) => {
            let _ = e;
            PostResult { error: Some(tr!("Prüfung abgebrochen; die Ausgabe ist vollständig.", "Check cancelled; the output is complete.").into()), ..Default::default() }
        }
        Err(e) => PostResult { error: Some(e), ..Default::default() },
    }
}

/// Titelsets eines VIDEO_TS-Ordners: je Titelset die VOB-Dateien des Films (ohne Menü).
fn title_vobs(video_dir: &Path) -> Vec<Vec<PathBuf>> {
    let mut sets: std::collections::BTreeMap<String, Vec<PathBuf>> = Default::default();
    if let Ok(rd) = std::fs::read_dir(video_dir) {
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_ascii_uppercase();
            // VTS_01_1.VOB bis VTS_01_9.VOB; _0 ist das Menü
            if name.len() == 12 && name.starts_with("VTS_") && name.ends_with(".VOB") && &name[7..8] != "0" {
                sets.entry(name[4..6].to_string()).or_default().push(ent.path());
            }
        }
    }
    sets.into_values()
        .map(|mut v| {
            v.sort();
            v
        })
        .collect()
}

/// Umfang der Quelle für die Anzeige „von ursprünglich …“: die ganze DVD oder beim Neu-
/// Zusammenstellen nur die gewählten Teile, nach dem Schnitt.
fn source_size(disc: &Disc, plan: &Plan) -> f64 {
    match plan.mode {
        Mode::FullDisc => disc.total_sectors as f64,
        Mode::Reauthor => plan
            .reauthor
            .iter()
            .map(|e| {
                let cells: f64 = crate::engine::plan::reauthor_cells(disc, e).iter().map(|k| disc.stats_for(k).total).sum();
                cells * crate::engine::plan::entry_kept_fraction(disc, e)
            })
            .sum(),
    }
}

/// Übersichtsbild neben der Ausgabe: gleicher Name, Endung .jpg.
fn sheet_path_for(output: &Output) -> PathBuf {
    match output {
        Output::Folder(p) => {
            let name = p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "DVD".into());
            p.with_file_name(format!("{name}.jpg"))
        }
        Output::Iso(p) | Output::Burn(p) | Output::Vob(p) | Output::Mpeg { path: p, .. } | Output::Mkv { path: p, .. } => p.with_extension("jpg"),
    }
}

/// Führt Buch, damit die Automatik den Rest passend verteilt.
struct Budget {
    target: f64,
    written: f64,
    rem_fixed: f64,
    rem_auto: f64,
}

impl Budget {
    fn new(plan: &Plan, est: &Estimate, ifo_sectors: f64) -> Self {
        let rem_auto = est.auto_video_in;
        let rem_fixed = est.total - est.auto_video_in * f64::from(est.auto_ratio) - ifo_sectors;
        Self {
            target: plan.target_sectors as f64 * SAFETY,
            written: ifo_sectors,
            rem_fixed: rem_fixed.max(0.0),
            rem_auto,
        }
    }

    fn ratio(&self) -> f32 {
        if self.rem_auto <= 1.0 {
            return 1.0;
        }
        let room = self.target - self.written - self.rem_fixed;
        (room / self.rem_auto).clamp(0.05, 1.0) as f32
    }

    fn account(&mut self, d: &CellDecision, video_in: u64, kept_other: u64, out_sectors: u64) {
        self.written += out_sectors as f64;
        let mut fixed = 1.0 + kept_other as f64;
        match d.mode {
            VideoMode::Auto => self.rem_auto = (self.rem_auto - video_in as f64).max(0.0),
            VideoMode::Keep => fixed += video_in as f64,
            VideoMode::Custom(r) => fixed += video_in as f64 * f64::from(r),
            VideoMode::Still => fixed += video_in as f64 * STILL_RATIO,
        }
        self.rem_fixed = (self.rem_fixed - fixed).max(0.0);
    }
}

struct Ctx<'a> {
    tx: &'a Sender<BackupMsg>,
    cancel: &'a AtomicBool,
    done_in: u64,
    total_in: u64,
    last_report: Instant,
    written: u64,
    /// Anteil am Gesamtfortschritt, den das Umwandeln ausmacht.
    span: f32,
}

impl Ctx<'_> {
    fn check(&self) -> Res<()> {
        if self.cancel.load(Ordering::Relaxed) {
            return Err(CANCELLED.into());
        }
        Ok(())
    }

    fn report(&mut self, ratio: f64, current: &str) {
        if self.last_report.elapsed().as_millis() < 120 {
            return;
        }
        self.last_report = Instant::now();
        let f = self.done_in as f32 / self.total_in.max(1) as f32;
        let _ = self.tx.send(BackupMsg::Progress {
            fraction: f * self.span,
            sectors_out: self.written,
            ratio_in: ratio,
            current: current.to_string(),
        });
    }
}

pub(crate) const CANCELLED: &str = "__abgebrochen__";

fn write_with_bup(dir: &Path, name_ifo: &str, data: &[u8]) -> Res<(PathBuf, PathBuf)> {
    let mut d = data.to_vec();
    let rem = d.len() % SECTOR;
    if rem != 0 {
        d.resize(d.len() + SECTOR - rem, 0);
    }
    let ifo_path = dir.join(name_ifo);
    crate::dvd::write_output(&ifo_path, &d)?;
    let bup_path = dir.join(format!("{}.BUP", name_ifo.trim_end_matches(".IFO")));
    crate::dvd::write_output(&bup_path, &d)?;
    Ok((ifo_path, bup_path))
}

#[allow(clippy::too_many_arguments)]
fn process_space(
    disc: &Disc,
    plan: &Plan,
    space: Space,
    writer: &mut SpaceWriter,
    budget: &mut Budget,
    ctx: &mut Ctx,
    label: &str,
) -> Res<SectorMap> {
    let reader = disc.source.reader(space)?;
    let total = reader.sectors();
    let mut map = SectorMap::default();
    if total == 0 {
        return Ok(map);
    }
    let mut vr = VobuReader::new(reader, 0, total);
    let mut rq = Requant::new();
    let mut last_key: Option<CellKey> = None;
    let mut decision = CellDecision::keep_all();
    let mut new_pos = 0u32;

    while let Some((start, data)) = vr.next()? {
        ctx.check()?;
        let key: CellKey = (space, Nav::vob_id(&data), Nav::cell_id(&data));
        if last_key != Some(key) {
            decision = plan.decide(disc, &key);
            rq.new_cell();
            last_key = Some(key);
        }
        let opts = decision.video_opts(budget.ratio());
        let out = transcode_vobu(&data, &decision, opts, &mut rq);
        let in_sectors = (data.len() / SECTOR) as u64;
        let out_sectors = (out.data.len() / SECTOR) as u64;
        writer.write(&out.data)?;
        map.vobus.push((
            start as u32,
            (start + in_sectors - 1) as u32,
            new_pos,
            new_pos + out_sectors as u32 - 1,
        ));
        new_pos += out_sectors as u32;
        budget.account(&decision, out.video_in, out.kept_other, out_sectors);
        ctx.written += out_sectors;
        ctx.done_in += in_sectors;
        ctx.report(f64::from(opts.ratio), label);
    }

    if map.vobus.is_empty() {
        // Ohne Navigationspakete bleibt nur die unveränderte Kopie.
        let mut r = disc.source.reader(space)?;
        copy_raw(&mut r, writer)?;
        map.vobus.push((0, total as u32 - 1, 0, total as u32 - 1));
        map.new_total = total as u32;
        writer.finish()?;
        ctx.done_in += total;
        ctx.written += total;
        return Ok(map);
    }
    writer.finish()?;
    map.new_total = new_pos;
    let snapshot = map.clone();
    writer.patch_all(&map, |s, i| {
        patch_nav(s, snapshot.vobus[i], &snapshot, plan.remove_puo, false);
    })?;
    Ok(map)
}

fn run_full_disc(disc: &Disc, plan: &Plan, dir: &Path, ctx: &mut Ctx, files: &mut Vec<PathBuf>) -> Res<u64> {
    let est = plan.estimate(disc);
    let ifo_sec = ifo_overhead(disc);
    let mut budget = Budget::new(plan, &est, ifo_sec);
    let mut written_total = 0u64;

    // Erst das Hauptmenü, damit seine Größe für die Lage der Titelsets feststeht.
    let _ = ctx.tx.send(BackupMsg::Stage(tr!("Hauptmenü", "Main menu").into()));
    let mut w = SpaceWriter::new(dir, "VIDEO_TS.VOB", true, false);
    let vmg_menu = process_space(disc, plan, Space::menu(0), &mut w, &mut budget, ctx, tr!("Hauptmenü", "Main menu"))?;
    let vmg_vob_paths = w.paths().to_vec();
    written_total += u64::from(vmg_menu.new_total);

    let mut vts_starts: Vec<u32> = Vec::new();
    let vmg_ifo = ifo::ifo_sectors(&disc.vmg.raw);
    let mut acc = vmg_ifo * 2 + vmg_menu.new_total;
    let mut vts_files: Vec<PathBuf> = Vec::new();

    for (n, v) in &disc.vts {
        ctx.check()?;
        let label = trf!("Titelset {n}", "Title set {n}");
        let _ = ctx.tx.send(BackupMsg::Stage(label.clone()));

        let mut wm = SpaceWriter::new(dir, &format!("VTS_{n:02}_0.VOB"), true, false);
        let menu = process_space(disc, plan, Space::menu(*n), &mut wm, &mut budget, ctx, &label)?;
        let mut wt = SpaceWriter::new(dir, &format!("VTS_{n:02}_"), false, plan.split_vob);
        let title = process_space(disc, plan, Space::title(*n), &mut wt, &mut budget, ctx, &label)?;

        let mut raw = v.raw.clone();
        ifo::patch_vts(&mut raw, &menu, &title, plan.remove_puo);
        // Abgewählte Spuren aus den Programmketten nehmen
        let mut done: Vec<usize> = Vec::new();
        for (idx, t) in disc.titles.iter().enumerate() {
            if t.vts != *n {
                continue;
            }
            let s = plan.settings(crate::engine::plan::ItemId::Title(idx));
            for pi in &t.pgcs {
                if done.contains(pi) {
                    continue;
                }
                done.push(*pi);
                if let Some(p) = v.pgcs.get(*pi) {
                    ifo::disable_streams(&mut raw, p.offset, &s.audio, &s.subs);
                }
            }
        }

        let (ifo_path, bup_path) = write_with_bup(dir, &format!("VTS_{n:02}_0.IFO"), &raw)?;
        vts_files.push(ifo_path);
        vts_files.extend_from_slice(wm.paths());
        vts_files.extend_from_slice(wt.paths());
        vts_files.push(bup_path);

        let ifo = ifo::ifo_sectors(&raw);
        let vts_size = ifo * 2 + menu.new_total + title.new_total;
        // Nummern können Lücken haben; die Tabelle ist nach Titelsetnummer sortiert.
        while vts_starts.len() < usize::from(*n) - 1 {
            vts_starts.push(0);
        }
        vts_starts.push(acc);
        acc += vts_size;
        written_total += u64::from(vts_size);
    }

    let mut raw = disc.vmg.raw.clone();
    ifo::patch_vmg(&mut raw, &vmg_menu, &vts_starts, plan.remove_puo, plan.region_free);
    let (ifo_path, bup_path) = write_with_bup(dir, "VIDEO_TS.IFO", &raw)?;
    files.push(ifo_path);
    files.extend_from_slice(&vmg_vob_paths);
    files.push(bup_path);
    files.extend_from_slice(&vts_files);
    written_total += u64::from(ifo::ifo_sectors(&raw)) * 2;
    Ok(written_total)
}

fn run_reauthor(disc: &Disc, plan: &Plan, dir: &Path, ctx: &mut Ctx, files: &mut Vec<PathBuf>) -> Res<u64> {
    if plan.reauthor.is_empty() {
        return Err(tr!("Es wurde noch kein Titel für die neue DVD ausgewählt.", "No title has been chosen for the new DVD yet.").into());
    }
    let groups = plan.title_groups(disc);
    if groups.len() > 99 {
        return Err(tr!("Eine DVD kann höchstens 99 Titel enthalten.", "A DVD can contain at most 99 titles.").into());
    }
    let est = plan.estimate(disc);
    let mut budget = Budget::new(plan, &est, 50.0);
    let count = groups.len();
    let mut vts_files = Vec::new();
    let mut titles = Vec::new();
    let mut vts_attrs = Vec::new();
    let mut sizes = Vec::new();
    let mut total = 0u64;

    for (i, group) in groups.iter().enumerate() {
        ctx.check()?;
        let title_no = i + 1;
        let (avts, paths, entry_titles) = author_entry(disc, plan, group, title_no, count, dir, &mut budget, ctx)?;
        let ifo_bytes = author::build_vts(&avts);
        let name = format!("VTS_{title_no:02}_0.IFO");
        let (ifo_path, bup_path) = write_with_bup(dir, &name, &ifo_bytes)?;
        vts_files.push(ifo_path);
        vts_files.extend_from_slice(&paths);
        vts_files.push(bup_path);
        vts_attrs.push(avts.attrs.clone());
        let ifo = ifo::ifo_sectors(&ifo_bytes);
        let size = ifo * 2 + avts.title_sectors;
        sizes.push(size);
        total += u64::from(size);
        titles.push(entry_titles);
    }

    let mut vmg = AVmg {
        titles,
        vts_attrs,
        first_play: vec![author::cmd_jump_tt(1)],
        pal: disc.is_pal(),
        menu_video: [disc.vmg.raw[0x100], disc.vmg.raw[0x101]],
        ..Default::default()
    };
    let probe = author::build_vmg(&vmg);
    let vmg_sectors = ifo::ifo_sectors(&probe) * 2;
    author::set_vts_starts(&mut vmg, &sizes, vmg_sectors);
    let vmg_bytes = author::build_vmg(&vmg);
    let (ifo_path, bup_path) = write_with_bup(dir, "VIDEO_TS.IFO", &vmg_bytes)?;
    files.push(ifo_path);
    files.push(bup_path);
    files.extend_from_slice(&vts_files);
    total += u64::from(vmg_sectors);
    Ok(total)
}

/// Offene Zelle beim Schreiben eines Titels.
struct OpenCell {
    first: u32,
    last_vobu: u32,
    /// Gemessene Dauer
    measured: u64,
    /// Dauer laut Zellentabelle der DVD
    table_ms: u64,
    flags: [u8; 4],
}

/// Schreibt einen Titel der neuen DVD aus einem oder mehreren Einträgen (`group`:
/// Indizes in `plan.reauthor`). Mehrere Einträge laufen nahtlos hintereinander, und
/// jeder beginnt ein Kapitel.
#[allow(clippy::too_many_arguments)]
fn author_entry(
    disc: &Disc,
    plan: &Plan,
    group: &[usize],
    title_no: usize,
    count: usize,
    dir: &Path,
    budget: &mut Budget,
    ctx: &mut Ctx,
) -> Res<(AVts, Vec<PathBuf>, TitleEntry)> {
    let entries: Vec<&ReauthorEntry> = group.iter().filter_map(|i| plan.reauthor.get(*i)).collect();
    let first_entry = *entries.first().ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
    let t0 = disc.titles.get(first_entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
    let v0 = disc.vts.get(&t0.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
    let joined = entries.len() > 1;
    let mut writer = SpaceWriter::new(dir, &format!("VTS_{title_no:02}_"), false, plan.split_vob);
    let mut new_pos = 0u32;
    let pal = v0.video.pal;

    // Spuren sind eingeschaltet, wenn irgendein Eintrag des Titels sie behält.
    let decisions: Vec<CellDecision> = entries.iter().map(|e| CellDecision::from_settings(&e.settings)).collect();
    let keep_audio = |ph: usize| decisions.iter().any(|d| d.keep_audio[ph]);
    let keep_sub = |ph: usize| decisions.iter().any(|d| d.keep_sub[ph]);
    let mut pgc = APgc { pal, ..Default::default() };
    if let Some(src) = t0.pgcs.first().and_then(|p| v0.pgcs.get(*p)) {
        pgc.audio_ctl = src.audio_ctl;
        pgc.sub_ctl = src.sub_ctl;
        for i in 0..8 {
            if pgc.audio_ctl[i] & 0x8000 != 0 && !keep_audio(usize::from((pgc.audio_ctl[i] >> 8) & 7)) {
                pgc.audio_ctl[i] = 0;
            }
        }
        for i in 0..32 {
            let c = pgc.sub_ctl[i];
            if c & 0x8000_0000 != 0 {
                let phys = [(c >> 24) & 0x1F, (c >> 16) & 0x1F, (c >> 8) & 0x1F, c & 0x1F];
                if !phys.iter().any(|p| keep_sub(*p as usize)) {
                    pgc.sub_ctl[i] = 0;
                }
            }
        }
        if src.offset + 0xE4 <= v0.raw.len() {
            pgc.palette.copy_from_slice(&v0.raw[src.offset + 0xA4..src.offset + 0xE4]);
        }
    }
    pgc.post = vec![if title_no < count { author::cmd_jump_tt((title_no + 1) as u8) } else { author::CMD_EXIT }];

    let mut cells_out: Vec<ACell> = Vec::new();
    let mut cell_addrs: Vec<CellAddr> = Vec::new();
    let mut admap: Vec<u32> = Vec::new();
    let mut time_points: Vec<(u64, u32)> = Vec::new();
    let mut maps: Vec<SectorMap> = Vec::with_capacity(entries.len());
    let mut elapsed_base = 0u64;
    let mut vob_new = 0u16;
    let mut cell_new = 0u8;
    // Mehrere Einträge in einem Titel: Zeitstempel fortlaufend machen, damit der Player
    // ohne Pause von einem Schnitt zum nächsten spielt.
    let mut retimer = Retimer::default();
    let mut open: Option<OpenCell> = None;

    let close = |open: &mut Option<OpenCell>,
                 cells_out: &mut Vec<ACell>,
                 cell_addrs: &mut Vec<CellAddr>,
                 elapsed_base: &mut u64,
                 new_pos: u32,
                 vob: u16,
                 cell: u8,
                 retime: bool| {
        if let Some(c) = open.take() {
            if new_pos == c.first {
                return;
            }
            let ms = if retime { c.measured } else { c.table_ms };
            cells_out.push(ACell { ms, flags: c.flags, first: c.first, last_vobu: c.last_vobu, last: new_pos - 1, vob, cell });
            cell_addrs.push(CellAddr { vob_id: vob, cell_id: cell, start: c.first, last: new_pos - 1 });
            *elapsed_base += ms;
        }
    };

    for (k, (entry, decision)) in entries.iter().zip(&decisions).enumerate() {
        let entry_no = group[k] + 1;
        let label = trf!("Titel {} von {}", "Title {} of {}", entry_no, plan.reauthor.len());
        let _ = ctx.tx.send(BackupMsg::Stage(label.clone()));
        let t = disc.titles.get(entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
        let v = disc.vts.get(&t.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
        let mut rq = Requant::new();
        let mut map = SectorMap::default();
        let mut last_src_vob = None;
        let mut entry_start = true;
        let trimmed = entry.cut.is_trim(crate::engine::plan::entry_duration_ms(disc, entry));
        // Eigene Kapitelmarken (Zeitachse der Ausgabe). An jeder Marke beginnt eine neue
        // Zelle und ein neues Programm, auf eine VOBU (etwa eine halbe Sekunde) genau.
        let marks = entry.output_marks(disc);
        let mut next_mark = 0usize;
        // Zeiten in der Zelle neu zählen, sobald Zellen nicht mehr denen der DVD entsprechen.
        let retime = trimmed || marks.is_some() || joined;
        // Beginn dieses Eintrags auf der Zeitachse des Titels
        let entry_base = elapsed_base;

        entry_stream(disc, entry, plan.declick, |ev| {
            ctx.check()?;
            let item = match ev {
                StreamEvent::Skipped(n) => {
                    ctx.done_in += n;
                    ctx.report(1.0, &label);
                    return Ok(());
                }
                StreamEvent::Vobu(item) => item,
            };
            let mut mark_hit = false;
            if let Some(m) = &marks {
                while next_mark < m.len() && m[next_mark] <= item.out_t0_ms {
                    next_mark += 1;
                    mark_hit = true;
                }
                // Eine Marke in der ersten VOBU fällt mit dem Anfang des Eintrags zusammen.
                if entry_start {
                    mark_hit = false;
                }
            }
            if item.cell_start || mark_hit || entry_start {
                close(&mut open, &mut cells_out, &mut cell_addrs, &mut elapsed_base, new_pos, vob_new, cell_new, retime);
                if cells_out.len() >= 255 {
                    return Err(tr!("Der Titel hat zu viele Zellen für eine einzelne Programmkette.", "The title has too many cells for a single program chain.").into());
                }
                if last_src_vob != Some(item.pc.vob_id) || cell_new == 255 {
                    vob_new += 1;
                    cell_new = 0;
                    last_src_vob = Some(item.pc.vob_id);
                }
                cell_new += 1;
                // Jeder Eintrag beginnt ein Kapitel, danach eigene Marken oder die der DVD.
                let new_program = entry_start
                    || match &marks {
                        Some(_) => mark_hit,
                        None => t.chapters.iter().any(|c| c.first_cell_idx == item.title_cell),
                    };
                if new_program || pgc.programs.is_empty() {
                    if pgc.programs.len() >= 99 {
                        return Err(tr!("Ein Titel kann höchstens 99 Kapitel haben.", "A title can have at most 99 chapters.").into());
                    }
                    pgc.programs.push((cells_out.len() + 1) as u8);
                }
                rq.new_cell();
                let offset = v.pgcs.get(item.pc.pgc).map(|p| p.offset).unwrap_or(0);
                let mut flags = cell_flags(&v.raw, offset, item.pc.cell_idx);
                if joined {
                    // Die Zeitstempel laufen durch; keine Unterbrechung der Systemuhr.
                    flags[0] &= !0x02;
                }
                open = Some(OpenCell { first: new_pos, last_vobu: new_pos, measured: 0, table_ms: item.pc.ms, flags });
                entry_start = false;
            }
            let mut data = item.data;
            if retime {
                // Nach dem Schnitt zählt die Zeit in der Zelle neu.
                let in_cell = (entry_base + item.out_t0_ms).saturating_sub(elapsed_base);
                data[DSI_OFF + 28..DSI_OFF + 32].copy_from_slice(&encode_time(in_cell, pal));
            }
            let opts = decision.video_opts(budget.ratio());
            let mut out = transcode_vobu(&data, decision, opts, &mut rq);
            let in_sectors = item.in_sectors as u32;
            let out_sectors = (out.data.len() / SECTOR) as u32;
            Nav::set_ids(&mut out.data, vob_new, cell_new);
            if joined {
                retimer.apply(&mut out.data);
            }
            writer.write(&out.data)?;
            let start = item.start as u32;
            map.vobus.push((start, start + in_sectors - 1, new_pos, new_pos + out_sectors - 1));
            admap.push(new_pos);
            let at = if retime { entry_base + item.out_t0_ms } else { elapsed_base + Nav::cell_elapsed_ms(&data) };
            time_points.push((at, new_pos));
            if let Some(o) = open.as_mut() {
                o.last_vobu = new_pos;
                o.measured += item.dur_ms;
            }
            new_pos += out_sectors;
            budget.account(decision, out.video_in, out.kept_other, u64::from(out_sectors));
            ctx.written += u64::from(out_sectors);
            ctx.done_in += u64::from(in_sectors);
            ctx.report(f64::from(opts.ratio), &label);
            Ok(())
        })?;
        close(&mut open, &mut cells_out, &mut cell_addrs, &mut elapsed_base, new_pos, vob_new, cell_new, retime);
        maps.push(map);
    }
    writer.finish()?;
    if new_pos == 0 {
        return Err(trf!("Titel {} enthält keine lesbaren Videodaten.", "Title {} contains no readable video data.", t0.number));
    }

    // Verweise auf VOBUs auflösen. Sie gelten je Eintrag, die alten Sektornummern sind nur
    // dort eindeutig (die Tabelle muss nach altem Start sortiert sein).
    for mut map in maps {
        map.new_total = new_pos;
        let mut lookup = map.clone();
        lookup.vobus.sort_by_key(|v| v.0);
        writer.patch_all(&map, |s, i| {
            patch_nav(s, map.vobus[i], &lookup, plan.remove_puo, true);
        })?;
    }

    pgc.cells = cells_out;
    let chapters = pgc.programs.len() as u16;

    // Zeitkarte: je Zeiteinheit die zuletzt begonnene VOBU
    let dur_s = elapsed_base / 1000;
    let unit = ((dur_s / 2040) + 1).clamp(1, 255) as u8;
    let mut entries = Vec::new();
    let mut k = 1u64;
    let mut idx = 0usize;
    while k * u64::from(unit) * 1000 < elapsed_base {
        let at = k * u64::from(unit) * 1000;
        while idx + 1 < time_points.len() && time_points[idx + 1].0 <= at {
            idx += 1;
        }
        entries.push(time_points[idx].1);
        k += 1;
    }

    let attrs = v0.raw[author::ATTR_START..author::ATTR_END].to_vec();
    let avts = AVts {
        attrs,
        pgcs: vec![pgc],
        menu: None,
        cells: cell_addrs,
        admap,
        tmaps: vec![TimeMap { unit_secs: unit, entries }],
        title_sectors: new_pos,
    };
    let title = TitleEntry {
        playback_type: 0x3C,
        angles: 1,
        chapters,
        vts: title_no as u8,
        vts_ttn: 1,
        vts_sector: 0,
    };
    Ok((avts, writer.paths().to_vec(), title))
}

/// Übernimmt Modus und Standzeit einer Zelle, ohne Verschachtelung und Zellbefehl.
fn cell_flags(raw: &[u8], pgc_off: usize, cell_idx: usize) -> [u8; 4] {
    let table = crate::dvd::be16(raw, pgc_off + 0xE8) as usize;
    let at = pgc_off + table + cell_idx * 24;
    let mut f = [0u8; 4];
    if at + 4 <= raw.len() {
        f.copy_from_slice(&raw[at..at + 4]);
    }
    f[0] &= 0x0A; // nur nahtlos und Zeitsprung bleiben
    f[3] = 0;
    f
}

/// Schreibt die gewählten Titel in Abspielreihenfolge in eine einzige VOB-Datei.
/// mpeg: Some(gewünschte Tonspur) für die Ausgabe als schlichter MPEG-2-Strom.
/// `chapters`: bekommt die Kapitelanfänge (90-kHz-Zeitstempel der Ausgabe), für MKV.
fn run_vob(disc: &Disc, plan: &Plan, path: &Path, ctx: &mut Ctx, mpeg: Option<Option<u8>>, chapters: &mut Vec<i64>) -> Res<u64> {
    if plan.reauthor.is_empty() {
        return Err(tr!("Es wurde kein Titel für die Datei ausgewählt.", "No title has been chosen for the file.").into());
    }
    let dir = path.parent().filter(|d| !d.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let name = path.file_name().ok_or(tr!("Der Dateiname fehlt.", "The file name is missing."))?.to_string_lossy().to_string();

    let est = plan.estimate(disc);
    let mut budget = Budget::new(plan, &est, 50.0);
    let mut writer = SpaceWriter::new(dir, &name, true, false);
    let mut retimer = Retimer::default();
    let mut maps: Vec<SectorMap> = Vec::with_capacity(plan.reauthor.len());
    let mut new_pos = 0u32;
    let mut vob_new = 0u16;
    let count = plan.reauthor.len();
    // MPEG-2: Alle Titel schreiben ihren Ton unter derselben Spurnummer, damit genau eine Spur entsteht.
    let mut target_phys: Option<u8> = None;

    for (i, entry) in plan.reauthor.iter().enumerate() {
        ctx.check()?;
        let t = disc.titles.get(entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
        let v = disc.vts.get(&t.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
        let label = trf!("Titel {} von {count}", "Title {} of {count}", i + 1);
        let _ = ctx.tx.send(BackupMsg::Stage(label.clone()));
        let mut decision = CellDecision::from_settings(&entry.settings);
        if let Some(want) = mpeg {
            // Genau eine Tonspur: die gewünschte, sonst die erste gewählte, sonst die erste
            let has = |ph: u8| t.audio.iter().any(|a| a.phys == ph);
            let chosen = want
                .filter(|ph| has(*ph))
                .or_else(|| t.audio.iter().map(|a| a.phys).find(|ph| entry.settings.audio[*ph as usize]))
                .or_else(|| t.audio.first().map(|a| a.phys));
            decision.keep_audio = [false; 8];
            if let Some(ph) = chosen {
                decision.keep_audio[ph as usize] = true;
                target_phys.get_or_insert(ph);
            }
            decision.keep_sub = [false; 32];
        }
        let mut rq = Requant::new();
        let mut map = SectorMap::default();
        let mut cell_new = 0u8;
        let mut last_src_vob = None;
        let _ = v;
        // Kapitel wie auf der DVD: Anfang jedes Eintrags (mit "An Schnitten"), sonst nur der
        // erste, dazu eigene Marken oder die Kapitel der DVD im Eintrag.
        let marks = entry.output_marks(disc);
        let mut next_mark = 0usize;
        let mut entry_start = true;

        entry_stream(disc, entry, plan.declick, |ev| {
            ctx.check()?;
            let item = match ev {
                StreamEvent::Skipped(n) => {
                    ctx.done_in += n;
                    ctx.report(1.0, &label);
                    return Ok(());
                }
                StreamEvent::Vobu(item) => item,
            };
            let mut chapter = entry_start && (i == 0 || plan.chapters_at_cuts);
            if !entry_start {
                match &marks {
                    Some(m) => {
                        while next_mark < m.len() && m[next_mark] <= item.out_t0_ms {
                            next_mark += 1;
                            chapter = true;
                        }
                    }
                    None => chapter = item.cell_start && t.chapters.iter().any(|c| c.first_cell_idx == item.title_cell),
                }
            } else if let Some(m) = &marks {
                while next_mark < m.len() && m[next_mark] <= item.out_t0_ms {
                    next_mark += 1;
                }
            }
            entry_start = false;
            if item.cell_start {
                if last_src_vob != Some(item.pc.vob_id) || cell_new == 255 {
                    vob_new = vob_new.wrapping_add(1);
                    cell_new = 0;
                    last_src_vob = Some(item.pc.vob_id);
                }
                cell_new += 1;
                rq.new_cell();
            }
            let opts = decision.video_opts(budget.ratio());
            let mut out = transcode_vobu(&item.data, &decision, opts, &mut rq);
            let in_sectors = item.in_sectors as u32;
            Nav::set_ids(&mut out.data, vob_new, cell_new);
            retimer.apply(&mut out.data);
            if chapter && out.data.len() >= SECTOR {
                // Anfang der VOBU laut Navigationspaket, schon auf der Zeitachse der Datei
                chapters.push(i64::from(crate::dvd::be32(&out.data, crate::dvd::vob::PCI_OFF + 12)));
            }
            if mpeg.is_some() {
                // Nur Bild und Ton bleiben übrig.
                let mut kept = Vec::with_capacity(out.data.len());
                for s in out.data.chunks_exact(SECTOR) {
                    match crate::dvd::vob::classify(s) {
                        crate::dvd::vob::Stream::Video => kept.extend_from_slice(s),
                        crate::dvd::vob::Stream::Audio(_) => {
                            let at = kept.len();
                            kept.extend_from_slice(s);
                            if let Some(ph) = target_phys {
                                renumber_audio(&mut kept[at..at + SECTOR], ph);
                            }
                        }
                        _ => {}
                    }
                }
                out.data = kept;
                if out.data.is_empty() {
                    ctx.done_in += item.in_sectors;
                    return Ok(());
                }
            }
            let out_sectors = (out.data.len() / SECTOR) as u32;
            writer.write(&out.data)?;
            let start = item.start as u32;
            map.vobus.push((start, start + in_sectors - 1, new_pos, new_pos + out_sectors - 1));
            new_pos += out_sectors;
            budget.account(&decision, out.video_in, out.kept_other, u64::from(out_sectors));
            ctx.written += u64::from(out_sectors);
            ctx.done_in += u64::from(in_sectors);
            ctx.report(f64::from(opts.ratio), &label);
            Ok(())
        })?;        maps.push(map);
    }
    if let Some(end) = retimer.end_pack() {
        writer.write(&end)?;
    }
    writer.finish()?;
    if new_pos == 0 {
        return Err(tr!("Die gewählten Titel enthalten keine lesbaren Videodaten.", "The chosen titles contain no readable video data.").into());
    }

    // Verweise gelten je Titel; die alten Sektornummern sind nur dort eindeutig.
    let _ = ctx.tx.send(BackupMsg::Stage(tr!("Datei abschließen", "Finishing file").into()));
    for map in maps.iter().filter(|_| mpeg.is_none()) {
        let mut lookup = map.clone();
        lookup.vobus.sort_by_key(|v| v.0);
        writer.patch_all(map, |s, i| {
            patch_nav(s, map.vobus[i], &lookup, plan.remove_puo, true);
        })?;
    }
    Ok(u64::from(new_pos))
}

/// Setzt die Spurnummer eines Tonpakets (DVD-Unterstrom bzw. MPEG-Ton-Strom).
fn renumber_audio(s: &mut [u8], phys: u8) {
    let h = crate::dvd::vob::pack_header_len(s);
    if h + 9 > s.len() {
        return;
    }
    match s[h + 3] {
        0xC0..=0xC7 => s[h + 3] = 0xC0 | (phys & 7),
        0xBD => {
            let at = h + 9 + usize::from(s[h + 8]);
            if let Some(b) = s.get_mut(at) {
                *b = (*b & 0xF8) | (phys & 7);
            }
        }
        _ => {}
    }
}

/// Entfernt frühere DVD-Dateien im Zielordner, damit sich nichts vermischt.
fn clear_dvd_files(dir: &Path) {
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    for ent in rd.flatten() {
        let name = ent.file_name().to_string_lossy().to_ascii_uppercase();
        let is_dvd = (name.starts_with("VTS_") || name.starts_with("VIDEO_TS"))
            && (name.ends_with(".IFO") || name.ends_with(".BUP") || name.ends_with(".VOB"));
        if is_dvd {
            let _ = std::fs::remove_file(ent.path());
        }
    }
}

/// Verhindert, dass das Backup seine eigene Quelle überschreibt.
fn guard_source(disc: &Disc, output: &Output, video_dir: &Path) -> Res<()> {
    let same = |a: &Path, b: &Path| match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
        (Ok(x), Ok(y)) => x == y,
        _ => false,
    };
    match output {
        Output::Folder(_) => {
            if let Some(FileRef::Disk(ifo)) = disc.source.files.get("VIDEO_TS.IFO") {
                if ifo.parent().is_some_and(|src| same(src, video_dir)) {
                    return Err(tr!("Das Ziel ist derselbe Ordner wie die Quelle. Bitte wähle einen anderen Zielordner.", "The target is the same folder as the source. Please choose a different target folder.").into());
                }
            }
        }
        Output::Iso(p) | Output::Burn(p) | Output::Vob(p) | Output::Mpeg { path: p, .. } | Output::Mkv { path: p, .. } => {
            let is_source_file = disc
                .source
                .files
                .values()
                .any(|r| matches!(r, FileRef::Disk(d) if same(p, d)));
            if is_source_file || same(p, &disc.source.origin) || disc.source.origins.iter().any(|o| same(p, o)) {
                return Err(tr!("Das Ziel ist dieselbe Datei wie die Quelle. Bitte wähle einen anderen Namen.", "The target is the same file as the source. Please choose a different name.").into());
            }
        }
    }
    Ok(())
}

/// Startet den eingebauten Windows-Brenner mit einem Abbild.
fn start_windows_burner(iso_path: &Path) -> bool {
    std::process::Command::new("isoburn.exe").arg(iso_path).spawn().is_ok()
}

/// Tiefenanalyse vor dem Export (Fortschritt 0 bis 10 %).
fn deep_analyse(disc: &Disc, tx: &Sender<BackupMsg>, cancel: &AtomicBool) -> Res<Disc> {
    let _ = tx.send(BackupMsg::Stage(tr!("Tiefenanalyse", "Deep analysis").into()));
    let mut d = disc.clone();
    let mut cb = |f: f32, _t: &str| -> bool {
        let _ = tx.send(BackupMsg::Progress {
            fraction: f * 0.1,
            sectors_out: 0,
            ratio_in: 1.0,
            current: tr!("Tiefenanalyse", "Deep analysis").into(),
        });
        !cancel.load(Ordering::Relaxed)
    };
    d.analyse(true, &mut cb).map_err(|e| if e == tr!("Abgebrochen", "Cancelled") { CANCELLED.to_string() } else { e })?;
    Ok(d)
}

fn run_inner(req: &BackupRequest, tx: &Sender<BackupMsg>, cancel: &AtomicBool) -> Res<BackupSummary> {
    let started = Instant::now();

    let analysed;
    let disc: &Disc = if req.deep_analysis && !req.disc.analysed_full {
        analysed = deep_analyse(&req.disc, tx, cancel)?;
        &analysed
    } else {
        &req.disc
    };

    if disc.scrambled || req.disc.check_scrambled() {
        return Err(
            tr!("Diese DVD ist kopiergeschützt (CSS). Das Programm verarbeitet nur ungeschützte Inhalte.", "This DVD is copy-protected (CSS). The program only processes unprotected content.").to_string(),
        );
    }

    let total_in = match req.plan.mode {
        Mode::FullDisc => disc.total_sectors,
        Mode::Reauthor => req
            .plan
            .reauthor
            .iter()
            .map(|e| crate::engine::plan::reauthor_cells(disc, e).iter().map(|k| disc.stats_for(k).total as u64).sum::<u64>())
            .sum::<u64>()
            .max(1),
    };

    let single = match &req.output {
        Output::Vob(p) => Some((p, None, false)),
        Output::Mpeg { path, audio } => Some((path, Some(*audio), false)),
        Output::Mkv { path, audio } => Some((path, Some(*audio), true)),
        _ => None,
    };
    if let Some((path, mpeg, mkv)) = single {
        guard_source(disc, &req.output, path)?;
        // MKV: erst ein MPEG-2-Strom daneben, der dann umgepackt wird
        let stream_path = if mkv { path.with_extension("mkv-arbeitsdatei.mpg") } else { path.clone() };
        let mut ctx = Ctx {
            tx,
            cancel,
            done_in: 0,
            total_in,
            last_report: Instant::now(),
            written: 0,
            span: if mkv { 0.9 } else { 0.98 },
        };
        let mut chapters = Vec::new();
        let mut sectors = match run_vob(disc, &req.plan, &stream_path, &mut ctx, mpeg, &mut chapters) {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(&stream_path);
                return Err(e);
            }
        };
        let post = post_process(&req.plan, &[vec![stream_path.clone()]], true, sheet_path_for(&req.output), tx, cancel);
        if mkv {
            let stage = tr!("MKV-Datei schreiben", "Writing MKV file");
            let _ = tx.send(BackupMsg::Stage(stage.into()));
            let mut last = Instant::now();
            let mut progress = |f: f32| {
                if last.elapsed().as_millis() > 200 {
                    last = Instant::now();
                    let _ = tx.send(BackupMsg::Progress { fraction: 0.9 + 0.1 * f, sectors_out: sectors, ratio_in: 1.0, current: stage.into() });
                }
                !cancel.load(Ordering::Relaxed)
            };
            let res = crate::engine::mkv::remux(&stream_path, path, &chapters, &mut progress);
            let _ = std::fs::remove_file(&stream_path);
            match res {
                Ok(bytes) => sectors = bytes.div_ceil(SECTOR as u64),
                Err(e) => {
                    let _ = std::fs::remove_file(path);
                    return Err(e);
                }
            }
        }
        return Ok(BackupSummary {
            path: path.clone(),
            sectors,
            target_sectors: req.plan.target_sectors,
            source_sectors: source_size(disc, &req.plan),
            seconds: started.elapsed().as_secs_f64(),
            fits: sectors <= req.plan.target_sectors,
            burn_started: false,
            check: post.check,
            sheet: post.sheet,
            extra_error: post.error,
            files: 1,
        });
    }

    let (out_path, video_dir, temp_dir) = match &req.output {
        Output::Folder(p) => {
            let vd = p.join("VIDEO_TS");
            (p.clone(), vd, None)
        }
        Output::Vob(_) | Output::Mpeg { .. } | Output::Mkv { .. } => return Err(tr!("Interner Fehler bei der Datei-Ausgabe", "Internal error in file output").into()),
        Output::Iso(p) | Output::Burn(p) => {
            let temp = PathBuf::from(format!("{}.arbeitsordner", p.display()));
            (p.clone(), temp.join("VIDEO_TS"), Some(temp))
        }
    };
    guard_source(disc, &req.output, &video_dir)?;
    std::fs::create_dir_all(&video_dir).map_err(|e| format!("{}: {e}", video_dir.display()))?;
    clear_dvd_files(&video_dir);
    if let Output::Folder(p) = &req.output {
        let _ = std::fs::create_dir_all(p.join("AUDIO_TS"));
    }

    let is_iso = matches!(req.output, Output::Iso(_) | Output::Burn(_));
    let mut ctx = Ctx {
        tx,
        cancel,
        done_in: 0,
        total_in,
        last_report: Instant::now(),
        written: 0,
        span: if is_iso { 0.85 } else { 0.98 },
    };

    let mut files = Vec::new();
    let result = match req.plan.mode {
        Mode::FullDisc => run_full_disc(disc, &req.plan, &video_dir, &mut ctx, &mut files),
        Mode::Reauthor => run_reauthor(disc, &req.plan, &video_dir, &mut ctx, &mut files),
    };

    let finish = |temp: &Option<PathBuf>| {
        if let Some(t) = temp {
            clear_dvd_files(&t.join("VIDEO_TS"));
            let _ = std::fs::remove_dir(t.join("VIDEO_TS"));
            let _ = std::fs::remove_dir(t);
        }
    };

    let sectors = match result {
        Ok(s) => s,
        Err(e) => {
            finish(&temp_dir);
            if temp_dir.is_none() {
                clear_dvd_files(&video_dir);
                let _ = std::fs::remove_dir(&video_dir);
                if let Output::Folder(p) = &req.output {
                    let _ = std::fs::remove_dir(p.join("AUDIO_TS"));
                    let _ = std::fs::remove_dir(p);
                }
            }
            return Err(e);
        }
    };

    // Sicherheitsprüfung: Das Ergebnis muss sich wieder als DVD öffnen lassen.
    let _ = tx.send(BackupMsg::Stage(tr!("Ergebnis prüfen", "Checking result").into()));
    let check = Source::open(&video_dir).and_then(Disc::open);
    if let Err(e) = check {
        finish(&temp_dir);
        return Err(trf!("Das Ergebnis lässt sich nicht lesen: {e}", "The result cannot be read: {e}"));
    }
    // Prüfung und Übersichtsbild, solange die Dateien noch einzeln vorliegen (vor dem ISO)
    let post = post_process(&req.plan, &title_vobs(&video_dir), false, sheet_path_for(&req.output), tx, cancel);

    let mut burn_started = false;
    if is_iso {
        let _ = tx.send(BackupMsg::Stage(tr!("ISO-Abbild schreiben", "Writing ISO image").into()));
        let tx2 = tx.clone();
        let res = iso::write_iso(&out_path, &disc.source.label, &files, |done, total| {
            if cancel.load(Ordering::Relaxed) {
                return false;
            }
            let _ = tx2.send(BackupMsg::Progress {
                fraction: 0.85 + 0.14 * (done as f32 / total.max(1) as f32),
                sectors_out: done / SECTOR as u64,
                ratio_in: 1.0,
                current: tr!("ISO-Abbild", "ISO image").into(),
            });
            true
        });
        finish(&temp_dir);
        if let Err(e) = res {
            let _ = std::fs::remove_file(&out_path);
            return Err(if e == tr!("Abgebrochen", "Cancelled") { CANCELLED.into() } else { e });
        }
        if matches!(req.output, Output::Burn(_)) {
            burn_started = start_windows_burner(&out_path);
        }
    }

    Ok(BackupSummary {
        path: out_path,
        sectors,
        target_sectors: req.plan.target_sectors,
        source_sectors: source_size(disc, &req.plan),
        seconds: started.elapsed().as_secs_f64(),
        fits: sectors <= req.plan.target_sectors,
        burn_started,
        check: post.check,
        sheet: post.sheet,
        extra_error: post.error,
        files: 1,
    })
}

/// Mehrere Ausgaben nacheinander (Export als Einzeldateien). Fortschritt und Stufen werden
/// auf das Ganze umgerechnet, die Tiefenanalyse läuft nur einmal. Bei Abbruch oder Fehler
/// bleiben die schon fertigen Dateien erhalten.
fn run_many_inner(mut reqs: Vec<BackupRequest>, tx: &Sender<BackupMsg>, cancel: &AtomicBool) -> Res<BackupSummary> {
    let started = Instant::now();
    let n = reqs.len().max(1);
    if let Some(first) = reqs.first() {
        if first.deep_analysis && !first.disc.analysed_full {
            let d = Arc::new(deep_analyse(&first.disc, tx, cancel)?);
            for r in &mut reqs {
                r.disc = d.clone();
                r.deep_analysis = false;
            }
        }
    }
    let mut all: Vec<BackupSummary> = Vec::new();
    for (k, req) in reqs.iter().enumerate() {
        let name = match &req.output {
            Output::Vob(p) | Output::Mpeg { path: p, .. } | Output::Mkv { path: p, .. } | Output::Iso(p) | Output::Burn(p) | Output::Folder(p) => {
                p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
            }
        };
        let prefix = trf!("Datei {} von {n} ({name})", "File {} of {n} ({name})", k + 1);
        let (itx, irx) = std::sync::mpsc::channel::<BackupMsg>();
        let prefix = &prefix;
        let res = std::thread::scope(|s| {
            let fwd = s.spawn(move || {
                for m in irx.iter() {
                    let m = match m {
                        BackupMsg::Stage(t) => BackupMsg::Stage(format!("{prefix}: {t}")),
                        BackupMsg::Progress { fraction, sectors_out, ratio_in, current } => {
                            BackupMsg::Progress { fraction: (k as f32 + fraction) / n as f32, sectors_out, ratio_in, current }
                        }
                        other => other,
                    };
                    let _ = tx.send(m);
                }
            });
            let r = run_inner(req, &itx, cancel);
            drop(itx);
            let _ = fwd.join();
            r
        });
        match res {
            Ok(s) => all.push(s),
            Err(e) if e == CANCELLED => return Err(e),
            Err(e) => return Err(trf!("{name}: {e}", "{name}: {e}")),
        }
    }
    let first_path = all.first().map(|s| s.path.clone()).unwrap_or_default();
    let dir = first_path.parent().map(Path::to_path_buf).unwrap_or(first_path);
    // Auffälligkeiten der Prüfung je Datei als Text; Stellen beziehen sich auf die jeweilige Datei
    let checked: Vec<&CheckReport> = all.iter().filter_map(|s| s.check.as_ref()).collect();
    let mut notes: Vec<String> = Vec::new();
    for s in &all {
        if let Some(c) = s.check.as_ref().filter(|c| !c.is_clean()) {
            let name = s.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
            notes.push(trf!(
                "{name}: {} beschädigte Tonstellen, {} Zeitsprünge",
                "{name}: {} damaged audio spots, {} time jumps",
                c.audio_breaks.len(),
                c.time_jumps.len()
            ));
        }
        if let Some(e) = &s.extra_error {
            notes.push(e.clone());
        }
    }
    let check = if !checked.is_empty() && checked.iter().all(|c| c.is_clean()) {
        Some(CheckReport { audio_breaks: Vec::new(), time_jumps: Vec::new(), seconds: checked.iter().map(|c| c.seconds).sum(), tracks: checked[0].tracks })
    } else {
        None
    };
    Ok(BackupSummary {
        path: dir,
        sectors: all.iter().map(|s| s.sectors).sum(),
        target_sectors: reqs.first().map(|r| r.plan.target_sectors).unwrap_or(0),
        source_sectors: all.iter().map(|s| s.source_sectors).sum(),
        seconds: started.elapsed().as_secs_f64(),
        fits: all.iter().all(|s| s.fits),
        burn_started: false,
        check,
        sheet: None,
        extra_error: (!notes.is_empty()).then(|| notes.join("\n")),
        files: all.len(),
    })
}

/// Einstieg für den Arbeits-Thread bei mehreren Ausgaben (Einzeldateien).
pub fn run_many(reqs: Vec<BackupRequest>, tx: Sender<BackupMsg>, cancel: Arc<AtomicBool>) {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_many_inner(reqs, &tx, &cancel)));
    let msg = match res {
        Ok(Ok(s)) => BackupMsg::Done(s),
        Ok(Err(e)) if e == CANCELLED => BackupMsg::Cancelled,
        Ok(Err(e)) => BackupMsg::Failed(e),
        Err(_) => BackupMsg::Failed(tr!("Beim Umwandeln ist ein interner Fehler aufgetreten.", "An internal error occurred during conversion.").into()),
    };
    let _ = tx.send(msg);
}

/// Einstieg für den Arbeits-Thread.
pub fn run(req: BackupRequest, tx: Sender<BackupMsg>, cancel: Arc<AtomicBool>) {
    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_inner(&req, &tx, &cancel)));
    let msg = match res {
        Ok(Ok(s)) => BackupMsg::Done(s),
        Ok(Err(e)) if e == CANCELLED => BackupMsg::Cancelled,
        Ok(Err(e)) => BackupMsg::Failed(e),
        Err(_) => BackupMsg::Failed(tr!("Beim Umwandeln ist ein interner Fehler aufgetreten.", "An internal error occurred during conversion.").into()),
    };
    let _ = tx.send(msg);
}
