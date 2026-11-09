//! Stapelverarbeitung: Mehrere DVDs nacheinander sichern. Die Warteschlange läuft in
//! einem eigenen Thread; Einträge können auch während des Laufs dazukommen.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::channel;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;

use crate::dvd::model::Disc;
use crate::dvd::source::Source;
use crate::dvd::Res;
use crate::engine::backup::{self, BackupMsg, BackupRequest, BackupSummary, Output};
use crate::engine::plan::{Plan, VideoMode};
use crate::engine::project::{self, Fingerprint};

/// Woher der Plan für einen Eintrag kommt.
#[derive(Clone, Debug)]
pub enum PlanSource {
    /// Fest eingestellt (aus dem Programm oder einer Projektdatei); gilt nur für die passende DVD.
    Fixed { plan: Box<Plan>, fingerprint: Fingerprint },
    /// Ganze DVD, Bild automatisch so weit verkleinert, dass sie aufs Ziel passt.
    Auto(AutoSettings),
}

#[derive(Clone, Debug)]
pub struct AutoSettings {
    pub target_sectors: u64,
    /// Nur Ton und Untertitel dieser Sprache behalten (falls vorhanden)
    pub only_lang: Option<String>,
    pub remove_puo: bool,
    pub region_free: bool,
    pub split_vob: bool,
}

#[derive(Clone, Debug)]
pub enum Status {
    Waiting,
    Running,
    Done(BackupSummary),
    Failed(String),
    Cancelled,
}

impl Status {
    pub fn is_finished(&self) -> bool {
        matches!(self, Status::Done(_) | Status::Failed(_) | Status::Cancelled)
    }
}

#[derive(Clone, Debug)]
pub struct BatchItem {
    pub id: u64,
    pub source: PathBuf,
    pub label: String,
    pub plan: PlanSource,
    pub output: Output,
    pub deep_analysis: bool,
    pub status: Status,
    /// Fortschritt des laufenden Eintrags (0..1) und was gerade passiert
    pub fraction: f32,
    pub stage: String,
}

impl BatchItem {
    pub fn new(source: PathBuf, label: String, plan: PlanSource, output: Output, deep_analysis: bool) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        Self {
            id: NEXT.fetch_add(1, Ordering::Relaxed),
            source,
            label,
            plan,
            output,
            deep_analysis,
            status: Status::Waiting,
            fraction: 0.0,
            stage: String::new(),
        }
    }

    /// Wohin der Eintrag schreibt (Ordner oder Datei).
    pub fn output_path(&self) -> &std::path::Path {
        match &self.output {
            Output::Folder(p) | Output::Iso(p) | Output::Burn(p) | Output::Vob(p) => p,
            Output::Mpeg { path, .. } => path,
        }
    }
}

type Notify = Arc<dyn Fn() + Send + Sync>;

struct Runner {
    stop: Arc<AtomicBool>,
    cancel: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

/// Die Warteschlange. Die Liste ist geteilt, damit Oberfläche und Arbeitsthread sie sehen.
#[derive(Default)]
pub struct Queue {
    items: Arc<Mutex<Vec<BatchItem>>>,
    runner: Option<Runner>,
}

impl Queue {
    pub fn items(&self) -> MutexGuard<'_, Vec<BatchItem>> {
        self.items.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, item: BatchItem) {
        self.items().push(item);
    }

    /// Entfernt einen Eintrag, außer er läuft gerade.
    pub fn remove(&self, id: u64) {
        self.items().retain(|i| i.id != id || matches!(i.status, Status::Running));
    }

    /// Verschiebt einen wartenden Eintrag um eine Stelle.
    pub fn move_by(&self, id: u64, up: bool) {
        let mut items = self.items();
        let Some(i) = items.iter().position(|x| x.id == id) else { return };
        let j = if up { i.checked_sub(1) } else { Some(i + 1).filter(|j| *j < items.len()) };
        if let Some(j) = j {
            items.swap(i, j);
        }
    }

    /// Nimmt fertige, fehlgeschlagene und abgebrochene Einträge aus der Liste.
    pub fn clear_finished(&self) {
        self.items().retain(|i| !i.status.is_finished());
    }

    /// Fehlgeschlagene und abgebrochene Einträge noch einmal versuchen.
    pub fn retry_failed(&self) {
        for i in self.items().iter_mut() {
            if matches!(i.status, Status::Failed(_) | Status::Cancelled) {
                i.status = Status::Waiting;
                i.fraction = 0.0;
                i.stage.clear();
            }
        }
    }

    pub fn is_running(&self) -> bool {
        self.runner.as_ref().is_some_and(|r| r.handle.as_ref().is_some_and(|h| !h.is_finished()))
    }

    pub fn waiting(&self) -> usize {
        self.items().iter().filter(|i| matches!(i.status, Status::Waiting)).count()
    }

    /// Startet die Abarbeitung aller wartenden Einträge.
    pub fn start(&mut self, notify: impl Fn() + Send + Sync + 'static) {
        if self.is_running() {
            return;
        }
        let stop = Arc::new(AtomicBool::new(false));
        let cancel = Arc::new(AtomicBool::new(false));
        let items = self.items.clone();
        let notify: Notify = Arc::new(notify);
        let (s2, c2) = (stop.clone(), cancel.clone());
        let handle = std::thread::spawn(move || worker(items, s2, c2, notify));
        self.runner = Some(Runner { stop, cancel, handle: Some(handle) });
    }

    /// Bricht den laufenden Eintrag ab und hält die Warteschlange an.
    pub fn cancel(&self) {
        if let Some(r) = &self.runner {
            r.stop.store(true, Ordering::Relaxed);
            r.cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Hält nach dem laufenden Eintrag an.
    pub fn stop_after_current(&self) {
        if let Some(r) = &self.runner {
            r.stop.store(true, Ordering::Relaxed);
        }
    }

    /// Wartet, bis der Arbeitsthread fertig ist.
    pub fn wait(&mut self) {
        if let Some(h) = self.runner.as_mut().and_then(|r| r.handle.take()) {
            let _ = h.join();
        }
    }
}

fn worker(items: Arc<Mutex<Vec<BatchItem>>>, stop: Arc<AtomicBool>, cancel: Arc<AtomicBool>, notify: Notify) {
    let lock = || items.lock().unwrap_or_else(|e| e.into_inner());
    loop {
        if stop.load(Ordering::Relaxed) {
            break;
        }
        let job = {
            let mut list = lock();
            list.iter_mut().find(|i| matches!(i.status, Status::Waiting)).map(|i| {
                i.status = Status::Running;
                i.fraction = 0.0;
                i.stage = "DVD wird geöffnet …".into();
                i.clone()
            })
        };
        let Some(job) = job else { break };
        notify();
        let id = job.id;
        let update = |f: &dyn Fn(&mut BatchItem)| {
            if let Some(i) = lock().iter_mut().find(|i| i.id == id) {
                f(i);
            }
            notify();
        };
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_item(&job, &cancel, &update)))
            .unwrap_or_else(|_| Err("Interner Fehler bei der Verarbeitung.".into()));
        let status = match result {
            Ok(s) => Status::Done(s),
            Err(e) if e == CANCELLED => Status::Cancelled,
            Err(e) => Status::Failed(e),
        };
        update(&|i| {
            i.status = status.clone();
            if matches!(status, Status::Done(_)) {
                i.fraction = 1.0;
            }
            i.stage.clear();
        });
        cancel.store(false, Ordering::Relaxed);
    }
    notify();
}

const CANCELLED: &str = "__abgebrochen__";

fn run_item(job: &BatchItem, cancel: &Arc<AtomicBool>, update: &dyn Fn(&dyn Fn(&mut BatchItem))) -> Res<BackupSummary> {
    // Öffnen und kurz analysieren; das zählt als die ersten 5 %.
    let source = Source::open(&job.source)?;
    let mut disc = Disc::open(source)?;
    if disc.check_scrambled() {
        return Err(COPY_PROTECTED.into());
    }
    let mut cb = |f: f32, t: &str| -> bool {
        let text = t.to_string();
        update(&|i| {
            i.fraction = f * 0.05;
            i.stage = text.clone();
        });
        !cancel.load(Ordering::Relaxed)
    };
    let scrambled = disc.analyse(false, &mut cb).map_err(|e| if cancel.load(Ordering::Relaxed) { CANCELLED.into() } else { e })?;
    if scrambled {
        return Err(COPY_PROTECTED.into());
    }
    let plan = plan_for(&job.plan, &disc)?;
    // Zielordner anlegen, falls es ihn noch nicht gibt
    let target = job.output_path();
    let dir = if matches!(job.output, Output::Folder(_)) { Some(target) } else { target.parent() };
    if let Some(dir) = dir.filter(|d| !d.as_os_str().is_empty()) {
        std::fs::create_dir_all(dir).map_err(|e| format!("Zielordner {}: {e}", dir.display()))?;
    }

    let (tx, rx) = channel();
    let req = BackupRequest { disc: Arc::new(disc), plan, output: job.output.clone(), deep_analysis: job.deep_analysis };
    let c2 = cancel.clone();
    let worker = std::thread::spawn(move || backup::run(req, tx, c2));
    let mut result = Err("Das Backup hat sich nicht zurückgemeldet.".to_string());
    for m in rx.iter() {
        match m {
            BackupMsg::Stage(s) => update(&|i| i.stage = s.clone()),
            BackupMsg::Progress { fraction, .. } => update(&|i| i.fraction = 0.05 + fraction * 0.95),
            BackupMsg::Done(s) => result = Ok(s),
            BackupMsg::Failed(e) => result = Err(e),
            BackupMsg::Cancelled => result = Err(CANCELLED.into()),
        }
    }
    let _ = worker.join();
    result
}

/// Plan für eine geöffnete DVD.
pub fn plan_for(src: &PlanSource, disc: &Disc) -> Res<Plan> {
    match src {
        PlanSource::Fixed { plan, fingerprint } => {
            if !fingerprint.matches(&Fingerprint::of(disc)) {
                return Err("Die DVD passt nicht mehr zu den gespeicherten Einstellungen (Titel oder Kapitel weichen ab).".into());
            }
            project::validate(plan, disc)?;
            Ok((**plan).clone())
        }
        PlanSource::Auto(a) => {
            let mut plan = Plan::new(a.target_sectors);
            plan.default_video = VideoMode::Auto;
            plan.remove_puo = a.remove_puo;
            plan.region_free = a.region_free;
            plan.split_vob = a.split_vob;
            if let Some(lang) = &a.only_lang {
                plan.keep_only_language(disc, lang);
            }
            Ok(plan)
        }
    }
}

const COPY_PROTECTED: &str = "Diese DVD ist kopiergeschützt (CSS) und wird nicht verarbeitet.";
