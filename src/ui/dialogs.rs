//! Dialoge: Fehler, Ladefortschritt, Backup, Fortschritt, Ergebnis und Einstellungen.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Area, Color32, CornerRadius, Id, Layout, Margin, Order, RichText, Stroke, Ui, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::ifo::lang_name;
use discslim::dvd::model::{fmt_time, Category, Disc};
use discslim::engine::backup::{self, BackupRequest, Output};
use discslim::engine::batch::{BatchItem, PlanSource};
use discslim::engine::plan::{fmt_size, Mode, Plan};
use discslim::engine::project::Fingerprint;

use super::style::{self, primary_button, segmented, semibold, soft_button};
use super::{App, BackupDialog, Job, JobResult, OutputKind};

/// Modales Fenster mit abgedunkeltem Hintergrund.
pub(super) fn modal(ctx: &egui::Context, p: &style::Pal, id: &str, width: f32, add: impl FnOnce(&mut Ui)) {
    let frame = egui::Frame::new()
        .fill(p.card)
        .stroke(Stroke::new(1.0, p.line))
        .corner_radius(CornerRadius::same(16))
        .inner_margin(Margin::same(22))
        .shadow(ctx.global_style().visuals.window_shadow);
    let area = Area::new(Id::unique(id))
        .order(Order::Foreground)
        .anchor(egui::Align2::CENTER_CENTER, Vec2::ZERO)
        .movable(false)
        .constrain(true);
    egui::Modal::new(Id::unique(id))
        .backdrop_color(Color32::from_black_alpha(150))
        .area(area)
        .frame(frame)
        .show(ctx, |ui| {
            ui.set_width(width);
            add(ui);
        });
}

fn free_space(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDiskFreeSpaceExW(dir: *const u16, free: *mut u64, total: *mut u64, total_free: *mut u64) -> i32;
        }
        let mut probe = path.to_path_buf();
        while !probe.exists() {
            probe = probe.parent()?.to_path_buf();
        }
        let wide: Vec<u16> =
            probe.as_os_str().to_string_lossy().encode_utf16().chain(std::iter::once(0)).collect();
        let (mut free, mut total, mut tf) = (0u64, 0u64, 0u64);
        let ok = unsafe { GetDiskFreeSpaceExW(wide.as_ptr(), &mut free, &mut total, &mut tf) };
        if ok != 0 {
            return Some(free);
        }
        None
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

fn fmt_duration(secs: f64) -> String {
    let s = secs.max(0.0) as u64;
    if s >= 3600 {
        format!("{} Std. {} Min.", s / 3600, (s / 60) % 60)
    } else if s >= 60 {
        format!("{} Min. {} Sek.", s / 60, s % 60)
    } else {
        format!("{s} Sek.")
    }
}

pub(super) fn progress_bar(ui: &mut Ui, p: &style::Pal, f: f32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 10.0), egui::Sense::hover());
    ui.painter().rect_filled(rect, CornerRadius::same(5), p.track);
    let fill = egui::Rect::from_min_size(rect.min, Vec2::new(rect.width() * f.clamp(0.0, 1.0), rect.height()));
    ui.painter().rect_filled(fill, CornerRadius::same(5), p.accent);
}

impl App {
    pub(super) fn open_backup_dialog(&mut self) {
        // Zuletzt gewählte Orte wieder anbieten
        let videos = dirs_home().join("Videos");
        let pick = |s: &str| if s.is_empty() || !Path::new(s).exists() { videos.clone() } else { PathBuf::from(s) };
        let folder_base = pick(&self.prefs.folder_dir);
        let file_base = pick(&self.prefs.file_dir);
        let label = self.disc.as_ref().map(|d| sanitize(&d.source.label)).unwrap_or_else(|| "DVD".into());
        // Für die VOB-Datei: ohne Auswahl der Hauptfilm, beim Neu-Zusammenstellen die ganze Liste.
        let vob_sel: Vec<bool> = match (self.plan.mode, self.disc.as_ref()) {
            (Mode::Reauthor, _) => vec![true; self.plan.reauthor.len()],
            (Mode::FullDisc, Some(d)) => d.titles.iter().map(|t| t.category == Category::MainMovie).collect(),
            (Mode::FullDisc, None) => Vec::new(),
        };
        self.backup_dlg = Some(BackupDialog {
            kind: self.prefs.backup_kind,
            folder: folder_base.join(&label).display().to_string(),
            iso: file_base.join(format!("{label}.iso")).display().to_string(),
            vob: file_base.join(format!("{label}.vob")).display().to_string(),
            mpg: file_base.join(format!("{label}.mpg")).display().to_string(),
            mpeg_audio: None,
            no_split: self.prefs.no_split,
            vob_sel,
            vob_keep_target: self.prefs.vob_keep_target,
            deep: self.prefs.deep_default,
            overwrite: None,
        });
    }

    pub(super) fn view_dialogs(&mut self, ctx: &egui::Context) {
        let p = self.pal;

        if self.loader.is_some() {
            let mut cancel = false;
            let (progress, text) = {
                let l = self.loader.as_ref().unwrap();
                (l.progress, l.text.clone())
            };
            modal(ctx, &p, "laden", 420.0, |ui| {
                ui.label(RichText::new(text).family(semibold()).size(16.0));
                ui.add_space(8.0);
                progress_bar(ui, &p, progress);
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if soft_button(ui, &p, "Abbrechen").clicked() {
                        cancel = true;
                    }
                });
            });
            if cancel {
                if let Some(l) = &self.loader {
                    l.cancel.store(true, Ordering::Relaxed);
                }
            }
        }

        if self.error.is_some() {
            let msg = self.error.clone().unwrap_or_default();
            let mut close = false;
            modal(ctx, &p, "fehler", 480.0, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::WARNING_CIRCLE).size(26.0).color(p.warn));
                    ui.label(RichText::new("Das hat nicht geklappt").family(semibold()).size(17.0));
                });
                ui.add_space(6.0);
                ui.label(RichText::new(msg).size(14.0).color(p.muted));
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if primary_button(ui, &p, "OK", true).clicked() {
                        close = true;
                    }
                });
            });
            if close {
                self.error = None;
            }
        }

        self.dialog_backup(ctx);
        self.dialog_queue(ctx);
        self.dialog_job(ctx);
        self.dialog_result(ctx);
        self.dialog_settings(ctx);
        self.dialog_about(ctx);
    }

    fn dialog_backup(&mut self, ctx: &egui::Context) {
        if self.backup_dlg.is_none() {
            return;
        }
        let p = self.pal;
        let Some(disc) = self.disc.clone() else { return };
        let mut start = false;
        let mut enqueue = false;
        let mut close = false;
        let dlg = self.backup_dlg.as_mut().unwrap();
        modal(ctx, &p, "backup", 560.0, |ui| {
            ui.label(RichText::new("Backup erstellen").family(semibold()).size(20.0));
            ui.add_space(4.0);
            let width = ui.available_width();
            let opts = [
                (OutputKind::Iso, "ISO-Abbild"),
                (OutputKind::Folder, "Ordner"),
                (OutputKind::Vob, "VOB-Datei"),
                (OutputKind::Mpeg, "MPEG-2"),
                (OutputKind::Burn, "Brennen"),
            ];
            segmented(ui, &p, &mut dlg.kind, &opts, width);
            ui.add_space(6.0);
            let hint = match dlg.kind {
                OutputKind::Iso => "Eine einzige Datei, die du später brennen oder einbinden kannst.",
                OutputKind::Folder => "Legt die Ordner VIDEO_TS und AUDIO_TS an.",
                OutputKind::Burn => "Legt ein ISO-Abbild an und öffnet danach den Windows-Brenner.",
                OutputKind::Vob => "Schreibt die gewählten Titel nacheinander in eine einzige VOB-Datei, ohne DVD-Struktur und Menüs.",
                OutputKind::Mpeg => "Schreibt die gewählten Titel als schlichte MPEG-2-Datei: nur Bild und eine Tonspur, ohne DVD-Navigation und Untertitel.",
            };
            ui.label(RichText::new(hint).size(13.0).color(p.muted));
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                let is_folder = dlg.kind == OutputKind::Folder;
                let is_vob = dlg.kind == OutputKind::Vob;
                let is_mpeg = dlg.kind == OutputKind::Mpeg;
                let value = match dlg.kind {
                    OutputKind::Folder => &mut dlg.folder,
                    OutputKind::Vob => &mut dlg.vob,
                    OutputKind::Mpeg => &mut dlg.mpg,
                    _ => &mut dlg.iso,
                };
                let edit = egui::TextEdit::singleline(value)
                    .desired_width(ui.available_width() - 120.0)
                    .margin(Margin::symmetric(10, 8));
                ui.add(edit);
                if soft_button(ui, &p, &format!("{}  Wählen", ic::FOLDER_OPEN)).clicked() {
                    if is_folder {
                        if let Some(f) = rfd::FileDialog::new().set_title("Zielordner wählen").pick_folder() {
                            *value = f.display().to_string();
                        }
                    } else {
                        let mut d = if is_mpeg {
                            rfd::FileDialog::new().set_title("MPEG-2-Datei speichern").add_filter("MPEG-2", &["mpg", "mpeg"])
                        } else if is_vob {
                            rfd::FileDialog::new().set_title("VOB-Datei speichern").add_filter("VOB-Datei", &["vob"])
                        } else {
                            rfd::FileDialog::new().set_title("ISO-Abbild speichern").add_filter("DVD-Abbild", &["iso"])
                        };
                        let cur = PathBuf::from(value.as_str());
                        if let Some(n) = cur.file_name() {
                            d = d.set_file_name(n.to_string_lossy());
                        }
                        if let Some(parent) = cur.parent().filter(|x| x.exists()) {
                            d = d.set_directory(parent);
                        }
                        if let Some(f) = d.save_file() {
                            *value = f.display().to_string();
                        }
                    }
                }
            });

            if matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg) {
                ui.add_space(8.0);
                ui.label(RichText::new("Titel für die Datei").family(semibold()).size(13.0).color(p.muted));
                egui::ScrollArea::vertical().id_salt("vob-titel").max_height(132.0).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    let rows = vob_rows(&disc, &self.plan);
                    dlg.vob_sel.resize(rows.len(), false);
                    for (on, text) in dlg.vob_sel.iter_mut().zip(rows.iter()) {
                        style::check(ui, &p, on, text);
                    }
                    if rows.is_empty() {
                        ui.label(RichText::new("Die Liste „Neue DVD“ ist noch leer.").size(13.0).color(p.muted));
                    }
                });
            }

            ui.add_space(8.0);
            ui.label(RichText::new("Optionen").family(semibold()).size(13.0).color(p.muted));
            let full_done = disc.analysed_full;
            let mut deep = dlg.deep || full_done;
            ui.add_enabled_ui(!full_done, |ui| {
                style::check(ui, &p, &mut deep, "Vorher Tiefenanalyse ausführen (genauer, dauert länger)");
            });
            if !full_done {
                dlg.deep = deep;
            }
            if dlg.kind == OutputKind::Mpeg {
                // Tonspur wählen (aus dem ersten gewählten Titel)
                let tracks = first_selected_tracks(&disc, &self.plan, &dlg.vob_sel);
                if dlg.mpeg_audio.is_none_or(|ph| !tracks.iter().any(|t| t.0 == ph)) {
                    dlg.mpeg_audio = tracks.first().map(|t| t.0);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Tonspur").color(p.muted));
                    let current = tracks.iter().find(|t| Some(t.0) == dlg.mpeg_audio).map(|t| t.1.clone()).unwrap_or_else(|| "keine".into());
                    egui::ComboBox::from_id_salt("mpeg-ton").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        for (ph, name) in &tracks {
                            ui.selectable_value(&mut dlg.mpeg_audio, Some(*ph), name);
                        }
                    });
                });
            }
            match dlg.kind {
                OutputKind::Vob | OutputKind::Mpeg => {
                    style::check(ui, &p, &mut dlg.vob_keep_target, "Automatik hält auch hier die Zielgröße ein");
                }
                _ => {
                    style::check(ui, &p, &mut self.plan.remove_puo, "Sperren für Vorspulen und Menütasten aufheben");
                    if self.plan.mode == Mode::FullDisc {
                        style::check(ui, &p, &mut self.plan.region_free, "Ländercode der DVD entfernen");
                    }
                }
            }
            if matches!(dlg.kind, OutputKind::Iso | OutputKind::Folder) {
                style::check(ui, &p, &mut dlg.no_split, "VOB-Dateien in voller Länge, nicht in 1-GB-Teile zerlegen");
                if dlg.no_split {
                    ui.label(
                        RichText::new(format!(
                            "{}  Gut zum Weiterverarbeiten. DVD-Player und Brennprogramme erwarten aber die geteilten Dateien.",
                            ic::INFO
                        ))
                        .size(12.5)
                        .color(p.warn),
                    );
                }
            }

            ui.add_space(8.0);
            let est = if matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg) {
                self.plan.for_single_file(&disc, &dlg.vob_sel, dlg.vob_keep_target).estimate(&disc)
            } else {
                self.plan.estimate(&disc)
            };
            let needed = (est.total * 2048.0) as u64;
            let need_txt = fmt_size(est.total);
            ui.label(RichText::new(format!("Voraussichtliche Größe: {need_txt}")).size(14.0));
            let target_path = PathBuf::from(match dlg.kind {
                OutputKind::Folder => &dlg.folder,
                OutputKind::Vob => &dlg.vob,
                OutputKind::Mpeg => &dlg.mpg,
                _ => &dlg.iso,
            });
            // ISO entsteht über einen Arbeitsordner und braucht vorübergehend doppelt so viel Platz.
            let is_file = dlg.kind != OutputKind::Folder;
            let required = if matches!(dlg.kind, OutputKind::Iso | OutputKind::Burn) { needed * 2 } else { needed };
            let mut blocked = false;
            if let Some(free) = free_space(&target_path) {
                if free < required {
                    blocked = true;
                    ui.label(
                        RichText::new(format!(
                            "{}  Auf dem Ziel sind nur {} frei, nötig sind etwa {}.",
                            ic::WARNING,
                            fmt_size(free as f64 / 2048.0),
                            fmt_size(required as f64 / 2048.0)
                        ))
                        .color(p.bad),
                    );
                }
            }
            if !est.fits {
                ui.label(RichText::new(format!("{}  Das Ergebnis wird größer als das Ziel.", ic::WARNING)).color(p.warn));
            }
            let _ = is_file;
            let path_ok = !target_path.as_os_str().is_empty()
                && (!matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg) || dlg.vob_sel.iter().any(|on| *on));

            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                let ok = path_ok && !blocked;
                if primary_button(ui, &p, &format!("{}  Backup starten", ic::PLAY), ok).clicked() {
                    start = true;
                }
                // Die Warteschlange prüft den Platz erst, wenn sie den Eintrag abarbeitet.
                let q = ui
                    .add_enabled(path_ok, egui::Button::new(RichText::new(format!("{}  Zur Warteschlange", ic::QUEUE)).size(14.0).color(p.text))
                        .fill(p.raised)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(9))
                        .min_size(Vec2::new(0.0, 36.0)))
                    .on_hover_text("Mit diesen Einstellungen später zusammen mit anderen DVDs sichern");
                if q.clicked() {
                    enqueue = true;
                }
                if soft_button(ui, &p, "Abbrechen").clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.backup_dlg = None;
        }
        if enqueue {
            self.enqueue_from_dialog();
            return;
        }
        if start {
            // Vor dem Start nachsehen, ob gleichnamige Dateien schon da sind.
            let found = self.backup_dlg.as_ref().map(existing_files).unwrap_or_default();
            if found.is_empty() {
                self.start_job(ctx);
            } else if let Some(d) = self.backup_dlg.as_mut() {
                d.overwrite = Some(found);
            }
        }
        self.dialog_overwrite(ctx);
    }

    /// Rückfrage, bevor vorhandene Dateien ersetzt werden.
    fn dialog_overwrite(&mut self, ctx: &egui::Context) {
        let Some(found) = self.backup_dlg.as_ref().and_then(|d| d.overwrite.clone()) else { return };
        let p = self.pal;
        let (mut yes, mut no) = (false, false);
        modal(ctx, &p, "ueberschreiben", 480.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::WARNING).size(26.0).color(p.warn));
                ui.label(RichText::new("Dateien überschreiben?").family(semibold()).size(18.0));
            });
            ui.add_space(4.0);
            let text = if found.len() == 1 {
                "Diese Datei gibt es am Ziel schon:".to_string()
            } else {
                format!("Diese {} Dateien gibt es am Ziel schon:", found.len())
            };
            ui.label(RichText::new(text).color(p.muted));
            egui::ScrollArea::vertical().id_salt("vorhanden").max_height(150.0).show(ui, |ui| {
                for name in found.iter().take(60) {
                    ui.label(RichText::new(name).size(13.0));
                }
                if found.len() > 60 {
                    ui.label(RichText::new(format!("… und {} weitere", found.len() - 60)).size(13.0).color(p.muted));
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new("Beim Überschreiben gehen sie verloren.").size(13.0).color(p.muted));
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, "Überschreiben", true).clicked() {
                    yes = true;
                }
                if soft_button(ui, &p, "Abbrechen").clicked() {
                    no = true;
                }
            });
        });
        if yes {
            self.start_job(ctx);
        } else if no {
            if let Some(d) = self.backup_dlg.as_mut() {
                d.overwrite = None;
            }
        }
    }

    pub(super) fn start_job(&mut self, ctx: &egui::Context) {
        let Some(dlg) = self.backup_dlg.take() else { return };
        let Some(disc) = self.disc.clone() else { return };
        let (output, plan) = self.request_from_dialog(&dlg, &disc);
        self.preview.pause();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let req = BackupRequest {
            disc,
            plan,
            output,
            deep_analysis: dlg.deep,
        };
        let cancel2 = cancel.clone();
        let ctx2 = ctx.clone();
        std::thread::spawn(move || {
            backup::run(req, tx, cancel2);
            ctx2.request_repaint();
        });
        self.job = Some(Job {
            rx,
            cancel,
            started: Instant::now(),
            stage: "Vorbereiten".into(),
            current: String::new(),
            fraction: 0.0,
            sectors_out: 0,
            ratio: 1.0,
        });
    }

    /// Stellt den Backup-Auftrag aus dem Dialog in die Warteschlange.
    fn enqueue_from_dialog(&mut self) {
        let Some(dlg) = self.backup_dlg.take() else { return };
        let Some(disc) = self.disc.clone() else { return };
        let (output, plan) = self.request_from_dialog(&dlg, &disc);
        let item = BatchItem::new(
            disc.source.origin.clone(),
            disc.source.label.clone(),
            PlanSource::Fixed { plan: Box::new(plan), fingerprint: Fingerprint::of(&disc) },
            output,
            dlg.deep,
        );
        self.queue.push(item);
        let n = self.queue.items().len();
        self.notice = Some((format!("In die Warteschlange gestellt ({n} Einträge)"), Instant::now()));
    }

    /// Ziel und Plan zum Backup-Dialog; merkt sich die Optionen fürs nächste Mal.
    fn request_from_dialog(&mut self, dlg: &BackupDialog, disc: &Disc) -> (Output, Plan) {
        // Ziel und Optionen fürs nächste Mal merken
        self.prefs.backup_kind = dlg.kind;
        self.prefs.no_split = dlg.no_split;
        self.prefs.vob_keep_target = dlg.vob_keep_target;
        self.prefs.remove_puo = self.plan.remove_puo;
        self.prefs.region_free = self.plan.region_free;
        let parent_of = |s: &str| PathBuf::from(s).parent().map(|p| p.display().to_string()).unwrap_or_default();
        match dlg.kind {
            OutputKind::Folder => self.prefs.folder_dir = parent_of(&dlg.folder),
            OutputKind::Vob => self.prefs.file_dir = parent_of(&dlg.vob),
            OutputKind::Mpeg => self.prefs.file_dir = parent_of(&dlg.mpg),
            _ => self.prefs.file_dir = parent_of(&dlg.iso),
        }
        let output = match dlg.kind {
            OutputKind::Folder => Output::Folder(PathBuf::from(&dlg.folder)),
            OutputKind::Iso => Output::Iso(with_iso_ext(&dlg.iso)),
            OutputKind::Burn => Output::Burn(with_iso_ext(&dlg.iso)),
            OutputKind::Vob => Output::Vob(with_ext(&dlg.vob, "vob")),
            OutputKind::Mpeg => Output::Mpeg { path: with_ext(&dlg.mpg, "mpg"), audio: dlg.mpeg_audio },
        };
        let plan = if matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg) {
            self.plan.for_single_file(disc, &dlg.vob_sel, dlg.vob_keep_target)
        } else {
            let mut plan = self.plan.clone();
            // Eine gebrannte DVD muss immer die übliche Teilung haben.
            plan.split_vob = !dlg.no_split || dlg.kind == OutputKind::Burn;
            plan
        };
        (output, plan)
    }

    fn dialog_job(&mut self, ctx: &egui::Context) {
        let p = self.pal;
        let Some(j) = self.job.as_ref() else { return };
        let mut cancel = false;
        let (fraction, stage, sectors, ratio, elapsed) =
            (j.fraction, j.stage.clone(), j.sectors_out, j.ratio, j.started.elapsed().as_secs_f64());
        modal(ctx, &p, "job", 500.0, |ui| {
            ui.label(RichText::new("Backup läuft").family(semibold()).size(20.0));
            ui.add_space(2.0);
            ui.label(RichText::new(stage).size(14.0).color(p.muted));
            ui.add_space(8.0);
            progress_bar(ui, &p, fraction);
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("{:.0} %", fraction * 100.0)).family(semibold()).size(18.0));
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if fraction > 0.02 {
                        let remaining = elapsed * f64::from(1.0 - fraction) / f64::from(fraction);
                        ui.label(RichText::new(format!("noch etwa {}", fmt_duration(remaining))).color(p.muted));
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(format!(
                    "Bisher geschrieben: {}  ·  Bild behält {:.0} %  ·  Vergangen: {}",
                    fmt_size(sectors as f64),
                    ratio * 100.0,
                    fmt_duration(elapsed)
                ))
                .size(13.0)
                .color(p.muted),
            );
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if soft_button(ui, &p, "Abbrechen").clicked() {
                    cancel = true;
                }
            });
        });
        if cancel {
            if let Some(j) = self.job.as_ref() {
                j.cancel.store(true, Ordering::Relaxed);
            }
        }
    }

    fn dialog_result(&mut self, ctx: &egui::Context) {
        let p = self.pal;
        let Some(res) = self.result.as_ref() else { return };
        let mut close = false;
        let mut open_at: Option<PathBuf> = None;
        modal(ctx, &p, "ergebnis", 500.0, |ui| match res {
            JobResult::Ok(s) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::CHECK_CIRCLE).size(28.0).color(p.good));
                    ui.label(RichText::new("Backup fertig").family(semibold()).size(20.0));
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!(
                        "{} geschrieben von ursprünglich {}. Dauer: {}",
                        fmt_size(s.sectors as f64),
                        fmt_size(s.source_sectors),
                        fmt_duration(s.seconds)
                    ))
                    .size(14.0),
                );
                ui.label(RichText::new(s.path.display().to_string()).size(13.0).color(p.muted));
                if !s.fits {
                    ui.label(
                        RichText::new(format!(
                            "{}  Das Ergebnis ist größer als das Ziel von {}.",
                            ic::WARNING,
                            fmt_size(s.target_sectors as f64)
                        ))
                        .color(p.warn),
                    );
                }
                if s.burn_started {
                    ui.label(RichText::new("Der Windows-Brenner wurde geöffnet.").color(p.muted));
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if primary_button(ui, &p, "Schließen", true).clicked() {
                        close = true;
                    }
                    if soft_button(ui, &p, &format!("{}  Im Explorer zeigen", ic::FOLDER_OPEN)).clicked() {
                        open_at = Some(s.path.clone());
                    }
                });
            }
            JobResult::Failed(e) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::X_CIRCLE).size(28.0).color(p.bad));
                    ui.label(RichText::new("Backup fehlgeschlagen").family(semibold()).size(20.0));
                });
                ui.add_space(4.0);
                ui.label(RichText::new(e).color(p.muted));
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if primary_button(ui, &p, "OK", true).clicked() {
                        close = true;
                    }
                });
            }
            JobResult::Cancelled => {
                ui.label(RichText::new("Backup abgebrochen").family(semibold()).size(20.0));
                ui.label(RichText::new("Angefangene Dateien wurden entfernt.").color(p.muted));
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if primary_button(ui, &p, "OK", true).clicked() {
                        close = true;
                    }
                });
            }
        });
        if let Some(path) = open_at {
            show_in_explorer(&path);
        }
        if close {
            self.result = None;
        }
    }

    fn dialog_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let p = self.pal;
        let mut close = false;
        let mut dark = p.dark;
        modal(ctx, &p, "einstellungen", 480.0, |ui| {
            ui.label(RichText::new("Einstellungen").family(semibold()).size(20.0));
            ui.add_space(4.0);
            ui.label(RichText::new("Design").family(semibold()).size(13.0).color(p.muted));
            let width = ui.available_width();
            let opts = [(true, "Dunkel"), (false, "Hell")];
            segmented(ui, &p, &mut dark, &opts, width);

            ui.add_space(6.0);
            ui.label(RichText::new("Sprachen").family(semibold()).size(13.0).color(p.muted));
            style::check(ui, &p, &mut self.prefs.only_lang, "Beim Öffnen nur diese Sprache behalten");
            ui.add_enabled_ui(self.prefs.only_lang, |ui| {
                egui::ComboBox::from_id_salt("sprache").selected_text(lang_name(&self.prefs.lang)).show_ui(ui, |ui| {
                    for code in ["de", "en", "fr", "es", "it", "nl", "pl", "tr", "ru"] {
                        ui.selectable_value(&mut self.prefs.lang, code.to_string(), lang_name(code));
                    }
                });
            });
            ui.label(
                RichText::new("Gibt es die Sprache auf der DVD nicht, bleiben alle Spuren ausgewählt.")
                    .size(12.5)
                    .color(p.faint),
            );

            ui.add_space(6.0);
            ui.label(RichText::new("Backup").family(semibold()).size(13.0).color(p.muted));
            style::check(ui, &p, &mut self.prefs.deep_default, "Tiefenanalyse vor dem Backup vorschlagen");

            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, "Fertig", true).clicked() {
                    close = true;
                }
            });
        });
        if dark != p.dark {
            self.set_dark(ctx, dark);
        }
        if close {
            self.settings_open = false;
        }
    }

    fn dialog_about(&mut self, ctx: &egui::Context) {
        if !self.about_open {
            return;
        }
        let p = self.pal;
        let mut close = false;
        modal(ctx, &p, "ueber", 480.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::DISC).size(30.0).color(p.accent));
                ui.label(RichText::new("DVDmanager").family(semibold()).size(20.0));
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    "Verkleinert eigene, ungeschützte Video-DVDs, damit sie auf einen Rohling passen. Das Bild wird dabei neu quantisiert, nicht komplett neu kodiert. Das geht schnell, und Struktur und Menüs bleiben erhalten.",
                )
                .color(p.muted),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new("Kopiergeschützte DVDs (CSS) werden erkannt und nicht verarbeitet.").size(13.0).color(p.faint),
            );
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, "Schließen", true).clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.about_open = false;
        }
    }
}

pub(super) fn dirs_home() -> PathBuf {
    std::env::var_os("USERPROFILE").map(PathBuf::from).unwrap_or_else(std::env::temp_dir)
}

/// Dateiname aus der Bezeichnung einer DVD (ohne Zusatz).
pub(super) fn file_stem(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_alphanumeric() || c == '-' || c == '_' || c == ' ' { c } else { '_' })
        .collect();
    let s = s.trim().to_string();
    if s.is_empty() { "DVD".into() } else { s }
}

/// Name für das Ergebnis eines Backups.
pub(super) fn sanitize(label: &str) -> String {
    let s = file_stem(label);
    if s == "DVD" && label.trim().is_empty() {
        s
    } else {
        format!("{s} (klein)")
    }
}

/// Dateien, die das Backup am gewählten Ziel ersetzen würde.
fn existing_files(d: &BackupDialog) -> Vec<String> {
    match d.kind {
        OutputKind::Folder => {
            let dir = PathBuf::from(&d.folder).join("VIDEO_TS");
            let mut out: Vec<String> = std::fs::read_dir(&dir)
                .map(|rd| {
                    rd.flatten()
                        .map(|e| e.file_name().to_string_lossy().to_string())
                        .filter(|n| {
                            let u = n.to_ascii_uppercase();
                            (u.starts_with("VTS_") || u.starts_with("VIDEO_TS"))
                                && (u.ends_with(".IFO") || u.ends_with(".BUP") || u.ends_with(".VOB"))
                        })
                        .map(|n| format!("VIDEO_TS\\{n}"))
                        .collect()
                })
                .unwrap_or_default();
            out.sort();
            out
        }
        OutputKind::Mpeg => {
            let p = with_ext(&d.mpg, "mpg");
            if p.exists() { vec![p.display().to_string()] } else { Vec::new() }
        }
        OutputKind::Vob => {
            let p = with_ext(&d.vob, "vob");
            if p.exists() { vec![p.display().to_string()] } else { Vec::new() }
        }
        OutputKind::Iso | OutputKind::Burn => {
            let p = with_iso_ext(&d.iso);
            if p.exists() { vec![p.display().to_string()] } else { Vec::new() }
        }
    }
}

/// Tonspuren des ersten Titels, der in die Datei kommt: (physische Nummer, Bezeichnung).
fn first_selected_tracks(disc: &Disc, plan: &Plan, sel: &[bool]) -> Vec<(u8, String)> {
    let first = sel.iter().position(|on| *on).unwrap_or(0);
    let title = match plan.mode {
        Mode::FullDisc => Some(first),
        Mode::Reauthor => plan.reauthor.get(first).map(|e| e.title),
    };
    let Some(t) = title.and_then(|i| disc.titles.get(i)) else { return Vec::new() };
    t.audio
        .iter()
        .map(|a| (a.phys, format!("{} · {} {}", lang_name(&a.attr.lang), a.attr.codec_name(), a.attr.channels_name())))
        .collect()
}

/// Öffnet den Explorer am Ergebnis: Ordner direkt, Dateien markiert in ihrem Ordner.
/// Der Pfad muss als `/select,"…"` übergeben werden; normale Argument-Anführungszeichen
/// versteht der Explorer nicht und öffnet sonst einen falschen Ordner.
pub(super) fn show_in_explorer(path: &Path) {
    let path = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let mut text = path.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\") {
        text = rest.to_string();
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let arg = if path.is_dir() { format!("\"{text}\"") } else { format!("/select,\"{text}\"") };
        let _ = std::process::Command::new("explorer.exe").raw_arg(arg).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path.parent().unwrap_or(&path)).spawn();
    }
}

fn with_iso_ext(s: &str) -> PathBuf {
    with_ext(s, "iso")
}

pub(super) fn with_ext(s: &str, ext: &str) -> PathBuf {
    let p = PathBuf::from(s);
    if p.extension().map(|e| e.eq_ignore_ascii_case(ext)).unwrap_or(false) {
        p
    } else {
        PathBuf::from(format!("{s}.{ext}"))
    }
}

/// Zeilen für die Titelauswahl der VOB-Ausgabe.
fn vob_rows(disc: &Disc, plan: &Plan) -> Vec<String> {
    match plan.mode {
        Mode::FullDisc => disc
            .titles
            .iter()
            .map(|t| {
                let kind = if t.category == Category::MainMovie { "Hauptfilm" } else { "Extra" };
                format!("{kind} · Titel {} · {}", t.number, fmt_time(t.ms))
            })
            .collect(),
        Mode::Reauthor => plan
            .reauthor
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let t = disc.titles.get(e.title)?;
                Some(format!("{}. Titel {} · Kapitel {} bis {}", i + 1, t.number, e.first_chapter + 1, e.last_chapter + 1))
            })
            .collect(),
    }
}
