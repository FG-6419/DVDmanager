//! Die Anwendung: Zustand, Hintergrundarbeiten und der Bildaufbau je Frame.

mod dialogs;
mod preview;
mod style;
mod thumbs;
mod views;

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Color32, Margin};

use discslim::dvd::model::{Category, Disc};
use discslim::dvd::source::Source;
use discslim::engine::backup::{BackupMsg, BackupSummary};
use discslim::engine::plan::{dvd5_sectors, dvd9_sectors, ItemId, Plan};

use preview::PreviewState;
use style::Pal;

/// Was in der Liste gewählt ist.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Selection {
    None,
    Item(ItemId),
    Entry(usize),
    /// Ein Titel der DVD in der Quellliste beim Neu-Zusammenstellen (nur ansehen)
    Source(usize),
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum TargetKind {
    Dvd5,
    Dvd9,
    Custom,
    /// Ohne Größengrenze
    Unlimited,
}

enum LoadMsg {
    Progress(f32, String),
    Done(Box<Disc>),
    Failed(String),
}

struct Loader {
    rx: Receiver<LoadMsg>,
    progress: f32,
    text: String,
    cancel: Arc<AtomicBool>,
    is_sample: bool,
}

#[derive(Clone, Copy, PartialEq)]
pub enum OutputKind {
    Folder,
    Iso,
    Burn,
    Vob,
    /// Schlichter MPEG-2-Strom mit einer Tonspur
    Mpeg,
}

pub struct BackupDialog {
    pub kind: OutputKind,
    pub folder: String,
    pub iso: String,
    pub vob: String,
    pub mpg: String,
    /// Tonspur für die MPEG-2-Datei (physische Nummer)
    pub mpeg_audio: Option<u8>,
    /// VOB-Dateien nicht in Teile von knapp 1 GB zerlegen.
    pub no_split: bool,
    /// Welche Titel in die einzelne VOB-Datei kommen.
    pub vob_sel: Vec<bool>,
    /// Automatik soll auch für die VOB-Datei die Zielgröße einhalten.
    pub vob_keep_target: bool,
    pub deep: bool,
    /// Vorhandene Dateien, die überschrieben würden (Rückfrage läuft)
    pub overwrite: Option<Vec<String>>,
}

pub struct Job {
    pub rx: Receiver<BackupMsg>,
    pub cancel: Arc<AtomicBool>,
    pub started: Instant,
    pub stage: String,
    pub current: String,
    pub fraction: f32,
    pub sectors_out: u64,
    pub ratio: f64,
}

pub enum JobResult {
    Ok(BackupSummary),
    Failed(String),
    Cancelled,
}

#[derive(Clone)]
pub struct Prefs {
    pub dark: bool,
    pub lang: String,
    pub only_lang: bool,
    pub deep_default: bool,
    pub target_kind: TargetKind,
    pub custom_mb: u32,
    /// Zuletzt gewähltes Backup-Ziel und seine Optionen
    pub backup_kind: OutputKind,
    pub folder_dir: String,
    pub file_dir: String,
    pub no_split: bool,
    pub remove_puo: bool,
    pub region_free: bool,
    pub vob_keep_target: bool,
    /// Vorschau mit Kompression, Schnittansicht, bildgenau als Vorgabe für neue Schnitte
    pub compressed: bool,
    pub cut_view: bool,
    pub cut_exact: bool,
    /// Sprungweite der Sekunden-Knöpfe beim Schneiden
    pub jump_secs: u32,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            dark: true,
            lang: "de".into(),
            only_lang: false,
            deep_default: false,
            target_kind: TargetKind::Dvd5,
            custom_mb: 700,
            backup_kind: OutputKind::Iso,
            folder_dir: String::new(),
            file_dir: String::new(),
            no_split: false,
            remove_puo: false,
            region_free: false,
            vob_keep_target: true,
            compressed: false,
            cut_view: false,
            cut_exact: false,
            jump_secs: 10,
        }
    }
}

pub struct App {
    pal: Pal,
    prefs: Prefs,
    frames: u64,
    screenshot: Option<PathBuf>,
    shot_taken: Arc<AtomicBool>,
    shot_requested: bool,

    disc: Option<Arc<Disc>>,
    plan: Plan,
    sel: Selection,
    loader: Option<Loader>,
    error: Option<String>,
    preview: PreviewState,

    backup_dlg: Option<BackupDialog>,
    job: Option<Job>,
    result: Option<JobResult>,
    settings_open: bool,
    about_open: bool,

    drives: Vec<(String, PathBuf, bool)>,
    drives_at: Option<Instant>,
    keep_plan_on_load: bool,
    /// Vorschaubilder der Einträge und die Schleife unter der Maus
    thumbs: Option<thumbs::Thumbs>,
    hover_loop: Option<thumbs::HoverLoop>,
    target_before_sample: Option<(TargetKind, u32)>,
    /// Nur im Entwicklungs-Build: springt für Prüfbilder in einen bestimmten Zustand.
    #[cfg(debug_assertions)]
    dev_state: Option<String>,
}

fn target_sectors(p: &Prefs) -> u64 {
    match p.target_kind {
        TargetKind::Dvd5 => dvd5_sectors(),
        TargetKind::Dvd9 => dvd9_sectors(),
        TargetKind::Custom => u64::from(p.custom_mb) * 1024 * 1024 / 2048,
        TargetKind::Unlimited => discslim::engine::plan::NO_TARGET,
    }
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, screenshot: Option<PathBuf>, open: Option<PathBuf>) -> Self {
        let mut prefs = Prefs::default();
        if let Some(st) = cc.storage {
            if let Some(v) = st.get_string("dark") {
                prefs.dark = v == "1";
            }
            if let Some(v) = st.get_string("lang") {
                prefs.lang = v;
            }
            if let Some(v) = st.get_string("only_lang") {
                prefs.only_lang = v == "1";
            }
            if let Some(v) = st.get_string("deep") {
                prefs.deep_default = v == "1";
            }
            if let Some(v) = st.get_string("custom_mb").and_then(|v| v.parse().ok()) {
                prefs.custom_mb = v;
            }
            let flag = |k: &str| st.get_string(k).map(|v| v == "1");
            if let Some(v) = st.get_string("backup_kind") {
                prefs.backup_kind = match v.as_str() {
                    "folder" => OutputKind::Folder,
                    "vob" => OutputKind::Vob,
                    "mpeg" => OutputKind::Mpeg,
                    "burn" => OutputKind::Burn,
                    _ => OutputKind::Iso,
                };
            }
            if let Some(v) = st.get_string("jump_secs").and_then(|v| v.parse().ok()) {
                prefs.jump_secs = v;
            }
            if let Some(v) = st.get_string("folder_dir") {
                prefs.folder_dir = v;
            }
            if let Some(v) = st.get_string("file_dir") {
                prefs.file_dir = v;
            }
            for (key, slot) in [
                ("no_split", &mut prefs.no_split),
                ("remove_puo", &mut prefs.remove_puo),
                ("region_free", &mut prefs.region_free),
                ("vob_keep_target", &mut prefs.vob_keep_target),
                ("compressed", &mut prefs.compressed),
                ("cut_view", &mut prefs.cut_view),
                ("cut_exact", &mut prefs.cut_exact),
            ] {
                if let Some(v) = flag(key) {
                    *slot = v;
                }
            }
            if let Some(v) = st.get_string("target") {
                prefs.target_kind = match v.as_str() {
                    "9" => TargetKind::Dvd9,
                    "c" => TargetKind::Custom,
                    "u" => TargetKind::Unlimited,
                    _ => TargetKind::Dvd5,
                };
            }
        }
        // Für die Prüfbilder immer das Standard-Design
        if screenshot.is_some() {
            prefs = Prefs::default();
        }
        style::install_fonts(&cc.egui_ctx);
        let pal = if prefs.dark { style::DARK } else { style::LIGHT };
        style::apply(&cc.egui_ctx, &pal);
        let target = target_sectors(&prefs);
        let mut app = Self {
            pal,
            prefs,
            frames: 0,
            screenshot,
            shot_taken: Arc::new(AtomicBool::new(false)),
            shot_requested: false,
            disc: None,
            plan: Plan::new(target),
            sel: Selection::None,
            loader: None,
            error: None,
            preview: PreviewState::default(),
            backup_dlg: None,
            job: None,
            result: None,
            settings_open: false,
            about_open: false,
            drives: Vec::new(),
            drives_at: None,
            keep_plan_on_load: false,
            thumbs: Some(thumbs::Thumbs::new(cc.egui_ctx.clone())),
            hover_loop: None,
            target_before_sample: None,
            #[cfg(debug_assertions)]
            dev_state: std::env::var("DISCSLIM_DEV_STATE").ok(),
        };
        app.preview.audio = Some(discslim::engine::audio::AudioBus::start());
        app.preview.compressed = app.prefs.compressed;
        app.preview.cut_view = app.prefs.cut_view;
        if let Some(p) = open {
            app.open_path(&cc.egui_ctx, p);
        }
        app
    }

    fn set_dark(&mut self, ctx: &egui::Context, dark: bool) {
        self.prefs.dark = dark;
        self.pal = if dark { style::DARK } else { style::LIGHT };
        style::apply(ctx, &self.pal);
    }

    /// Öffnet Ordner, Laufwerk oder ISO im Hintergrund und analysiert den Inhalt.
    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        self.start_loader(ctx, false, move |tx, cancel| {
            let source = Source::open(&path)?;
            load_and_analyse(source, tx, cancel)
        });
    }

    /// Erzeugt die Beispiel-DVD und öffnet sie.
    fn open_sample(&mut self, ctx: &egui::Context) {
        self.start_loader(ctx, true, move |tx, cancel| {
            let _ = tx.send(LoadMsg::Progress(0.0, "Beispiel-DVD wird erzeugt …".into()));
            let tx2 = tx.clone();
            let dir = discslim::demo::build_sample(&discslim::demo::sample_dir(), |f| {
                let _ = tx2.send(LoadMsg::Progress(f * 0.8, "Beispiel-DVD wird erzeugt …".into()));
            })?;
            let source = Source::open(&dir)?;
            load_and_analyse(source, tx, cancel)
        });
    }

    fn start_loader(
        &mut self,
        ctx: &egui::Context,
        is_sample: bool,
        job: impl FnOnce(&std::sync::mpsc::Sender<LoadMsg>, &AtomicBool) -> Result<Disc, String> + Send + 'static,
    ) {
        self.preview.stop();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let c2 = cancel.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(&tx, &c2)));
            let msg = match res {
                Ok(Ok(d)) => LoadMsg::Done(Box::new(d)),
                Ok(Err(e)) => LoadMsg::Failed(e),
                Err(_) => LoadMsg::Failed("Die DVD konnte nicht gelesen werden (interner Fehler).".into()),
            };
            let _ = tx.send(msg);
            ctx2.request_repaint();
        });
        self.loader =
            Some(Loader { rx, progress: 0.0, text: "DVD wird geöffnet …".into(), cancel, is_sample });
    }

    fn pump_loader(&mut self, ctx: &egui::Context) {
        let Some(l) = self.loader.as_mut() else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(60));
        let mut finished = None;
        while let Ok(m) = l.rx.try_recv() {
            match m {
                LoadMsg::Progress(f, t) => {
                    l.progress = f;
                    l.text = t;
                }
                LoadMsg::Done(d) => finished = Some(Ok(d)),
                LoadMsg::Failed(e) => finished = Some(Err(e)),
            }
        }
        let is_sample = l.is_sample;
        match finished {
            Some(Ok(d)) => {
                self.loader = None;
                self.accept_disc(*d, is_sample);
            }
            Some(Err(e)) => {
                self.loader = None;
                self.keep_plan_on_load = false;
                self.preview.dirty = true;
                if e != "Abgebrochen" {
                    self.error = Some(e);
                }
            }
            None => {}
        }
    }

    fn accept_disc(&mut self, disc: Disc, is_sample: bool) {
        if self.keep_plan_on_load {
            // Nach der Tiefenanalyse bleiben alle Einstellungen, nur die Größen werden genauer.
            self.keep_plan_on_load = false;
            self.disc = Some(Arc::new(disc));
            self.preview.dirty = true;
            return;
        }
        // Neue DVD: alte Vorschaubilder gelten nicht mehr.
        if let Some(t) = self.thumbs.as_mut() {
            t.clear();
        }
        self.hover_loop = None;
        // Die kleine Zielgröße der Beispiel-DVD gilt nicht für echte DVDs.
        if let Some((kind, mb)) = self.target_before_sample.take() {
            self.prefs.target_kind = kind;
            self.prefs.custom_mb = mb;
        }
        // Die Zielgröße bleibt auch bei der Beispiel-DVD die gewählte (Bild ist standardmäßig "Original").
        let _ = is_sample;
        let mut plan = Plan::new(target_sectors(&self.prefs));
        plan.remove_puo = self.prefs.remove_puo;
        plan.region_free = self.prefs.region_free;
        if self.prefs.only_lang {
            apply_language(&disc, &mut plan, &self.prefs.lang);
        }
        self.plan = plan;
        self.sel = disc
            .titles
            .iter()
            .position(|t| t.category == Category::MainMovie)
            .map(|i| Selection::Item(ItemId::Title(i)))
            .unwrap_or(Selection::None);
        self.disc = Some(Arc::new(disc));
        self.preview.dirty = true;
    }

    /// Volle Analyse über alle Daten.
    fn deep_analyse(&mut self, ctx: &egui::Context) {
        let Some(disc) = self.disc.clone() else { return };
        self.keep_plan_on_load = true;
        self.start_loader(ctx, false, move |tx, cancel| {
            let mut d = (*disc).clone();
            let tx2 = tx.clone();
            let mut cb = |f: f32, t: &str| -> bool {
                let _ = tx2.send(LoadMsg::Progress(f, t.to_string()));
                !cancel.load(Ordering::Relaxed)
            };
            d.analyse(true, &mut cb)?;
            Ok(d)
        });
    }

    fn pump_job(&mut self, ctx: &egui::Context) {
        let Some(j) = self.job.as_mut() else { return };
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let mut done = None;
        while let Ok(m) = j.rx.try_recv() {
            match m {
                BackupMsg::Stage(s) => j.stage = s,
                BackupMsg::Progress { fraction, sectors_out, ratio_in, current } => {
                    j.fraction = fraction;
                    j.sectors_out = sectors_out;
                    j.ratio = ratio_in;
                    j.current = current;
                }
                BackupMsg::Done(s) => done = Some(JobResult::Ok(s)),
                BackupMsg::Failed(e) => done = Some(JobResult::Failed(e)),
                BackupMsg::Cancelled => done = Some(JobResult::Cancelled),
            }
        }
        if let Some(r) = done {
            self.job = None;
            self.result = Some(r);
        }
    }

    fn drives(&mut self) -> &[(String, PathBuf, bool)] {
        let stale = self.drives_at.map(|t| t.elapsed().as_secs() >= 3).unwrap_or(true);
        if stale {
            self.drives = discslim::dvd::source::optical_drives_with_video();
            self.drives_at = Some(Instant::now());
        }
        &self.drives
    }

    fn pick_folder(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new().set_title("DVD-Ordner wählen (VIDEO_TS)").pick_folder() {
            self.open_path(ctx, p);
        }
    }

    fn pick_iso(&mut self, ctx: &egui::Context) {
        if let Some(p) = rfd::FileDialog::new()
            .set_title("ISO-Abbild wählen")
            .add_filter("DVD-Abbild", &["iso", "img"])
            .add_filter("Alle Dateien", &["*"])
            .pick_file()
        {
            self.open_path(ctx, p);
        }
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped = ctx.input(|i| i.raw.dropped_files.first().map(|f| f.path().to_path_buf()));
        if let Some(p) = dropped {
            if self.job.is_none() && self.loader.is_none() {
                self.open_path(ctx, p);
            }
        }
    }

    fn logic_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        ctx.request_repaint();
        if self.shot_taken.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Mit dem Zählen warten, bis eine Ladeaktion fertig ist.
        if self.loader.is_some() {
            return;
        }
        self.frames += 1;
        #[cfg(debug_assertions)]
        self.apply_dev_state(ctx);
        if self.frames >= 120 && !self.shot_requested {
            self.shot_requested = true;
            let flag = self.shot_taken.clone();
            let ctx2 = ctx.clone();
            ctx.request_screenshot(move |img| {
                save_png(&path, &img);
                flag.store(true, Ordering::SeqCst);
                ctx2.request_repaint();
            });
        }
    }
}

#[cfg(debug_assertions)]
impl App {
    /// Zeigt für Prüfbilder einen bestimmten Zustand, gesteuert von außen.
    fn apply_dev_state(&mut self, ctx: &egui::Context) {
        let Some(state) = self.dev_state.clone() else { return };
        match (state.as_str(), self.frames) {
            ("backup", 5) => self.open_backup_dialog(),
            ("cut" | "cutview", 5) => {
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    let t = &d.titles[0];
                    let mut e = discslim::engine::plan::ReauthorEntry::new(0, 0, t.chapters.len().saturating_sub(1), Default::default());
                    e.cut.start_ms = Some(8_000);
                    e.cut.end_ms = Some(41_500);
                    e.cut.exact = true;
                    self.plan.reauthor.push(e);
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("cutview", 60) => self.preview.cut_view = true,
            ("split", 5) => {
                // Titel 1 nach Kapiteln zerlegen und ohne Größengrenze anzeigen
                if let Some(d) = self.disc.clone() {
                    let last = d.titles[0].chapters.len().saturating_sub(1);
                    let s = self.plan.settings(ItemId::Title(0));
                    self.split_chapters(&d, 0, 0, last, s, 0);
                }
                self.prefs.target_kind = TargetKind::Unlimited;
                self.plan.target_sectors = target_sectors(&self.prefs);
            }
            ("allshort", 5) => {
                // Sehr kurze Schnitte, um das Weiterschalten zu prüfen
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    for (i, t) in d.titles.iter().enumerate() {
                        let mut e = discslim::engine::plan::ReauthorEntry::new(i, 0, t.chapters.len().saturating_sub(1), Default::default());
                        e.cut = discslim::engine::plan::Cut { start_ms: Some(1_000), end_ms: Some(1_600), exact: true };
                        self.plan.reauthor.push(e);
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("allend", 5) => {
                // Kurze Schnitte, die genau am Titelende aufhören
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    for (i, t) in d.titles.iter().enumerate() {
                        let mut e = discslim::engine::plan::ReauthorEntry::new(i, 0, t.chapters.len().saturating_sub(1), Default::default());
                        let total = discslim::engine::plan::entry_duration_ms(&d, &e);
                        e.cut = discslim::engine::plan::Cut { start_ms: Some(total.saturating_sub(600)), end_ms: Some(total), exact: true };
                        self.plan.reauthor.push(e);
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("revprog", 5) => {
                // Lange Schnitte, Rückwärtsfahrt vom Anfang des zweiten Eintrags in den ersten hinein
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    for (i, t) in d.titles.iter().enumerate().take(2) {
                        let mut e = discslim::engine::plan::ReauthorEntry::new(i, 0, t.chapters.len().saturating_sub(1), Default::default());
                        e.cut = discslim::engine::plan::Cut { start_ms: Some(2_000), end_ms: Some(6_000), exact: true };
                        self.plan.reauthor.push(e);
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("revprog", 30) => {
                self.update_program();
                self.preview.program = true;
                self.preview.prog_cur = 1;
                self.preview.prog_seek = Some(2_000.0);
                self.preview.prog_play = Some(false);
            }
            ("revprog", 60) => self.preview.dev_hold_reverse(),
            ("allshort" | "allend", 30) => {
                self.update_program();
                self.preview.program = true;
                self.preview.prog_cur = 0;
                self.preview.prog_seek = self.preview.prog.first().map(|c| c.1);
                self.preview.prog_play = Some(true);
            }
            ("allcuts", 5) => {
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    for (i, t) in d.titles.iter().enumerate() {
                        let mut e = discslim::engine::plan::ReauthorEntry::new(i, 0, t.chapters.len().saturating_sub(1), Default::default());
                        let total = discslim::engine::plan::entry_duration_ms(&d, &e);
                        e.cut = discslim::engine::plan::Cut { start_ms: Some(2_000), end_ms: Some(total.saturating_sub(3_000)), exact: false };
                        self.plan.reauthor.push(e);
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("allcuts", 40) => {
                self.update_program();
                self.preview.program = true;
                self.preview.prog_cur = 0;
                self.preview.prog_seek = self.preview.prog.first().map(|c| c.1);
                self.preview.prog_play = Some(true);
            }
            ("overwrite", 5) => {
                self.open_backup_dialog();
                if let Some(d) = self.backup_dlg.as_mut() {
                    d.kind = OutputKind::Folder;
                    d.overwrite = Some(vec!["VIDEO_TS\\VIDEO_TS.IFO".into(), "VIDEO_TS\\VTS_01_0.IFO".into(), "VIDEO_TS\\VTS_01_1.VOB".into()]);
                }
            }
            ("source", 5) => {
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                self.sel = Selection::Source(1);
                self.preview.dirty = true;
            }
            ("vob", 5) => {
                self.open_backup_dialog();
                if let Some(d) = self.backup_dlg.as_mut() {
                    d.kind = OutputKind::Vob;
                }
            }
            ("nosplit", 5) => {
                self.open_backup_dialog();
                if let Some(d) = self.backup_dlg.as_mut() {
                    d.kind = OutputKind::Folder;
                    d.no_split = true;
                }
            }
            ("settings", 5) => self.settings_open = true,
            ("light", 5) => self.set_dark(ctx, false),
            ("play", 5) => self.preview.play(),
            ("sample", 1) => {
                self.frames = 2;
                self.dev_state = None;
                self.open_sample(ctx);
            }
            ("job", 5) => {
                self.open_backup_dialog();
                if let (Some(d), Ok(p)) = (self.backup_dlg.as_mut(), std::env::var("DISCSLIM_DEV_TARGET")) {
                    d.iso = p;
                }
                self.start_job(ctx);
            }
            ("reauthor", 5) => {
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    for (i, t) in d.titles.iter().enumerate() {
                        self.plan.reauthor.push(discslim::engine::plan::ReauthorEntry::new(
                            i,
                            0,
                            t.chapters.len().saturating_sub(1),
                            Default::default(),
                        ));
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            _ => {}
        }
    }
}

fn save_png(path: &Path, img: &egui::ColorImage) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let mut rgba = Vec::with_capacity(img.pixels.len() * 4);
    for px in &img.pixels {
        rgba.extend_from_slice(&px.to_srgba_unmultiplied());
    }
    let [w, h] = img.size;
    if let Err(e) = image::save_buffer(path, &rgba, w as u32, h as u32, image::ExtendedColorType::Rgba8) {
        eprintln!("Bild konnte nicht gespeichert werden: {e}");
    }
}

fn load_and_analyse(
    source: Source,
    tx: &std::sync::mpsc::Sender<LoadMsg>,
    cancel: &AtomicBool,
) -> Result<Disc, String> {
    let mut disc = Disc::open(source)?;
    let _ = tx.send(LoadMsg::Progress(0.8, "DVD wird analysiert …".into()));
    if disc.check_scrambled() {
        return Err(COPY_PROTECTED.into());
    }
    let tx2 = tx.clone();
    let mut cb = |f: f32, t: &str| -> bool {
        let _ = tx2.send(LoadMsg::Progress(0.8 + f * 0.2, t.to_string()));
        !cancel.load(Ordering::Relaxed)
    };
    let scrambled = disc.analyse(false, &mut cb)?;
    if scrambled {
        return Err(COPY_PROTECTED.into());
    }
    Ok(disc)
}

const COPY_PROTECTED: &str = "Diese DVD ist kopiergeschützt (CSS). Das Programm verarbeitet bewusst nur ungeschützte Inhalte, etwa selbst erstellte DVDs oder eigene VIDEO_TS-Ordner.";

/// Wählt beim Öffnen nur die bevorzugte Sprache, sofern es sie gibt.
fn apply_language(disc: &Disc, plan: &mut Plan, lang: &str) {
    for (i, t) in disc.titles.iter().enumerate() {
        let id = ItemId::Title(i);
        let mut s = plan.settings(id);
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
        plan.items.insert(id, s);
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_loader(ctx);
        self.pump_job(ctx);
        self.handle_dropped(ctx);
        self.logic_screenshot(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let p = self.pal;
        let page = egui::Frame::new().fill(p.bg).inner_margin(Margin::same(0));
        if let Some(t) = self.thumbs.as_mut() {
            t.pump(&ctx);
        }
        if let Some(l) = self.hover_loop.as_mut() {
            l.seen_this_frame = false;
        }
        egui::CentralPanel::default().frame(page).show(ui, |ui| {
            self.view_topbar(ui, &ctx);
            if self.disc.is_some() {
                self.view_workspace(ui, &ctx);
            } else {
                self.view_start(ui, &ctx);
            }
        });
        self.view_dialogs(&ctx);
        self.pump_preview(&ctx);
        // Schleife nur, solange die Maus über dem Vorschaubild steht
        if self.hover_loop.as_ref().is_some_and(|l| !l.seen_this_frame) {
            self.hover_loop = None;
        }
        if let Some(l) = self.hover_loop.as_mut() {
            l.pump(&ctx);
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if self.screenshot.is_some() {
            return;
        }
        let flag = |b: bool| if b { "1".to_string() } else { "0".to_string() };
        storage.set_string("dark", flag(self.prefs.dark));
        storage.set_string("lang", self.prefs.lang.clone());
        storage.set_string("only_lang", flag(self.prefs.only_lang));
        storage.set_string("deep", flag(self.prefs.deep_default));
        // Die kleine Zielgröße der Beispiel-DVD nicht als Vorgabe behalten
        let (kind, mb) = self.target_before_sample.unwrap_or((self.prefs.target_kind, self.prefs.custom_mb));
        storage.set_string("custom_mb", mb.to_string());
        storage.set_string(
            "backup_kind",
            match self.prefs.backup_kind {
                OutputKind::Folder => "folder",
                OutputKind::Vob => "vob",
                OutputKind::Mpeg => "mpeg",
                OutputKind::Burn => "burn",
                OutputKind::Iso => "iso",
            }
            .into(),
        );
        storage.set_string("folder_dir", self.prefs.folder_dir.clone());
        storage.set_string("jump_secs", self.prefs.jump_secs.to_string());
        storage.set_string("file_dir", self.prefs.file_dir.clone());
        let p = &self.prefs;
        for (key, v) in [
            ("no_split", p.no_split),
            ("remove_puo", self.plan.remove_puo),
            ("region_free", self.plan.region_free),
            ("vob_keep_target", p.vob_keep_target),
            ("compressed", self.preview.compressed),
            ("cut_view", self.preview.cut_view),
            ("cut_exact", p.cut_exact),
        ] {
            storage.set_string(key, flag(v));
        }
        storage.set_string(
            "target",
            match kind {
                TargetKind::Dvd5 => "5",
                TargetKind::Dvd9 => "9",
                TargetKind::Custom => "c",
                TargetKind::Unlimited => "u",
            }
            .into(),
        );
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        let c: Color32 = self.pal.bg;
        egui::Rgba::from(c).to_array()
    }
}

