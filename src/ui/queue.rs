//! Warteschlange: mehrere DVDs, ISO-Abbilder und Projekte nacheinander sichern.

use std::path::{Path, PathBuf};

use eframe::egui::{self, Align, CornerRadius, Layout, Margin, RichText};
use egui_phosphor::regular as ic;

use discslim::dvd::source::Source;
use discslim::engine::backup::Output;
use discslim::engine::batch::{AutoSettings, BatchItem, PlanSource, Status};
use discslim::engine::plan::fmt_size;
use discslim::engine::project::Project;

use super::dialogs::{dirs_home, modal, progress_bar, sanitize, show_in_explorer, with_ext};
use super::style::{self, primary_button, segmented, semibold, soft_button};
use super::{target_sectors, App, OutputKind, TargetKind};

impl App {
    /// Zielordner für neu hinzugefügte DVDs.
    fn batch_dir(&self) -> PathBuf {
        if self.prefs.batch_dir.trim().is_empty() {
            dirs_home().join("Videos")
        } else {
            PathBuf::from(self.prefs.batch_dir.trim())
        }
    }

    /// Ein freier Ausgabename im Zielordner, der auch in der Warteschlange noch nicht vorkommt.
    fn unique_output(&self, label: &str) -> Output {
        let dir = self.batch_dir();
        let base = sanitize(label);
        let taken: Vec<PathBuf> = self.queue.items().iter().map(|i| i.output_path().to_path_buf()).collect();
        let folder = self.prefs.batch_kind == OutputKind::Folder;
        for n in 1.. {
            let name = if n == 1 { base.clone() } else { format!("{base} {n}") };
            let path = if folder { dir.join(&name) } else { with_ext(&dir.join(&name).display().to_string(), "iso") };
            let used = path.exists() || taken.iter().any(|t| t == &path);
            if !used {
                return if folder { Output::Folder(path) } else { Output::Iso(path) };
            }
        }
        unreachable!()
    }

    fn auto_settings(&self) -> AutoSettings {
        AutoSettings {
            target_sectors: target_sectors(&self.prefs),
            only_lang: self.prefs.only_lang.then(|| self.prefs.lang.clone()),
            remove_puo: self.prefs.remove_puo,
            region_free: self.prefs.region_free,
            split_vob: true,
        }
    }

    /// Nimmt DVD-Ordner, ISO-Dateien, Laufwerke und Projektdateien in die Warteschlange auf.
    pub(super) fn queue_add_paths(&mut self, paths: &[PathBuf]) {
        let mut errors = Vec::new();
        for path in paths {
            let added = if Project::is_project_path(path) {
                Project::load(path).map(|p| {
                    let output = self.unique_output(&p.label);
                    BatchItem::new(
                        p.source.clone(),
                        p.label.clone(),
                        PlanSource::Fixed { plan: Box::new(p.plan.to_plan()), fingerprint: p.fingerprint.clone() },
                        output,
                        self.prefs.deep_default,
                    )
                })
            } else {
                Source::open(path).map(|s| {
                    let output = self.unique_output(&s.label);
                    BatchItem::new(path.clone(), s.label.clone(), PlanSource::Auto(self.auto_settings()), output, self.prefs.deep_default)
                })
            };
            match added {
                Ok(item) => self.queue.push(item),
                Err(e) => errors.push(format!("{}: {e}", short_name(path))),
            }
        }
        if !errors.is_empty() {
            self.error = Some(format!("Nicht alles ließ sich hinzufügen:\n{}", errors.join("\n")));
        }
    }

    /// Kleine Anzeige in der Kopfzeile: Knopf zur Warteschlange mit Zähler bzw. Fortschritt.
    pub(super) fn queue_badge(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let (running, total, done, frac) = {
            let items = self.queue.items();
            let run = items.iter().find(|i| matches!(i.status, Status::Running)).map(|i| i.fraction);
            let done = items.iter().filter(|i| i.status.is_finished()).count();
            (run, items.len(), done, run.unwrap_or(0.0))
        };
        let tip = if total == 0 { "Warteschlange: mehrere DVDs nacheinander sichern".to_string() } else { format!("Warteschlange ({done} von {total} erledigt)") };
        if let Some(_) = running {
            ui.label(RichText::new(format!("{} / {}  ·  {:.0} %", done + 1, total, frac * 100.0)).size(12.5).color(p.accent));
        } else if total > 0 {
            ui.label(RichText::new(format!("{done}/{total}")).size(12.5).color(p.muted));
        }
        let icon = if running.is_some() { ic::SPINNER } else { ic::QUEUE };
        if style::icon_button(ui, &p, icon, &tip).clicked() {
            self.queue_open = true;
        }
    }

    pub(super) fn dialog_queue(&mut self, ctx: &egui::Context) {
        if !self.queue_open {
            return;
        }
        let p = self.pal;
        let running = self.queue.is_running();
        let items: Vec<BatchItem> = self.queue.items().clone();
        let mut close = false;
        let mut start = false;
        let mut cancel = false;
        let mut stop_after = false;
        let mut remove = None;
        let mut move_by = None;
        let mut reveal = None;
        let mut add_folders = false;
        let mut add_isos = false;
        let mut add_projects = false;
        let mut clear = false;
        let mut retry = false;

        modal(ctx, &p, "warteschlange", 700.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::QUEUE).size(24.0).color(p.accent));
                ui.label(RichText::new("Warteschlange").family(semibold()).size(20.0));
            });
            ui.label(
                RichText::new("Mehrere DVDs nacheinander sichern. Ziehe Ordner, ISO-Dateien oder Projekte einfach hierher.")
                    .size(13.0)
                    .color(p.muted),
            );
            ui.add_space(8.0);

            egui::ScrollArea::vertical().id_salt("stapel").max_height(330.0).auto_shrink([false, true]).show(ui, |ui| {
                ui.spacing_mut().item_spacing.y = 6.0;
                if items.is_empty() {
                    ui.add_space(20.0);
                    ui.vertical_centered(|ui| {
                        ui.label(RichText::new(ic::QUEUE).size(32.0).color(p.faint));
                        ui.label(RichText::new("Noch nichts in der Warteschlange.").color(p.muted));
                        ui.label(
                            RichText::new("Im Backup-Dialog kannst du die aktuelle DVD mit allen Einstellungen hinzufügen.")
                                .size(12.5)
                                .color(p.faint),
                        );
                    });
                    ui.add_space(20.0);
                }
                let n = items.len();
                for (k, it) in items.iter().enumerate() {
                    egui::Frame::new()
                        .fill(p.raised)
                        .corner_radius(CornerRadius::same(10))
                        .inner_margin(Margin::symmetric(12, 8))
                        .show(ui, |ui| {
                            ui.set_width(ui.available_width());
                            ui.horizontal(|ui| {
                                let (icon, color) = match &it.status {
                                    Status::Waiting => (ic::CLOCK, p.muted),
                                    Status::Running => (ic::SPINNER, p.accent),
                                    Status::Done(s) if !s.fits => (ic::WARNING, p.warn),
                                    Status::Done(_) => (ic::CHECK_CIRCLE, p.good),
                                    Status::Failed(_) => (ic::X_CIRCLE, p.bad),
                                    Status::Cancelled => (ic::PROHIBIT, p.muted),
                                };
                                ui.label(RichText::new(icon).size(22.0).color(color));
                                ui.vertical(|ui| {
                                    let how = match &it.plan {
                                        PlanSource::Auto(a) => {
                                            if a.target_sectors >= discslim::engine::plan::NO_TARGET {
                                                "Ganze DVD, ohne Größengrenze".to_string()
                                            } else {
                                                format!("Ganze DVD, automatisch auf {}", fmt_size(a.target_sectors as f64))
                                            }
                                        }
                                        PlanSource::Fixed { .. } => "Eigene Einstellungen".to_string(),
                                    };
                                    ui.label(RichText::new(format!("{}. {}", k + 1, it.label)).family(semibold()).size(14.0));
                                    ui.label(RichText::new(format!("{how}  →  {}", it.output_path().display())).size(12.0).color(p.muted));
                                    match &it.status {
                                        Status::Running => {
                                            ui.add_space(2.0);
                                            progress_bar(ui, &p, it.fraction);
                                            ui.label(RichText::new(format!("{:.0} %  ·  {}", it.fraction * 100.0, it.stage)).size(12.0).color(p.muted));
                                        }
                                        Status::Done(s) => {
                                            let mut t = format!("Fertig: {} in {:.0} s", fmt_size(s.sectors as f64), s.seconds);
                                            if !s.fits {
                                                t.push_str(" · größer als das Ziel");
                                            }
                                            ui.label(RichText::new(t).size(12.0).color(if s.fits { p.good } else { p.warn }));
                                        }
                                        Status::Failed(e) => {
                                            ui.label(RichText::new(e).size(12.0).color(p.bad));
                                        }
                                        Status::Cancelled => {
                                            ui.label(RichText::new("Abgebrochen").size(12.0).color(p.muted));
                                        }
                                        Status::Waiting => {}
                                    }
                                });
                                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                    let busy = matches!(it.status, Status::Running);
                                    if !busy && style::icon_button(ui, &p, ic::TRASH, "Aus der Warteschlange nehmen").clicked() {
                                        remove = Some(it.id);
                                    }
                                    if matches!(it.status, Status::Done(_)) && style::icon_button(ui, &p, ic::FOLDER_OPEN, "Im Explorer zeigen").clicked() {
                                        reveal = Some(it.output_path().to_path_buf());
                                    }
                                    if matches!(it.status, Status::Waiting) {
                                        if k + 1 < n && style::icon_button(ui, &p, ic::ARROW_DOWN, "Nach unten").clicked() {
                                            move_by = Some((it.id, false));
                                        }
                                        if k > 0 && style::icon_button(ui, &p, ic::ARROW_UP, "Nach oben").clicked() {
                                            move_by = Some((it.id, true));
                                        }
                                    }
                                });
                            });
                        });
                }
            });

            ui.add_space(8.0);
            ui.separator();
            ui.label(RichText::new("Hinzufügen").family(semibold()).size(13.0).color(p.muted));
            ui.horizontal(|ui| {
                if soft_button(ui, &p, &format!("{}  DVD-Ordner …", ic::FOLDER_PLUS)).clicked() {
                    add_folders = true;
                }
                if soft_button(ui, &p, &format!("{}  ISO-Dateien …", ic::FILE_PLUS)).clicked() {
                    add_isos = true;
                }
                if soft_button(ui, &p, &format!("{}  Projekte …", ic::FLOPPY_DISK)).clicked() {
                    add_projects = true;
                }
            });
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new("Ausgabe").color(p.muted));
                let opts = [(OutputKind::Iso, "ISO-Abbild"), (OutputKind::Folder, "Ordner")];
                segmented(ui, &p, &mut self.prefs.batch_kind, &opts, 220.0);
            });
            ui.horizontal(|ui| {
                ui.label(RichText::new("Zielordner").color(p.muted));
                if self.prefs.batch_dir.is_empty() {
                    self.prefs.batch_dir = self.batch_dir().display().to_string();
                }
                let edit = egui::TextEdit::singleline(&mut self.prefs.batch_dir)
                    .desired_width(ui.available_width() - 110.0)
                    .margin(Margin::symmetric(10, 6));
                ui.add(edit);
                if soft_button(ui, &p, &format!("{}  Wählen", ic::FOLDER_OPEN)).clicked() {
                    if let Some(f) = rfd::FileDialog::new().set_title("Zielordner für die Warteschlange").pick_folder() {
                        self.prefs.batch_dir = f.display().to_string();
                    }
                }
            });
            let target = match self.prefs.target_kind {
                TargetKind::Unlimited => "ohne Größengrenze".to_string(),
                _ => format!("automatisch auf {}", fmt_size(target_sectors(&self.prefs) as f64)),
            };
            let lang = if self.prefs.only_lang { ", nur die bevorzugte Sprache" } else { "" };
            ui.label(
                RichText::new(format!(
                    "{}  Neue DVDs werden ganz übernommen und {target} verkleinert{lang}. Projekte behalten ihre eigenen Einstellungen.",
                    ic::INFO
                ))
                .size(12.5)
                .color(p.faint),
            );

            ui.add_space(10.0);
            let waiting = items.iter().filter(|i| matches!(i.status, Status::Waiting)).count();
            let finished = items.iter().filter(|i| i.status.is_finished()).count();
            let failed = items.iter().filter(|i| matches!(i.status, Status::Failed(_) | Status::Cancelled)).count();
            ui.horizontal(|ui| {
                if finished > 0 && ui.add(egui::Button::new(RichText::new(format!("{}  Erledigte entfernen", ic::BROOM)).size(13.0))).clicked() {
                    clear = true;
                }
                if failed > 0 && ui.add(egui::Button::new(RichText::new(format!("{}  Erneut versuchen", ic::ARROW_COUNTER_CLOCKWISE)).size(13.0))).clicked() {
                    retry = true;
                }
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if running {
                        if primary_button(ui, &p, &format!("{}  Abbrechen", ic::STOP), true).clicked() {
                            cancel = true;
                        }
                        if soft_button(ui, &p, "Nach diesem anhalten")
                            .on_hover_text("Der laufende Eintrag wird fertig, danach hält die Warteschlange an.")
                            .clicked()
                        {
                            stop_after = true;
                        }
                    } else {
                        let label = if waiting == 1 { "1 DVD sichern".to_string() } else { format!("{waiting} DVDs sichern") };
                        if primary_button(ui, &p, &format!("{}  {label}", ic::PLAY), waiting > 0).clicked() {
                            start = true;
                        }
                    }
                    if soft_button(ui, &p, "Schließen").on_hover_text("Die Warteschlange läuft im Hintergrund weiter.").clicked() {
                        close = true;
                    }
                });
            });
        });

        if let Some(id) = remove {
            self.queue.remove(id);
        }
        if let Some((id, up)) = move_by {
            self.queue.move_by(id, up);
        }
        if let Some(path) = reveal {
            show_in_explorer(&path);
        }
        if clear {
            self.queue.clear_finished();
        }
        if retry {
            self.queue.retry_failed();
        }
        let dir = self.batch_dir();
        if add_folders {
            if let Some(list) = rfd::FileDialog::new().set_title("DVD-Ordner wählen (VIDEO_TS)").pick_folders() {
                self.queue_add_paths(&list);
            }
        }
        if add_isos {
            let d = rfd::FileDialog::new()
                .set_title("ISO-Abbilder wählen")
                .add_filter("DVD-Abbild", &["iso", "img"])
                .set_directory(&dir);
            if let Some(list) = d.pick_files() {
                self.queue_add_paths(&list);
            }
        }
        if add_projects {
            let d = rfd::FileDialog::new()
                .set_title("Projekte wählen")
                .add_filter("DVDmanager-Projekt", &[discslim::engine::project::EXTENSION]);
            if let Some(list) = d.pick_files() {
                self.queue_add_paths(&list);
            }
        }
        if start {
            let ctx2 = ctx.clone();
            self.queue.start(move || ctx2.request_repaint());
        }
        if cancel {
            self.queue.cancel();
        }
        if stop_after {
            self.queue.stop_after_current();
        }
        if close {
            self.queue_open = false;
        }
    }
}

fn short_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| p.display().to_string())
}
