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

const CANCELLED: &str = "__abgebrochen__";

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
    if plan.reauthor.len() > 99 {
        return Err(tr!("Eine DVD kann höchstens 99 Titel enthalten.", "A DVD can contain at most 99 titles.").into());
    }
    let est = plan.estimate(disc);
    let mut budget = Budget::new(plan, &est, 50.0);
    let count = plan.reauthor.len();
    let mut vts_files = Vec::new();
    let mut titles = Vec::new();
    let mut vts_attrs = Vec::new();
    let mut sizes = Vec::new();
    let mut total = 0u64;

    for (i, entry) in plan.reauthor.iter().enumerate() {
        ctx.check()?;
        let title_no = i + 1;
        let label = trf!("Titel {title_no} von {count}", "Title {title_no} of {count}");
        let _ = ctx.tx.send(BackupMsg::Stage(label.clone()));
        let (avts, paths, entry_titles) = author_entry(disc, plan, entry, title_no, count, dir, &mut budget, ctx, &label)?;
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

#[allow(clippy::too_many_arguments)]
fn author_entry(
    disc: &Disc,
    plan: &Plan,
    entry: &ReauthorEntry,
    title_no: usize,
    count: usize,
    dir: &Path,
    budget: &mut Budget,
    ctx: &mut Ctx,
    label: &str,
) -> Res<(AVts, Vec<PathBuf>, TitleEntry)> {
    let t = disc.titles.get(entry.title).ok_or(tr!("Titel nicht gefunden", "Title not found"))?;
    let v = disc.vts.get(&t.vts).ok_or(tr!("Titelset nicht gefunden", "Title set not found"))?;
    let decision = CellDecision::from_settings(&entry.settings);
    let mut writer = SpaceWriter::new(dir, &format!("VTS_{title_no:02}_"), false, plan.split_vob);
    let mut rq = Requant::new();
    let mut map = SectorMap::default();
    let mut new_pos = 0u32;

    let mut pgc = APgc { pal: v.video.pal, ..Default::default() };
    if let Some(src) = t.pgcs.first().and_then(|p| v.pgcs.get(*p)) {
        pgc.audio_ctl = src.audio_ctl;
        pgc.sub_ctl = src.sub_ctl;
        for i in 0..8 {
            if pgc.audio_ctl[i] & 0x8000 != 0 && !decision.keep_audio[usize::from((pgc.audio_ctl[i] >> 8) & 7)] {
                pgc.audio_ctl[i] = 0;
            }
        }
        for i in 0..32 {
            let c = pgc.sub_ctl[i];
            if c & 0x8000_0000 != 0 {
                let phys = [(c >> 24) & 0x1F, (c >> 16) & 0x1F, (c >> 8) & 0x1F, c & 0x1F];
                if !phys.iter().any(|p| decision.keep_sub[*p as usize]) {
                    pgc.sub_ctl[i] = 0;
                }
            }
        }
        if src.offset + 0xE4 <= v.raw.len() {
            pgc.palette.copy_from_slice(&v.raw[src.offset + 0xA4..src.offset + 0xE4]);
        }
    }
    pgc.post = vec![if title_no < count { author::cmd_jump_tt((title_no + 1) as u8) } else { author::CMD_EXIT }];

    let mut cells_out: Vec<ACell> = Vec::new();
    let mut cell_addrs: Vec<CellAddr> = Vec::new();
    let mut admap: Vec<u32> = Vec::new();
    let mut time_points: Vec<(u64, u32)> = Vec::new();
    let mut elapsed_base = 0u64;
    let mut vob_new = 0u16;
    let mut cell_new = 0u8;
    let mut last_src_vob = None;
    let trimmed = entry.cut.is_trim(crate::engine::plan::entry_duration_ms(disc, entry));
    // Eigene Kapitelmarken (Zeitachse der Ausgabe). An jeder Marke beginnt eine neue
    // Zelle und ein neues Programm, auf eine VOBU (etwa eine halbe Sekunde) genau.
    let marks = entry.output_marks(disc);
    let mut next_mark = 0usize;
    // Zeiten in der Zelle neu zählen, sobald Zellen nicht mehr denen der DVD entsprechen.
    let retime = trimmed || marks.is_some();
    let pal = v.video.pal;
    // Offene Zelle: (PGC, Zelle in der PGC, erster Sektor, letzte VOBU, gemessene Dauer, Dauer laut Tabelle)
    let mut open: Option<(usize, usize, u32, u32, u64, u64)> = None;

    let close = |open: &mut Option<(usize, usize, u32, u32, u64, u64)>,
                 cells_out: &mut Vec<ACell>,
                 cell_addrs: &mut Vec<CellAddr>,
                 elapsed_base: &mut u64,
                 new_pos: u32,
                 vob: u16,
                 cell: u8| {
        if let Some((pgc_idx, cell_idx, first, last_vobu, measured, table_ms)) = open.take() {
            if new_pos == first {
                return;
            }
            let ms = if retime { measured } else { table_ms };
            let offset = v.pgcs.get(pgc_idx).map(|p| p.offset).unwrap_or(0);
            cells_out.push(ACell {
                ms,
                flags: cell_flags(&v.raw, offset, cell_idx),
                first,
                last_vobu,
                last: new_pos - 1,
                vob,
                cell,
            });
            cell_addrs.push(CellAddr { vob_id: vob, cell_id: cell, start: first, last: new_pos - 1 });
            *elapsed_base += ms;
        }
    };

    entry_stream(disc, entry, |ev| {
        ctx.check()?;
        let item = match ev {
            StreamEvent::Skipped(n) => {
                ctx.done_in += n;
                ctx.report(1.0, label);
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
            // Eine Marke in der allerersten VOBU fällt mit Kapitel 1 zusammen.
            if open.is_none() && cells_out.is_empty() {
                mark_hit = false;
            }
        }
        if item.cell_start || mark_hit {
            close(&mut open, &mut cells_out, &mut cell_addrs, &mut elapsed_base, new_pos, vob_new, cell_new);
            if cells_out.len() >= 255 {
                return Err(tr!("Der Titel hat zu viele Zellen für eine einzelne Programmkette.", "The title has too many cells for a single program chain.").into());
            }
            if last_src_vob != Some(item.pc.vob_id) || cell_new == 255 {
                vob_new += 1;
                cell_new = 0;
                last_src_vob = Some(item.pc.vob_id);
            }
            cell_new += 1;
            let new_program = match &marks {
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
            open = Some((item.pc.pgc, item.pc.cell_idx, new_pos, new_pos, 0, item.pc.ms));
        }
        let mut data = item.data;
        if retime {
            // Nach dem Schnitt zählt die Zeit in der Zelle neu.
            let in_cell = item.out_t0_ms.saturating_sub(elapsed_base);
            data[DSI_OFF + 28..DSI_OFF + 32].copy_from_slice(&encode_time(in_cell, pal));
        }
        let opts = decision.video_opts(budget.ratio());
        let mut out = transcode_vobu(&data, &decision, opts, &mut rq);
        let in_sectors = item.in_sectors as u32;
        let out_sectors = (out.data.len() / SECTOR) as u32;
        Nav::set_ids(&mut out.data, vob_new, cell_new);
        writer.write(&out.data)?;
        let start = item.start as u32;
        map.vobus.push((start, start + in_sectors - 1, new_pos, new_pos + out_sectors - 1));
        admap.push(new_pos);
        let at = if retime { item.out_t0_ms } else { elapsed_base + Nav::cell_elapsed_ms(&data) };
        time_points.push((at, new_pos));
        if let Some(o) = open.as_mut() {
            o.3 = new_pos;
            o.4 += item.dur_ms;
        }
        new_pos += out_sectors;
        budget.account(&decision, out.video_in, out.kept_other, u64::from(out_sectors));
        ctx.written += u64::from(out_sectors);
        ctx.done_in += u64::from(in_sectors);
        ctx.report(f64::from(opts.ratio), label);
        Ok(())
    })?;
    close(&mut open, &mut cells_out, &mut cell_addrs, &mut elapsed_base, new_pos, vob_new, cell_new);    writer.finish()?;
    if new_pos == 0 {
        return Err(trf!("Titel {} enthält keine lesbaren Videodaten.", "Title {} contains no readable video data.", t.number));
    }
    map.new_total = new_pos;

    // Verweise auf VOBUs auflösen (die Tabelle muss nach altem Start sortiert sein)
    let mut lookup = map.clone();
    lookup.vobus.sort_by_key(|v| v.0);
    writer.patch_all(&map, |s, i| {
        patch_nav(s, map.vobus[i], &lookup, plan.remove_puo, true);
    })?;

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

    let attrs = v.raw[author::ATTR_START..author::ATTR_END].to_vec();
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
fn run_vob(disc: &Disc, plan: &Plan, path: &Path, ctx: &mut Ctx, mpeg: Option<Option<u8>>) -> Res<u64> {
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
        let _ = (t, v);

        entry_stream(disc, entry, |ev| {
            ctx.check()?;
            let item = match ev {
                StreamEvent::Skipped(n) => {
                    ctx.done_in += n;
                    ctx.report(1.0, &label);
                    return Ok(());
                }
                StreamEvent::Vobu(item) => item,
            };
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
        Output::Iso(p) | Output::Burn(p) | Output::Vob(p) | Output::Mpeg { path: p, .. } => {
            let is_source_file = disc
                .source
                .files
                .values()
                .any(|r| matches!(r, FileRef::Disk(d) if same(p, d)));
            if is_source_file || same(p, &disc.source.origin) {
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

fn run_inner(req: &BackupRequest, tx: &Sender<BackupMsg>, cancel: &AtomicBool) -> Res<BackupSummary> {
    let started = Instant::now();

    let analysed;
    let disc: &Disc = if req.deep_analysis && !req.disc.analysed_full {
        let _ = tx.send(BackupMsg::Stage(tr!("Tiefenanalyse", "Deep analysis").into()));
        let mut d = (*req.disc).clone();
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
        analysed = d;
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
        Output::Vob(p) => Some((p, None)),
        Output::Mpeg { path, audio } => Some((path, Some(*audio))),
        _ => None,
    };
    if let Some((path, mpeg)) = single {
        guard_source(disc, &req.output, path)?;
        let mut ctx = Ctx {
            tx,
            cancel,
            done_in: 0,
            total_in,
            last_report: Instant::now(),
            written: 0,
            span: 0.98,
        };
        let sectors = match run_vob(disc, &req.plan, path, &mut ctx, mpeg) {
            Ok(s) => s,
            Err(e) => {
                let _ = std::fs::remove_file(path);
                return Err(e);
            }
        };
        return Ok(BackupSummary {
            path: path.clone(),
            sectors,
            target_sectors: req.plan.target_sectors,
            source_sectors: total_in as f64,
            seconds: started.elapsed().as_secs_f64(),
            fits: sectors <= req.plan.target_sectors,
            burn_started: false,
        });
    }

    let (out_path, video_dir, temp_dir) = match &req.output {
        Output::Folder(p) => {
            let vd = p.join("VIDEO_TS");
            (p.clone(), vd, None)
        }
        Output::Vob(_) | Output::Mpeg { .. } => return Err(tr!("Interner Fehler bei der Datei-Ausgabe", "Internal error in file output").into()),
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
        source_sectors: disc.total_sectors as f64,
        seconds: started.elapsed().as_secs_f64(),
        fits: sectors <= req.plan.target_sectors,
        burn_started,
    })
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
