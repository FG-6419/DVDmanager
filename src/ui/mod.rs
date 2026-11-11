//! Die Anwendung: Zustand, Hintergrundarbeiten und der Bildaufbau je Frame.

mod dialogs;
mod preview;
mod queue;
mod scenes;
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
use discslim::engine::batch::Queue;
use discslim::engine::plan::{dvd5_sectors, dvd9_sectors, ItemId, Mode, Plan, NO_TARGET};
use discslim::engine::project::{self, Project};

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
    /// Vorschaubild eines Titels: Nummer, Länge (ms), Bild
    Thumb(usize, u64, egui::ColorImage),
    Done(Box<Disc>),
    Failed(String),
}

struct Loader {
    rx: Receiver<LoadMsg>,
    progress: f32,
    text: String,
    cancel: Arc<AtomicBool>,
    is_sample: bool,
    /// Vorschaubilder der Titel, die schon fertig sind (Nummer, Länge, Bild)
    thumbs: Vec<(usize, u64, egui::TextureHandle)>,
}

/// Höchstens so viele Vorschaubilder beim Einlesen
const LOAD_THUMBS: usize = 6;

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
    /// Abstand für "Kapitel gleichmäßig verteilen" (Minuten)
    pub chapter_every_min: u32,
    /// Warteschlange: Ausgabe (ISO oder Ordner) und Zielordner für neu hinzugefügte DVDs
    pub batch_kind: OutputKind,
    pub batch_dir: String,
    /// Hauptverzeichnisse aus den Einstellungen: dort öffnen die Dialoge nach dem Start.
    pub load_dir: String,
    pub save_dir: String,
    /// Ordner für Projektdateien (Öffnen und Speichern)
    pub project_dir: String,
    /// Ton an Schnittstellen kurz blenden, Ausgabe prüfen, Übersichtsbild und sein Bildabstand
    pub declick: bool,
    pub verify: bool,
    pub sheet: bool,
    pub sheet_secs: u32,
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
            chapter_every_min: 5,
            batch_kind: OutputKind::Iso,
            batch_dir: String::new(),
            load_dir: String::new(),
            save_dir: String::new(),
            project_dir: String::new(),
            declick: true,
            verify: true,
            sheet: false,
            sheet_secs: 60,
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

    /// Projekt, dessen DVD gerade geladen wird; danach gilt sein Plan.
    pending_project: Option<Project>,
    /// Datei, in der das aktuelle Projekt zuletzt gespeichert oder aus der es geladen wurde
    project_path: Option<PathBuf>,
    /// Kurze Meldung in der Kopfzeile (Text, seit)
    notice: Option<(String, Instant)>,
    /// Stapelverarbeitung
    queue: Queue,
    queue_open: bool,
    /// Szenenerkennung für einen Eintrag
    scene_dlg: Option<scenes::SceneDialog>,
    /// Zuletzt gewählte Orte in dieser Sitzung (zum Laden bzw. Speichern)
    last_load_dir: Option<PathBuf>,
    last_save_dir: Option<PathBuf>,
    /// In dieser Sitzung zuletzt gewählter Projektordner
    last_project_dir: Option<PathBuf>,
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
            if let Some(v) = st.get_string("chapter_every_min").and_then(|v| v.parse().ok()) {
                prefs.chapter_every_min = v;
            }
            if let Some(v) = st.get_string("batch_kind") {
                prefs.batch_kind = if v == "folder" { OutputKind::Folder } else { OutputKind::Iso };
            }
            if let Some(v) = st.get_string("load_dir") {
                prefs.load_dir = v;
            }
            if let Some(v) = st.get_string("project_dir") {
                prefs.project_dir = v;
            }
            if let Some(v) = st.get_string("save_dir") {
                prefs.save_dir = v;
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
                ("declick", &mut prefs.declick),
                ("verify", &mut prefs.verify),
                ("sheet", &mut prefs.sheet),
            ] {
                if let Some(v) = flag(key) {
                    *slot = v;
                }
            }
            if let Some(v) = st.get_string("sheet_secs").and_then(|v| v.parse().ok()) {
                prefs.sheet_secs = v;
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
        // Sprache der Oberfläche: gespeichert, sonst wie Windows
        let ui_lang = cc
            .storage
            .and_then(|st| st.get_string("ui_lang"))
            .and_then(|v| discslim::i18n::Lang::from_code(&v))
            .unwrap_or_else(discslim::i18n::system_lang);
        discslim::i18n::set_lang(ui_lang);
        // Für die Prüfbilder immer das Standard-Design, Sprache nach DISCSLIM_LANG (sonst Deutsch)
        if screenshot.is_some() {
            prefs = Prefs::default();
            let lang = std::env::var("DISCSLIM_LANG").ok().and_then(|v| discslim::i18n::Lang::from_code(&v));
            discslim::i18n::set_lang(lang.unwrap_or(discslim::i18n::Lang::De));
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
            pending_project: None,
            project_path: None,
            notice: None,
            queue: Queue::default(),
            queue_open: false,
            scene_dlg: None,
            last_load_dir: None,
            last_save_dir: None,
            last_project_dir: None,
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
    /// Eine Projektdatei öffnet ihre DVD und stellt danach alles wieder her.
    fn open_path(&mut self, ctx: &egui::Context, path: PathBuf) {
        if Project::is_project_path(&path) {
            self.open_project(ctx, &path);
            return;
        }
        self.pending_project = None;
        self.project_path = None;
        self.start_loader(ctx, false, move |tx, cancel| {
            let source = Source::open(&path)?;
            load_and_analyse(source, tx, cancel)
        });
    }

    /// Lädt eine Projektdatei und liest die zugehörige DVD neu ein.
    fn open_project(&mut self, ctx: &egui::Context, path: &Path) {
        let proj = match Project::load(path) {
            Ok(p) => p,
            Err(e) => {
                self.error = Some(e);
                return;
            }
        };
        let source = proj.source.clone();
        if !source.exists() {
            self.error = Some(trf!(
                "Die DVD zum Projekt „{}“ ist nicht mehr da:\n{}\n\nLege die DVD ein oder stelle den Ordner bzw. die ISO-Datei wieder an diesen Ort.", "The DVD for project \"{}\" is no longer available:\n{}\n\nInsert the DVD or put the folder or ISO file back in this location.",
                proj.label,
                source.display()
            ));
            return;
        }
        self.start_loader(ctx, false, move |tx, cancel| {
            let source = Source::open(&source)?;
            load_and_analyse(source, tx, cancel)
        });
        self.pending_project = Some(proj);
        self.project_path = Some(path.to_path_buf());
    }

    /// Startort der Dialoge zum Laden: zuletzt gewählt, sonst das Hauptverzeichnis.
    pub(super) fn load_start_dir(&self) -> Option<PathBuf> {
        self.last_load_dir
            .clone()
            .or_else(|| Some(PathBuf::from(self.prefs.load_dir.trim())).filter(|p| !p.as_os_str().is_empty()))
            .filter(|p| p.is_dir())
    }

    /// Startort der Dialoge zum Speichern: zuletzt gewählt, sonst das Hauptverzeichnis,
    /// sonst der beim letzten Backup benutzte Ort, sonst „Videos“.
    pub(super) fn save_start_dir(&self) -> PathBuf {
        let set = |s: &str| Some(PathBuf::from(s.trim())).filter(|p| !p.as_os_str().is_empty() && p.is_dir());
        self.last_save_dir
            .clone()
            .filter(|p| p.is_dir())
            .or_else(|| set(&self.prefs.save_dir))
            .or_else(|| set(&self.prefs.file_dir))
            .or_else(|| set(&self.prefs.folder_dir))
            .unwrap_or_else(|| dialogs::dirs_home().join("Videos"))
    }

    /// Merkt sich den Ort einer geladenen DVD, ISO oder Projektdatei. Bei einem
    /// VIDEO_TS-Ordner ist das der Ordner, in dem die DVD-Ordner liegen.
    pub(super) fn note_load(&mut self, picked: &Path) {
        let mut dir = picked.parent();
        if picked.file_name().is_some_and(|n| n.eq_ignore_ascii_case("VIDEO_TS")) {
            dir = dir.and_then(Path::parent);
        }
        if let Some(d) = dir.filter(|d| !d.as_os_str().is_empty()) {
            self.last_load_dir = Some(d.to_path_buf());
        }
    }

    /// Merkt sich den Ordner, in dem gespeichert wurde.
    pub(super) fn note_save_dir(&mut self, dir: &Path) {
        if !dir.as_os_str().is_empty() {
            self.last_save_dir = Some(dir.to_path_buf());
        }
    }

    /// Dateidialog zum Laden, im zuletzt benutzten Ordner.
    pub(super) fn load_dialog(&self, title: &str) -> rfd::FileDialog {
        let d = rfd::FileDialog::new().set_title(title);
        match self.load_start_dir() {
            Some(dir) => d.set_directory(dir),
            None => d,
        }
    }

    /// Dateidialog zum Speichern, im zuletzt benutzten Ordner.
    pub(super) fn save_dialog(&self, title: &str) -> rfd::FileDialog {
        rfd::FileDialog::new().set_title(title).set_directory(self.save_start_dir())
    }

    /// Startort für Projektdialoge: in dieser Sitzung zuletzt gewählt, sonst der
    /// Projektordner aus den Einstellungen.
    fn project_start_dir(&self) -> Option<PathBuf> {
        self.last_project_dir
            .clone()
            .or_else(|| Some(PathBuf::from(self.prefs.project_dir.trim())).filter(|p| !p.as_os_str().is_empty()))
            .filter(|p| p.is_dir())
    }

    fn note_project_dir(&mut self, file: &Path) {
        if let Some(d) = file.parent().filter(|d| !d.as_os_str().is_empty()) {
            self.last_project_dir = Some(d.to_path_buf());
        }
    }

    pub(super) fn pick_project(&mut self, ctx: &egui::Context) {
        let mut d = self.load_dialog(tr!("Projekt öffnen", "Open project")).add_filter(tr!("DVDmanager-Projekt", "DVDmanager project"), &[project::EXTENSION]);
        if let Some(dir) = self.project_start_dir() {
            d = d.set_directory(dir);
        }
        if let Some(p) = d.pick_file() {
            self.note_project_dir(&p);
            self.open_project(ctx, &p);
        }
    }

    /// Speichert die Einstellungen als Projektdatei. Ohne bekannten Ort oder mit
    /// `ask` wird nach dem Ort gefragt.
    pub(super) fn save_project(&mut self, ask: bool) {
        let Some(disc) = self.disc.clone() else { return };
        let path = match (&self.project_path, ask) {
            (Some(p), false) => p.clone(),
            _ => {
                let name = self
                    .project_path
                    .as_ref()
                    .and_then(|p| p.file_name())
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_else(|| format!("{}.{}", dialogs::file_stem(&disc.source.label), project::EXTENSION));
                let mut d = self
                    .save_dialog(tr!("Projekt speichern", "Save project"))
                    .add_filter(tr!("DVDmanager-Projekt", "DVDmanager project"), &[project::EXTENSION])
                    .set_file_name(name);
                // Ein schon gespeichertes Projekt bleibt in seinem Ordner, neue kommen in den Projektordner.
                let dir = self
                    .project_path
                    .as_ref()
                    .and_then(|p| p.parent())
                    .filter(|d| d.is_dir())
                    .map(Path::to_path_buf)
                    .or_else(|| self.project_start_dir());
                if let Some(dir) = dir {
                    d = d.set_directory(dir);
                }
                let Some(mut p) = d.save_file() else { return };
                if !Project::is_project_path(&p) {
                    p.set_extension(project::EXTENSION);
                }
                self.note_project_dir(&p);
                p
            }
        };
        match Project::new(&disc, &self.plan).save(&path) {
            Ok(()) => {
                let time = clock_now();
                self.notice = Some((
                    trf!("Projekt gespeichert: {} ({time})", "Project saved: {} ({time})", path.file_name().unwrap_or_default().to_string_lossy()),
                    Instant::now(),
                ));
                self.project_path = Some(path);
            }
            Err(e) => self.error = Some(trf!("Das Projekt konnte nicht gespeichert werden.\n{e}", "The project could not be saved.\n{e}")),
        }
    }

    /// Stellt die Zielgröße in der Oberfläche passend zum Plan ein.
    fn sync_target_kind(&mut self) {
        let s = self.plan.target_sectors;
        self.prefs.target_kind = if s == dvd5_sectors() {
            TargetKind::Dvd5
        } else if s == dvd9_sectors() {
            TargetKind::Dvd9
        } else if s >= NO_TARGET {
            TargetKind::Unlimited
        } else {
            self.prefs.custom_mb = ((s * 2048) / (1024 * 1024)).max(1) as u32;
            TargetKind::Custom
        };
    }

    /// Erzeugt die Beispiel-DVD und öffnet sie.
    fn open_sample(&mut self, ctx: &egui::Context) {
        self.pending_project = None;
        self.project_path = None;
        self.start_loader(ctx, true, move |tx, cancel| {
            let _ = tx.send(LoadMsg::Progress(0.0, tr!("Beispiel-DVD wird erzeugt …", "Creating sample DVD …").into()));
            let tx2 = tx.clone();
            let dir = discslim::demo::build_sample(&discslim::demo::sample_dir(), |f| {
                let _ = tx2.send(LoadMsg::Progress(f * 0.8, tr!("Beispiel-DVD wird erzeugt …", "Creating sample DVD …").into()));
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
                Err(_) => LoadMsg::Failed(tr!("Die DVD konnte nicht gelesen werden (interner Fehler).", "The DVD could not be read (internal error).").into()),
            };
            let _ = tx.send(msg);
            ctx2.request_repaint();
        });
        self.loader = Some(Loader {
            rx,
            progress: 0.0,
            text: tr!("DVD wird geöffnet …", "Opening DVD …").into(),
            cancel,
            is_sample,
            thumbs: Vec::new(),
        });
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
                LoadMsg::Thumb(n, ms, img) => {
                    let tex = ctx.load_texture(format!("laden-{n}"), img, egui::TextureOptions::LINEAR);
                    l.thumbs.push((n, ms, tex));
                    l.thumbs.sort_by_key(|t| t.0);
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
                if self.pending_project.take().is_some() {
                    self.project_path = None;
                }
                self.preview.dirty = true;
                if e != tr!("Abgebrochen", "Cancelled") {
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
            plan.keep_only_language(&disc, &self.prefs.lang);
        }
        self.plan = plan;
        self.sel = disc
            .titles
            .iter()
            .position(|t| t.category == Category::MainMovie)
            .map(|i| Selection::Item(ItemId::Title(i)))
            .unwrap_or(Selection::None);
        // Geladenes Projekt: seine Einstellungen ersetzen die Vorgaben.
        if let Some(proj) = self.pending_project.take() {
            match proj.plan_for(&disc) {
                Ok(plan) => {
                    self.plan = plan;
                    self.sync_target_kind();
                    if self.plan.mode == Mode::Reauthor {
                        self.sel = if self.plan.reauthor.is_empty() { Selection::None } else { Selection::Entry(0) };
                    }
                    self.notice = Some((trf!("Projekt „{}“ geladen", "Project \"{}\" loaded", proj.label), Instant::now()));
                }
                Err(e) => {
                    self.project_path = None;
                    self.error = Some(trf!("{e}\n\nDie DVD wurde mit den Standard-Einstellungen geöffnet.", "{e}\n\nThe DVD was opened with the default settings."));
                }
            }
        }
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
        if let Some(p) = self.load_dialog(tr!("DVD-Ordner wählen (VIDEO_TS)", "Choose DVD folder (VIDEO_TS)")).pick_folder() {
            self.note_load(&p);
            self.open_path(ctx, p);
        }
    }

    fn pick_iso(&mut self, ctx: &egui::Context) {
        if let Some(p) = self
            .load_dialog(tr!("ISO-Abbild wählen", "Choose ISO image"))
            .add_filter(tr!("DVD-Abbild", "DVD image"), &["iso", "img"])
            .add_filter(tr!("Alle Dateien", "All files"), &["*"])
            .pick_file()
        {
            self.note_load(&p);
            self.open_path(ctx, p);
        }
    }

    fn handle_dropped(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).filter(|p| !p.as_os_str().is_empty()).collect());
        if dropped.is_empty() {
            return;
        }
        // Bei offener Warteschlange landen alle gezogenen DVDs und Projekte dort.
        if self.queue_open {
            self.queue_add_paths(&dropped);
            return;
        }
        if self.job.is_none() && self.loader.is_none() {
            self.note_load(&dropped[0]);
            self.open_path(ctx, dropped[0].clone());
        }
    }

    /// Tastenkürzel: Strg+S speichert das Projekt, Strg+Umschalt+S fragt nach dem Ort.
    fn handle_shortcuts(&mut self, ctx: &egui::Context) {
        if self.disc.is_none() || self.loader.is_some() || self.job.is_some() {
            return;
        }
        let (save, save_as) = ctx.input_mut(|i| {
            let as_ = i.consume_key(egui::Modifiers::COMMAND | egui::Modifiers::SHIFT, egui::Key::S);
            let plain = i.consume_key(egui::Modifiers::COMMAND, egui::Key::S);
            (plain, as_)
        });
        if save || save_as {
            self.save_project(save_as);
        }
    }

    fn logic_screenshot(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else { return };
        ctx.request_repaint();
        if self.shot_taken.load(Ordering::SeqCst) {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            return;
        }
        // Mit dem Zählen warten, bis eine Ladeaktion fertig ist (außer beim Prüfbild des Ladedialogs).
        #[cfg(debug_assertions)]
        let show_loading = self.dev_state.as_deref() == Some("loading");
        #[cfg(not(debug_assertions))]
        let show_loading = false;
        if self.loader.is_some() && !show_loading {
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
            ("save", _) if self.disc.is_some() && self.loader.is_none() => {
                // Projekt laden, speichern, beenden (prüft, ob wirklich geschrieben wird)
                let before = format!("{:?}", self.project_path);
                self.save_project(false);
                let _ = std::fs::write(
                    std::env::var("DISCSLIM_DEV_LOG").unwrap_or_else(|_| "devlog.txt".into()),
                    format!("vorher {before}\nnachher {:?}\nMeldung {:?}\nFehler {:?}\n", self.project_path, self.notice.as_ref().map(|n| n.0.clone()), self.error),
                );
                self.dev_state = None;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
            ("backup", 5) => self.open_backup_dialog(),
            ("about", 5) => self.about_open = true,
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
            ("fadeall", 5) => {
                // Zwei Einträge; der erste hat eine Blende, die auf den zweiten übertragen wird
                self.dev_state = Some("fade".into());
                self.apply_dev_state(ctx);
                if let Some(d) = self.disc.clone() {
                    let t = &d.titles[1.min(d.titles.len() - 1)];
                    let e = discslim::engine::plan::ReauthorEntry::new(1.min(d.titles.len() - 1), 0, t.chapters.len().saturating_sub(1), Default::default());
                    self.plan.reauthor.push(e);
                }
                self.dev_state = Some("fadeall".into());
            }
            ("fade", 5) => {
                // Schnitt mit Blenden; die Vorschau steht mitten im Einblenden
                self.plan.mode = Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    let t = &d.titles[0];
                    let mut e = discslim::engine::plan::ReauthorEntry::new(0, 0, t.chapters.len().saturating_sub(1), Default::default());
                    e.cut = discslim::engine::plan::Cut { start_ms: Some(8_000), end_ms: Some(41_500), exact: true };
                    e.fade = discslim::engine::plan::Fades {
                        fade_in: discslim::engine::plan::FadeKind::Black,
                        fade_out: discslim::engine::plan::FadeKind::White,
                        ms: 1_500,
                        audio: false,
                    };
                    self.plan.reauthor.push(e);
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("fade", 40) => self.preview.seek_time(8_600.0, true),
            ("scenes", 5) => {
                // Szenendialog mit fertiger Analyse des Hauptfilms
                self.plan.mode = Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    let t = &d.titles[0];
                    let mut e = discslim::engine::plan::ReauthorEntry::new(0, 0, t.chapters.len().saturating_sub(1), Default::default());
                    e.cut = discslim::engine::plan::Cut::whole(t.ms, false);
                    self.plan.reauthor.push(e);
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
                self.open_scene_dialog(0, ctx);
                if let Some(dlg) = self.scene_dlg.as_mut() {
                    dlg.wait_for_result();
                }
            }
            ("scenes", 30) => {
                if let Some(dlg) = self.scene_dlg.as_mut() {
                    dlg.deselect(1);
                }
            }
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
            // Übergang anhören: Eintrag 1 = Titel 1 ab 10 s, Eintrag 2 = Titel 3 ab 5 s.
            // Protokolliert die Wiedergabe und wartet mit dem Prüfbild, bis sie angehalten hat.
            ("listen", 5) => {
                self.plan.mode = discslim::engine::plan::Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    // DISCSLIM_LISTEN=same|samekey: zwei aneinanderliegende Szenen desselben Titels
                    let variant = std::env::var("DISCSLIM_LISTEN").unwrap_or_default();
                    let parts: [(usize, u64, u64); 2] = if variant.starts_with("same") {
                        [(0, 10_000, 20_000), (0, 20_000, 30_000)]
                    } else {
                        [(0, 10_000, 20_000), (2, 5_000, 15_000)]
                    };
                    let exact = variant != "samekey";
                    for (t, a, b) in parts {
                        let mut e = discslim::engine::plan::ReauthorEntry::new(t, 0, d.titles[t].chapters.len().saturating_sub(1), Default::default());
                        e.cut = discslim::engine::plan::Cut { start_ms: Some(a), end_ms: Some(b), exact };
                        self.plan.reauthor.push(e);
                    }
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("listen", 40) => self.listen_junction(0),
            ("listen", n) if n > 40 => {
                if let Ok(log) = std::env::var("DISCSLIM_DEV_LOG") {
                    use std::io::Write;
                    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(log) {
                        let _ = writeln!(f, "{} {}", clock_now(), self.preview.dev_status());
                    }
                }
                if self.preview.listen_until.is_some() || self.preview.is_playing() {
                    self.frames = 100; // Prüfbild erst nach dem Anhalten
                }
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
            ("loading", 2) => {
                // Ladedialog mit Vorschaubildern: die DVD öffnen und das Einlesen offen halten
                if let Some(d) = self.disc.take() {
                    let origin = d.source.origin.clone();
                    self.start_loader(ctx, false, move |tx, _cancel| {
                        let disc = Disc::open(Source::open(&origin)?)?;
                        let _ = tx.send(LoadMsg::Progress(0.86, tr!("DVD wird analysiert …", "Analysing DVD …").into()));
                        spawn_load_thumbs(&disc, tx.clone());
                        std::thread::sleep(std::time::Duration::from_secs(30));
                        Ok(disc)
                    });
                }
            }
            ("chapters", 5) => {
                // Eintrag mit eigenen Kapitelmarken, eine davon außerhalb des Schnitts
                self.plan.mode = Mode::Reauthor;
                if let Some(d) = self.disc.clone() {
                    let t = &d.titles[0];
                    let mut e = discslim::engine::plan::ReauthorEntry::new(0, 0, t.chapters.len().saturating_sub(1), Default::default());
                    let total = discslim::engine::plan::entry_duration_ms(&d, &e);
                    e.cut.end_ms = Some(total * 7 / 8);
                    e.chapter_marks = Some(vec![total / 5, total * 2 / 5, total * 3 / 5, total * 9 / 10]);
                    self.plan.reauthor.push(e);
                }
                self.sel = Selection::Entry(0);
                self.preview.dirty = true;
            }
            ("queue", 5) => {
                use discslim::engine::batch::{AutoSettings, BatchItem, PlanSource, Status};
                let auto = AutoSettings { target_sectors: dvd5_sectors(), only_lang: None, remove_puo: false, region_free: false, split_vob: true };
                let src = self.disc.as_ref().map(|d| d.source.origin.clone()).unwrap_or_default();
                for (label, status, f) in [
                    ("URLAUB_2019", Status::Failed("Diese DVD ist kopiergeschützt (CSS) und wird nicht verarbeitet.".into()), 0.0),
                    ("HOCHZEIT", Status::Running, 0.42),
                    ("KINDERGEBURTSTAG", Status::Waiting, 0.0),
                    ("WEIHNACHTEN", Status::Waiting, 0.0),
                ] {
                    let mut it = BatchItem::new(src.clone(), label.into(), PlanSource::Auto(auto.clone()), discslim::engine::backup::Output::Iso(PathBuf::from(format!("C:\\Videos\\{label} (klein).iso"))), false);
                    it.status = status;
                    it.fraction = f;
                    it.stage = "Titelset 1".into();
                    self.queue.push(it);
                }
                self.queue_open = true;
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
            // Ergebnisfenster mit Prüfung und Übersichtsbild (sauber bzw. mit Fund)
            ("result" | "resultbad", 5) => {
                let bad = state == "resultbad";
                let check = discslim::engine::inspect::CheckReport {
                    audio_breaks: if bad { vec![83.2, 1245.0] } else { Vec::new() },
                    time_jumps: if bad { vec![1245.0] } else { Vec::new() },
                    seconds: 1830.0,
                    tracks: 1,
                };
                self.result = Some(JobResult::Ok(BackupSummary {
                    path: PathBuf::from(r"D:\Filme\Szenen.mpg"),
                    sectors: 900_000,
                    target_sectors: u64::MAX,
                    source_sectors: 1_400_000.0,
                    seconds: 95.0,
                    fits: true,
                    burn_started: false,
                    check: Some(check),
                    sheet: Some(PathBuf::from(r"D:\Filme\Szenen.jpg")),
                    extra_error: None,
                }));
            }
            _ => {}
        }
    }
}

/// Aktuelle Ortszeit als „HH:MM:SS“.
fn clock_now() -> String {
    #[cfg(windows)]
    {
        // SAFETY: GetLocalTime füllt nur die zurückgegebene Struktur.
        let t = unsafe { windows::Win32::System::SystemInformation::GetLocalTime() };
        format!("{:02}:{:02}:{:02}", t.wHour, t.wMinute, t.wSecond)
    }
    #[cfg(not(windows))]
    {
        let s = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0) % 86_400;
        format!("{:02}:{:02}:{:02} UTC", s / 3600, s / 60 % 60, s % 60)
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
    let _ = tx.send(LoadMsg::Progress(0.8, tr!("DVD wird analysiert …", "Analysing DVD …").into()));
    if disc.check_scrambled() {
        return Err(copy_protected().into());
    }
    spawn_load_thumbs(&disc, tx.clone());
    let tx2 = tx.clone();
    let mut cb = |f: f32, t: &str| -> bool {
        let _ = tx2.send(LoadMsg::Progress(0.8 + f * 0.2, t.to_string()));
        !cancel.load(Ordering::Relaxed)
    };
    let scrambled = disc.analyse(false, &mut cb)?;
    if scrambled {
        return Err(copy_protected().into());
    }
    Ok(disc)
}

/// Holt neben der Analyse für die längsten Titel je ein Vorschaubild. Läuft in einem
/// eigenen Thread und endet von selbst, wenn das Einlesen vorbei ist.
fn spawn_load_thumbs(disc: &Disc, tx: std::sync::mpsc::Sender<LoadMsg>) {
    let mut picks: Vec<usize> = (0..disc.titles.len()).filter(|i| disc.titles[*i].ms > 0).collect();
    picks.sort_by_key(|i| std::cmp::Reverse(disc.titles[*i].ms));
    picks.truncate(LOAD_THUMBS);
    let jobs: Vec<_> = picks
        .into_iter()
        .map(|i| {
            let t = &disc.titles[i];
            let segments = discslim::engine::preview::title_segments(disc, i, 0, t.chapters.len().saturating_sub(1));
            // Nicht ganz am Anfang: dort ist oft noch Schwarzbild oder ein Logo.
            let at = (t.ms as f64 * 0.15).min(300_000.0);
            (t.number, t.ms, segments, at)
        })
        .collect();
    let source = disc.source.clone();
    std::thread::spawn(move || {
        for (n, ms, segments, at) in jobs {
            if segments.is_empty() {
                continue;
            }
            let Some(img) = thumbs::grab_frame(source.clone(), segments, at, std::time::Duration::from_secs(5)) else { continue };
            if tx.send(LoadMsg::Thumb(n, ms, img)).is_err() {
                return; // Einlesen schon fertig
            }
        }
    });
}

fn copy_protected() -> &'static str {
    tr!("Diese DVD ist kopiergeschützt (CSS). Das Programm verarbeitet bewusst nur ungeschützte Inhalte, etwa selbst erstellte DVDs oder eigene VIDEO_TS-Ordner.", "This DVD is copy-protected (CSS). The program deliberately processes only unprotected content, such as self-made DVDs or your own VIDEO_TS folders.")
}


impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.pump_loader(ctx);
        self.pump_job(ctx);
        self.handle_dropped(ctx);
        self.handle_shortcuts(ctx);
        if self.queue.is_running() {
            // Fortschritt der Warteschlange sichtbar halten
            ctx.request_repaint_after(std::time::Duration::from_millis(250));
        }
        if self.notice.as_ref().is_some_and(|n| n.1.elapsed().as_secs() >= 4) {
            self.notice = None;
        }
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
        storage.set_string("chapter_every_min", self.prefs.chapter_every_min.to_string());
        storage.set_string("batch_kind", if self.prefs.batch_kind == OutputKind::Folder { "folder" } else { "iso" }.into());
        storage.set_string("ui_lang", discslim::i18n::lang().code().into());
        storage.set_string("load_dir", self.prefs.load_dir.clone());
        storage.set_string("save_dir", self.prefs.save_dir.clone());
        storage.set_string("project_dir", self.prefs.project_dir.clone());
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
            ("declick", p.declick),
            ("verify", p.verify),
            ("sheet", p.sheet),
        ] {
            storage.set_string(key, flag(v));
        }
        storage.set_string("sheet_secs", p.sheet_secs.to_string());
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

