//! Dialoge: Fehler, Ladefortschritt, Backup, Fortschritt, Ergebnis und Einstellungen.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;
use std::time::Instant;

use eframe::egui::{self, Align, Area, Color32, CornerRadius, Id, Layout, Margin, Order, RichText, Stroke, Ui, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::ifo::lang_name;
use discslim::i18n::Lang;
use discslim::dvd::model::{fmt_time, Category, Disc};
use discslim::engine::backup::{self, BackupRequest, Output};
use discslim::engine::batch::{BatchItem, PlanSource};
use discslim::engine::plan::{fmt_size, Mode, Plan};
use discslim::engine::project::Fingerprint;

use super::style::{self, primary_button, segmented, semibold, soft_button};
use super::{App, BackupDialog, ExtStyle, Job, JobResult, OutputKind};

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
        trf!("{} Std. {} Min.", "{} h {} min", s / 3600, (s / 60) % 60)
    } else if s >= 60 {
        trf!("{} Min. {} Sek.", "{} min {} s", s / 60, s % 60)
    } else {
        trf!("{s} Sek.", "{s} s")
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
        // Zuletzt gewählten Ort wieder anbieten (sonst das Hauptverzeichnis zum Speichern)
        let base = self.save_start_dir();
        let folder_base = base.clone();
        let file_base = base;
        let label = self.disc.as_ref().map(|d| sanitize(&d.source.label)).unwrap_or_else(|| "DVD".into());
        // Für die VOB-Datei: ohne Auswahl der Hauptfilm, beim Neu-Zusammenstellen die ganze Liste.
        let vob_sel: Vec<bool> = match (self.plan.mode, self.disc.as_ref()) {
            (Mode::Reauthor, _) => vec![true; self.plan.reauthor.len()],
            (Mode::FullDisc, Some(d)) => d.titles.iter().map(|t| t.category == Category::MainMovie).collect(),
            (Mode::FullDisc, None) => Vec::new(),
        };
        // Namen der Titel: eigener Name des Eintrags oder die übliche Bezeichnung
        let names: Vec<String> = match self.disc.as_ref() {
            Some(d) => {
                let rows = vob_rows(d, &self.plan);
                match self.plan.mode {
                    Mode::Reauthor => rows
                        .into_iter()
                        .enumerate()
                        .map(|(i, r)| self.plan.reauthor.get(i).and_then(|e| e.name.clone()).filter(|n| !n.trim().is_empty()).unwrap_or(r))
                        .collect(),
                    Mode::FullDisc => rows,
                }
            }
            None => Vec::new(),
        };
        // „Brennen“ ist jetzt eine Option des ISO-Abbilds
        let burn = self.prefs.backup_kind == OutputKind::Burn;
        let kind = if burn { OutputKind::Iso } else { self.prefs.backup_kind };
        // Zusammen geöffnete Videodateien: gleich wieder je Titel eine eigene Datei
        let many_files = self.disc.as_ref().is_some_and(|d| d.source.origins.len() > 1);
        self.backup_dlg = Some(BackupDialog {
            kind,
            burn,
            separate: many_files,
            suffix_on: self.prefs.file_suffix_on,
            suffix: self.prefs.file_suffix.clone(),
            separate_dir: file_base.display().to_string(),
            names,
            folder: folder_base.join(&label).display().to_string(),
            iso: file_base.join(format!("{label}.iso")).display().to_string(),
            vob: file_base.join(format!("{label}.{}", file_ext(OutputKind::Vob, &self.prefs.ext))).display().to_string(),
            mpg: file_base.join(format!("{label}.{}", file_ext(OutputKind::Mpeg, &self.prefs.ext))).display().to_string(),
            mkv: file_base.join(format!("{label}.{}", file_ext(OutputKind::Mkv, &self.prefs.ext))).display().to_string(),
            ext: self.prefs.ext,
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
            let mut toggle_live = false;
            let (progress, text, live_on, live_tex) = {
                let l = self.loader.as_ref().unwrap();
                (l.progress, l.text.clone(), l.live.enabled(), l.live.texture())
            };
            modal(ctx, &p, "laden", 420.0, |ui| {
                ui.label(RichText::new(text).family(semibold()).size(16.0));
                ui.add_space(8.0);
                progress_bar(ui, &p, progress);
                if live_on {
                    ui.add_space(12.0);
                    live_picture(ui, &p, live_tex);
                }
                ui.add_space(12.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                        cancel = true;
                    }
                    if live_button(ui, &p, live_on).clicked() {
                        toggle_live = true;
                    }
                });
            });
            if toggle_live {
                self.prefs.live_preview = !live_on;
                if let Some(l) = self.loader.as_mut() {
                    l.live.set_enabled(!live_on);
                }
            }
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
                    ui.label(RichText::new(tr!("Das hat nicht geklappt", "That didn't work")).family(semibold()).size(17.0));
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
        self.dialog_scenes(ctx);
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
        let start_dir = self.save_start_dir();
        let export_rows = export_rows(&disc, &self.plan);
        let thumbs = &mut self.thumbs;
        let dlg = self.backup_dlg.as_mut().unwrap();
        modal(ctx, &p, "backup", 560.0, |ui| {
            ui.label(RichText::new(tr!("Exportieren", "Export")).family(semibold()).size(20.0));
            ui.add_space(4.0);
            let width = ui.available_width();
            let opts = [
                (OutputKind::Iso, tr!("ISO-Abbild", "ISO image")),
                (OutputKind::Folder, tr!("Ordner", "Folder")),
                (OutputKind::Vob, tr!("VOB-Datei", "VOB file")),
                (OutputKind::Mpeg, "MPEG-2"),
                (OutputKind::Mkv, "MKV"),
            ];
            segmented(ui, &p, &mut dlg.kind, &opts, width);
            ui.add_space(6.0);
            let file_kind = matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv);
            let rows = vob_rows(&disc, &self.plan);
            // Einzeldateien gibt es auch bei nur einem Titel: dann kommt der Dateiname aus der Liste
            if rows.is_empty() {
                dlg.separate = false;
            }
            let separate = file_kind && dlg.separate;
            let hint = match dlg.kind {
                OutputKind::Iso if dlg.burn => tr!("Legt ein ISO-Abbild an und öffnet danach den Windows-Brenner.", "Creates an ISO image and then opens the Windows disc burner."),
                OutputKind::Iso => tr!("Eine einzige Datei, die du später brennen oder einbinden kannst.", "A single file you can burn or mount later."),
                _ if separate => tr!("Schreibt jeden gewählten Titel in eine eigene Datei. Der Name in der Liste unten wird zum Dateinamen.", "Writes each chosen title to its own file. The name in the list below becomes the file name."),
                OutputKind::Folder => tr!("Legt die Ordner VIDEO_TS und AUDIO_TS an.", "Creates the VIDEO_TS and AUDIO_TS folders."),
                OutputKind::Burn => tr!("Legt ein ISO-Abbild an und öffnet danach den Windows-Brenner.", "Creates an ISO image and then opens the Windows disc burner."),
                OutputKind::Vob => tr!("Schreibt die gewählten Titel nacheinander in eine einzige VOB-Datei, ohne DVD-Struktur und Menüs.", "Writes the chosen titles one after another into a single VOB file, without DVD structure and menus."),
                OutputKind::Mpeg => tr!("Schreibt die gewählten Titel als schlichte MPEG-2-Datei: nur Bild und eine Tonspur, ohne DVD-Navigation und Untertitel.", "Writes the chosen titles as a plain MPEG-2 file: video and one audio track only, without DVD navigation and subtitles."),
                OutputKind::Mkv => tr!("Wie MPEG-2 (Bild und eine Tonspur, ohne Neukodierung), aber als MKV-Datei mit Kapiteln. VLC, Kodi und viele Fernseher zeigen die Kapitel an.", "Like MPEG-2 (video and one audio track, no re-encoding), but as an MKV file with chapters. VLC, Kodi and many TVs show the chapters."),
            };
            ui.label(RichText::new(hint).size(13.0).color(p.muted));
            ui.add_space(4.0);

            ui.horizontal(|ui| {
                // Einzeldateien: hier wird nur der Zielordner gewählt
                let is_folder = dlg.kind == OutputKind::Folder || separate;
                let is_vob = dlg.kind == OutputKind::Vob;
                let is_mpeg = dlg.kind == OutputKind::Mpeg;
                let is_mkv = dlg.kind == OutputKind::Mkv;
                let value = match dlg.kind {
                    _ if separate => &mut dlg.separate_dir,
                    OutputKind::Folder => &mut dlg.folder,
                    OutputKind::Vob => &mut dlg.vob,
                    OutputKind::Mpeg => &mut dlg.mpg,
                    OutputKind::Mkv => &mut dlg.mkv,
                    _ => &mut dlg.iso,
                };
                let edit = egui::TextEdit::singleline(value)
                    .desired_width(ui.available_width() - 120.0)
                    .margin(Margin::symmetric(10, 8));
                ui.add(edit);
                if soft_button(ui, &p, &trf!("{}  Wählen", "{}  Choose", ic::FOLDER_OPEN)).clicked() {
                    if is_folder {
                        // Im Ordner über dem bisherigen Ziel beginnen (Einzeldateien: im Ziel selbst)
                        let cur = PathBuf::from(value.as_str());
                        let dir = if separate && cur.is_dir() { Some(cur.clone()) } else { cur.parent().filter(|x| x.is_dir()).map(Path::to_path_buf) };
                        let dir = dir.unwrap_or_else(|| start_dir.clone());
                        if let Some(f) = rfd::FileDialog::new().set_title(tr!("Zielordner wählen", "Choose target folder")).set_directory(dir).pick_folder() {
                            *value = f.display().to_string();
                        }
                    } else {
                        let mut d = if is_mkv {
                            rfd::FileDialog::new().set_title(tr!("MKV-Datei speichern", "Save MKV file")).add_filter("Matroska", &["mkv"])
                        } else if is_mpeg {
                            rfd::FileDialog::new().set_title(tr!("MPEG-2-Datei speichern", "Save MPEG-2 file")).add_filter("MPEG-2", &["mpg", "mpeg"])
                        } else if is_vob {
                            rfd::FileDialog::new().set_title(tr!("VOB-Datei speichern", "Save VOB file")).add_filter(tr!("VOB-Datei", "VOB file"), &["vob"])
                        } else {
                            rfd::FileDialog::new().set_title(tr!("ISO-Abbild speichern", "Save ISO image")).add_filter(tr!("DVD-Abbild", "DVD image"), &["iso"])
                        };
                        let cur = PathBuf::from(value.as_str());
                        if let Some(n) = cur.file_name() {
                            d = d.set_file_name(n.to_string_lossy());
                        }
                        d = d.set_directory(cur.parent().filter(|x| x.is_dir()).map(Path::to_path_buf).unwrap_or_else(|| start_dir.clone()));
                        if let Some(f) = d.save_file() {
                            *value = f.display().to_string();
                        }
                    }
                }
            });

            if file_kind {
                ui.add_space(8.0);
                if !rows.is_empty() {
                    let text = if rows.len() == 1 {
                        tr!("Titel einzeln speichern, Dateiname aus der Liste", "Save title on its own, file name from the list")
                    } else {
                        tr!("Jeden Titel als eigene Datei speichern", "Save each title as a separate file")
                    };
                    // Ohne Häkchen gilt der Dateiname im Pfad oben
                    style::check(ui, &p, &mut dlg.separate, text);
                    ui.add_space(4.0);
                }
                let head = if separate { tr!("Titel und Dateinamen", "Titles and file names") } else { tr!("Titel für die Datei", "Titles for the file") };
                ui.label(RichText::new(head).family(semibold()).size(13.0).color(p.muted));
                let list_h = if separate { 230.0 } else { 250.0 };
                egui::ScrollArea::vertical().id_salt("vob-titel").max_height(list_h).show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 2.0;
                    dlg.vob_sel.resize(rows.len(), false);
                    dlg.names.resize(rows.len(), String::new());
                    for (i, default) in rows.iter().enumerate() {
                        // Jede Zeile wie in der Liste „Neue DVD“ mit Bildern von Anfang und Ende.
                        // Bei Einzeldateien ist der Name zugleich der Dateiname und lässt sich ändern.
                        if let Some(row) = export_rows.get(i) {
                            let mut changed = false;
                            ui.horizontal(|ui| {
                                style::check(ui, &p, &mut dlg.vob_sel[i], "");
                                let name = separate.then(|| (&mut dlg.names[i], default.as_str()));
                                let (resp, ch) = export_row(ui, &p, &disc, row, thumbs.as_mut(), name);
                                changed = ch;
                                if resp.clicked() {
                                    dlg.vob_sel[i] = !dlg.vob_sel[i];
                                }
                            });
                            if changed && self.plan.mode == Mode::Reauthor {
                                if let Some(e) = self.plan.reauthor.get_mut(i) {
                                    let n = dlg.names[i].trim();
                                    e.name = (!n.is_empty() && n != default).then(|| n.to_string());
                                }
                            }
                            continue;
                        }
                        if !separate {
                            let name = if dlg.names[i].trim().is_empty() { default.clone() } else { dlg.names[i].clone() };
                            style::check(ui, &p, &mut dlg.vob_sel[i], &name);
                            continue;
                        }
                        ui.horizontal(|ui| {
                            style::check(ui, &p, &mut dlg.vob_sel[i], "");
                            // Der Name lässt sich ändern; beim Neu-Zusammenstellen bleibt er am Eintrag.
                            let edit = egui::TextEdit::singleline(&mut dlg.names[i]).desired_width(ui.available_width() - 4.0).hint_text(default.as_str());
                            if ui.add(edit).on_hover_text(tr!("Name des Titels, zugleich Dateiname beim Export als Einzeldateien", "Name of the title, also the file name when exporting separate files")).changed()
                                && self.plan.mode == Mode::Reauthor
                            {
                                if let Some(e) = self.plan.reauthor.get_mut(i) {
                                    let n = dlg.names[i].trim();
                                    e.name = (!n.is_empty() && n != default).then(|| n.to_string());
                                }
                            }
                        });
                    }
                    if rows.is_empty() {
                        ui.label(RichText::new(tr!("Die Liste „Neue DVD“ ist noch leer.", "The \"New DVD\" list is still empty.")).size(13.0).color(p.muted));
                    }
                });
                if separate {
                    // Zusatz an die Dateinamen, z. B. damit „Film.vob“ neben der Quelle „Film_Neu.vob“ heißt
                    ui.add_space(2.0);
                    ui.horizontal(|ui| {
                        style::check(ui, &p, &mut dlg.suffix_on, tr!("Zusatz an die Dateinamen hängen:", "Append to the file names:"));
                        let edit = egui::TextEdit::singleline(&mut dlg.suffix).desired_width(120.0).hint_text("_Neu");
                        ui.add_enabled(dlg.suffix_on, edit).on_hover_text(tr!(
                            "Wird an jeden Titelnamen gehängt, z. B. „Film“ wird zu „Film_Neu“. So überschreibt der Export keine gleichnamigen Dateien.",
                            "Appended to each title name, e.g. \"Film\" becomes \"Film_Neu\". This way the export does not overwrite files with the same name."
                        ));
                    });
                    let ext = file_ext(dlg.kind, &dlg.ext);
                    let files = separate_names(&dlg.names, &rows, &dlg.vob_sel, dlg.name_suffix());
                    let mut shown: Vec<String> = files.iter().take(3).map(|(_, n)| format!("{n}.{ext}")).collect();
                    if files.len() > 3 {
                        shown.push("…".into());
                    }
                    ui.label(
                        RichText::new(format!("{}  {}: {}", ic::FILES, discslim::i18n::count(files.len(), "Datei", "Dateien", "file", "files"), shown.join(", ")))
                            .size(12.5)
                            .color(p.faint),
                    );
                }
            }

            ui.add_space(8.0);
            ui.label(RichText::new(tr!("Optionen", "Options")).family(semibold()).size(13.0).color(p.muted));
            let full_done = disc.analysed_full;
            let mut deep = dlg.deep || full_done;
            ui.add_enabled_ui(!full_done, |ui| {
                style::check(ui, &p, &mut deep, tr!("Vorher Tiefenanalyse ausführen (genauer, dauert länger)", "Run deep analysis first (more accurate, takes longer)"));
            });
            if !full_done {
                dlg.deep = deep;
            }
            if matches!(dlg.kind, OutputKind::Mpeg | OutputKind::Mkv) {
                // Tonspur wählen (aus dem ersten gewählten Titel)
                let tracks = first_selected_tracks(&disc, &self.plan, &dlg.vob_sel);
                if dlg.mpeg_audio.is_none_or(|ph| !tracks.iter().any(|t| t.0 == ph)) {
                    dlg.mpeg_audio = tracks.first().map(|t| t.0);
                }
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tr!("Tonspur", "Audio track")).color(p.muted));
                    let current = tracks.iter().find(|t| Some(t.0) == dlg.mpeg_audio).map(|t| t.1.clone()).unwrap_or_else(|| tr!("keine", "none").into());
                    egui::ComboBox::from_id_salt("mpeg-ton").width(260.0).selected_text(current).show_ui(ui, |ui| {
                        for (ph, name) in &tracks {
                            ui.selectable_value(&mut dlg.mpeg_audio, Some(*ph), name);
                        }
                    });
                });
            }
            match dlg.kind {
                OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv => {
                    style::check(ui, &p, &mut dlg.vob_keep_target, tr!("Automatik hält auch hier die Zielgröße ein", "Automatic mode keeps the target size here too"));
                }
                _ => {
                    if dlg.kind == OutputKind::Iso {
                        style::check(ui, &p, &mut dlg.burn, tr!("Danach brennen (öffnet den Windows-Brenner)", "Burn afterwards (opens the Windows disc burner)"));
                    }
                    style::check(ui, &p, &mut self.plan.remove_puo, tr!("Sperren für Vorspulen und Menütasten aufheben", "Remove fast-forward and menu button locks"));
                    if self.plan.mode == Mode::FullDisc {
                        style::check(ui, &p, &mut self.plan.region_free, tr!("Ländercode der DVD entfernen", "Remove the DVD's region code"));
                    }
                }
            }
            if self.plan.mode == Mode::Reauthor || matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv) {
                style::check(ui, &p, &mut self.prefs.declick, tr!("Ton an Schnittstellen ganz kurz weich blenden (gegen Knacken)", "Briefly soften the audio at cut points (prevents clicks)"));
            }
            style::check(ui, &p, &mut self.prefs.verify, tr!("Ausgabe danach prüfen (Tonrahmen und Zeitstempel)", "Check the output afterwards (audio frames and timestamps)"));
            ui.horizontal(|ui| {
                style::check(ui, &p, &mut self.prefs.sheet, tr!("Übersichtsbild als JPG speichern, ein Bild alle", "Save an overview JPG, one picture every"));
                ui.add_enabled(
                    self.prefs.sheet,
                    egui::DragValue::new(&mut self.prefs.sheet_secs).range(5..=3600).speed(1.0).suffix(tr!(" s", " s")),
                );
            });
            if matches!(dlg.kind, OutputKind::Iso | OutputKind::Folder) {
                style::check(ui, &p, &mut dlg.no_split, tr!("VOB-Dateien in voller Länge, nicht in 1-GB-Teile zerlegen", "Keep VOB files in full length, do not split into 1 GB parts"));
                if dlg.no_split {
                    ui.label(
                        RichText::new(trf!(
                            "{}  Gut zum Weiterverarbeiten. DVD-Player und Brennprogramme erwarten aber die geteilten Dateien.", "{}  Good for further processing. DVD players and burning software expect the split files, though.",
                            ic::INFO
                        ))
                        .size(12.5)
                        .color(p.warn),
                    );
                }
            }

            ui.add_space(8.0);
            let est = if matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv) {
                self.plan.for_single_file(&disc, &dlg.vob_sel, dlg.vob_keep_target).estimate(&disc)
            } else {
                self.plan.estimate(&disc)
            };
            let needed = (est.total * 2048.0) as u64;
            let need_txt = fmt_size(est.total);
            ui.label(RichText::new(trf!("Voraussichtliche Größe: {need_txt}", "Estimated size: {need_txt}")).size(14.0));
            let target_path = PathBuf::from(match dlg.kind {
                _ if separate => &dlg.separate_dir,
                OutputKind::Folder => &dlg.folder,
                OutputKind::Vob => &dlg.vob,
                OutputKind::Mpeg => &dlg.mpg,
                OutputKind::Mkv => &dlg.mkv,
                _ => &dlg.iso,
            });
            // ISO entsteht über einen Arbeitsordner und braucht vorübergehend doppelt so viel Platz.
            let is_file = dlg.kind != OutputKind::Folder;
            let required = if dlg.kind == OutputKind::Iso { needed * 2 } else { needed };
            let mut blocked = false;
            if let Some(free) = free_space(&target_path) {
                if free < required {
                    blocked = true;
                    ui.label(
                        RichText::new(trf!(
                            "{}  Auf dem Ziel sind nur {} frei, nötig sind etwa {}.", "{}  Only {} free on the target, about {} needed.",
                            ic::WARNING,
                            fmt_size(free as f64 / 2048.0),
                            fmt_size(required as f64 / 2048.0)
                        ))
                        .color(p.bad),
                    );
                }
            }
            if !est.fits {
                ui.label(RichText::new(trf!("{}  Das Ergebnis wird größer als das Ziel.", "{}  The result will be larger than the target.", ic::WARNING)).color(p.warn));
            }
            let _ = is_file;
            let path_ok = !target_path.as_os_str().is_empty() && (!file_kind || dlg.vob_sel.iter().any(|on| *on));

            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                let ok = path_ok && !blocked;
                if primary_button(ui, &p, &trf!("{}  Export starten", "{}  Start export", ic::PLAY), ok).clicked() {
                    start = true;
                }
                // Die Warteschlange prüft den Platz erst, wenn sie den Eintrag abarbeitet.
                let q = ui
                    .add_enabled(path_ok, egui::Button::new(RichText::new(trf!("{}  Zur Warteschlange", "{}  Add to queue", ic::QUEUE)).size(14.0).color(p.text))
                        .fill(p.raised)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(9))
                        .min_size(Vec2::new(0.0, 36.0)))
                    .on_hover_text(tr!("Mit diesen Einstellungen später zusammen mit anderen DVDs sichern", "Back up later with these settings, together with other DVDs"));
                if q.clicked() {
                    enqueue = true;
                }
                if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
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
            let rows = vob_rows(&disc, &self.plan);
            let found = self.backup_dlg.as_ref().map(|d| existing_files(d, &rows)).unwrap_or_default();
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
                ui.label(RichText::new(tr!("Dateien überschreiben?", "Overwrite files?")).family(semibold()).size(18.0));
            });
            ui.add_space(4.0);
            let text = if found.len() == 1 {
                tr!("Diese Datei gibt es am Ziel schon:", "This file already exists at the target:").to_string()
            } else {
                trf!("Diese {} Dateien gibt es am Ziel schon:", "These {} files already exist at the target:", found.len())
            };
            ui.label(RichText::new(text).color(p.muted));
            egui::ScrollArea::vertical().id_salt("vorhanden").max_height(150.0).show(ui, |ui| {
                for name in found.iter().take(60) {
                    ui.label(RichText::new(name).size(13.0));
                }
                if found.len() > 60 {
                    ui.label(RichText::new(trf!("… und {} weitere", "… and {} more", found.len() - 60)).size(13.0).color(p.muted));
                }
            });
            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Beim Überschreiben gehen sie verloren.", "They will be lost when overwritten.")).size(13.0).color(p.muted));
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, tr!("Überschreiben", "Overwrite"), true).clicked() {
                    yes = true;
                }
                if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
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
        let mut reqs: Vec<BackupRequest> = self
            .requests_from_dialog(&dlg, &disc)
            .into_iter()
            .map(|(output, plan)| BackupRequest { disc: disc.clone(), plan, output, deep_analysis: dlg.deep })
            .collect();
        self.preview.pause();
        let (tx, rx) = channel();
        let cancel = Arc::new(AtomicBool::new(false));
        let cancel2 = cancel.clone();
        let ctx2 = ctx.clone();
        let live = super::thumbs::LiveView::start(self.prefs.live_preview);
        let frame = live.frame.clone();
        std::thread::spawn(move || {
            discslim::engine::live::bind(Some(frame));
            if reqs.len() == 1 {
                backup::run(reqs.remove(0), tx, cancel2);
            } else {
                backup::run_many(reqs, tx, cancel2);
            }
            discslim::engine::live::bind(None);
            ctx2.request_repaint();
        });
        self.job = Some(Job {
            rx,
            cancel,
            started: Instant::now(),
            stage: tr!("Vorbereiten", "Preparing").into(),
            current: String::new(),
            fraction: 0.0,
            sectors_out: 0,
            ratio: 1.0,
            live,
        });
    }

    /// Stellt den Backup-Auftrag aus dem Dialog in die Warteschlange.
    fn enqueue_from_dialog(&mut self) {
        let Some(dlg) = self.backup_dlg.take() else { return };
        let Some(disc) = self.disc.clone() else { return };
        // Einzeldateien: je Datei ein Eintrag in der Warteschlange
        for (output, plan) in self.requests_from_dialog(&dlg, &disc) {
            let mut item = BatchItem::new(
                disc.source.origin.clone(),
                disc.source.label.clone(),
                PlanSource::Fixed { plan: Box::new(plan), fingerprint: Fingerprint::of(&disc) },
                output,
                dlg.deep,
            );
            item.more_sources = disc.source.origins.iter().skip(1).cloned().collect();
            self.queue.push(item);
        }
        let n = self.queue.items().len();
        self.notice = Some((trf!("In die Warteschlange gestellt ({})", "Added to the queue ({})", discslim::i18n::count(n, "Eintrag", "Einträge", "entry", "entries")), Instant::now()));
    }

    /// Ziele und Pläne zum Export-Dialog: eines, oder beim Export als Einzeldateien je
    /// gewähltem Titel eines. Merkt sich die Optionen fürs nächste Mal.
    fn requests_from_dialog(&mut self, dlg: &BackupDialog, disc: &Disc) -> Vec<(Output, Plan)> {
        let file_kind = matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv);
        if !(file_kind && dlg.separate) {
            return vec![self.request_from_dialog(dlg, disc)];
        }
        self.prefs.backup_kind = dlg.kind;
        self.prefs.vob_keep_target = dlg.vob_keep_target;
        self.prefs.file_dir = dlg.separate_dir.clone();
        self.prefs.file_suffix_on = dlg.suffix_on;
        self.prefs.file_suffix = dlg.suffix.clone();
        self.note_save_dir(Path::new(&dlg.separate_dir));
        let rows = vob_rows(disc, &self.plan);
        let dir = PathBuf::from(&dlg.separate_dir);
        let ext = file_ext(dlg.kind, &dlg.ext);
        separate_names(&dlg.names, &rows, &dlg.vob_sel, dlg.name_suffix())
            .into_iter()
            .map(|(i, name)| {
                let only: Vec<bool> = (0..rows.len()).map(|k| k == i).collect();
                let path = dir.join(format!("{name}.{ext}"));
                let output = match dlg.kind {
                    OutputKind::Vob => Output::Vob(path),
                    OutputKind::Mpeg => Output::Mpeg { path, audio: dlg.mpeg_audio },
                    _ => Output::Mkv { path, audio: dlg.mpeg_audio },
                };
                let mut plan = self.plan.for_single_file(disc, &only, dlg.vob_keep_target);
                plan.declick = self.prefs.declick;
                plan.verify = self.prefs.verify;
                plan.sheet_secs = self.prefs.sheet.then_some(self.prefs.sheet_secs.max(1));
                (output, plan)
            })
            .collect()
    }

    /// Ziel und Plan zum Backup-Dialog; merkt sich die Optionen fürs nächste Mal.
    fn request_from_dialog(&mut self, dlg: &BackupDialog, disc: &Disc) -> (Output, Plan) {
        // Ziel und Optionen fürs nächste Mal merken
        self.prefs.backup_kind = if dlg.kind == OutputKind::Iso && dlg.burn { OutputKind::Burn } else { dlg.kind };
        self.prefs.no_split = dlg.no_split;
        self.prefs.vob_keep_target = dlg.vob_keep_target;
        self.prefs.remove_puo = self.plan.remove_puo;
        self.prefs.region_free = self.plan.region_free;
        let parent_of = |s: &str| PathBuf::from(s).parent().map(|p| p.display().to_string()).unwrap_or_default();
        let dir = match dlg.kind {
            OutputKind::Folder => {
                self.prefs.folder_dir = parent_of(&dlg.folder);
                self.prefs.folder_dir.clone()
            }
            OutputKind::Vob => {
                self.prefs.file_dir = parent_of(&dlg.vob);
                self.prefs.file_dir.clone()
            }
            OutputKind::Mpeg => {
                self.prefs.file_dir = parent_of(&dlg.mpg);
                self.prefs.file_dir.clone()
            }
            OutputKind::Mkv => {
                self.prefs.file_dir = parent_of(&dlg.mkv);
                self.prefs.file_dir.clone()
            }
            _ => {
                self.prefs.file_dir = parent_of(&dlg.iso);
                self.prefs.file_dir.clone()
            }
        };
        self.note_save_dir(Path::new(&dir));
        let output = match dlg.kind {
            OutputKind::Folder => Output::Folder(PathBuf::from(&dlg.folder)),
            OutputKind::Iso if dlg.burn => Output::Burn(with_iso_ext(&dlg.iso)),
            OutputKind::Iso => Output::Iso(with_iso_ext(&dlg.iso)),
            OutputKind::Burn => Output::Burn(with_iso_ext(&dlg.iso)),
            OutputKind::Vob => Output::Vob(with_ext_case(&dlg.vob, &file_ext(dlg.kind, &dlg.ext))),
            OutputKind::Mpeg => Output::Mpeg { path: with_ext_case(&dlg.mpg, &file_ext(dlg.kind, &dlg.ext)), audio: dlg.mpeg_audio },
            OutputKind::Mkv => Output::Mkv { path: with_ext_case(&dlg.mkv, &file_ext(dlg.kind, &dlg.ext)), audio: dlg.mpeg_audio },
        };
        let plan = if matches!(dlg.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv) {
            self.plan.for_single_file(disc, &dlg.vob_sel, dlg.vob_keep_target)
        } else {
            let mut plan = self.plan.clone();
            // Eine gebrannte DVD muss immer die übliche Teilung haben.
            plan.split_vob = !dlg.no_split || (dlg.kind == OutputKind::Iso && dlg.burn);
            plan
        };
        let mut plan = plan;
        plan.declick = self.prefs.declick;
        plan.verify = self.prefs.verify;
        plan.sheet_secs = self.prefs.sheet.then_some(self.prefs.sheet_secs.max(1));
        (output, plan)
    }

    fn dialog_job(&mut self, ctx: &egui::Context) {
        let p = self.pal;
        let Some(j) = self.job.as_ref() else { return };
        let mut cancel = false;
        let mut toggle_live = false;
        let (fraction, stage, sectors, ratio, elapsed) =
            (j.fraction, j.stage.clone(), j.sectors_out, j.ratio, j.started.elapsed().as_secs_f64());
        let (live_on, live_tex) = (j.live.enabled(), j.live.texture());
        modal(ctx, &p, "job", 500.0, |ui| {
            ui.label(RichText::new(tr!("Export läuft", "Export running")).family(semibold()).size(20.0));
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
                        ui.label(RichText::new(trf!("noch etwa {}", "about {} left", fmt_duration(remaining))).color(p.muted));
                    }
                });
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(trf!(
                    "Bisher geschrieben: {}  ·  Bild behält {:.0} %  ·  Vergangen: {}", "Written so far: {}  ·  Video keeps {:.0} %  ·  Elapsed: {}",
                    fmt_size(sectors as f64),
                    ratio * 100.0,
                    fmt_duration(elapsed)
                ))
                .size(13.0)
                .color(p.muted),
            );
            if live_on {
                ui.add_space(10.0);
                live_picture(ui, &p, live_tex);
            }
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                    cancel = true;
                }
                if live_button(ui, &p, live_on).clicked() {
                    toggle_live = true;
                }
            });
        });
        if toggle_live {
            self.prefs.live_preview = !live_on;
            if let Some(j) = self.job.as_mut() {
                j.live.set_enabled(!live_on);
            }
        }
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
                    ui.label(RichText::new(tr!("Export fertig", "Export finished")).family(semibold()).size(20.0));
                });
                ui.add_space(4.0);
                ui.label(
                    RichText::new(trf!(
                        "{} geschrieben von ursprünglich {}. Dauer: {}", "{} written from originally {}. Duration: {}",
                        fmt_size(s.sectors as f64),
                        fmt_size(s.source_sectors),
                        fmt_duration(s.seconds)
                    ))
                    .size(14.0),
                );
                let where_ = if s.files > 1 {
                    trf!("{} Dateien in {}", "{} files in {}", s.files, s.path.display())
                } else {
                    s.path.display().to_string()
                };
                ui.label(RichText::new(where_).size(13.0).color(p.muted));
                if !s.fits {
                    ui.label(
                        RichText::new(trf!(
                            "{}  Das Ergebnis ist größer als das Ziel von {}.", "{}  The result is larger than the target of {}.",
                            ic::WARNING,
                            fmt_size(s.target_sectors as f64)
                        ))
                        .color(p.warn),
                    );
                }
                if s.burn_started {
                    ui.label(RichText::new(tr!("Der Windows-Brenner wurde geöffnet.", "The Windows disc burner has been opened.")).color(p.muted));
                }
                if let Some(c) = &s.check {
                    ui.add_space(4.0);
                    let places = |v: &[f64]| {
                        let mut t: Vec<String> = v.iter().take(6).map(|s| fmt_time((s * 1000.0) as u64)).collect();
                        if v.len() > 6 {
                            t.push("…".into());
                        }
                        t.join(", ")
                    };
                    if c.is_clean() {
                        ui.label(
                            RichText::new(trf!(
                                "{}  Ausgabe geprüft ({}): Ton sauber, keine beschädigten Tonrahmen, keine Zeitsprünge.", "{}  Output checked ({}): audio clean, no damaged audio frames, no time jumps.",
                                ic::CHECK,
                                fmt_time((c.seconds * 1000.0) as u64)
                            ))
                            .size(13.5)
                            .color(p.good),
                        );
                    } else {
                        if !c.audio_breaks.is_empty() {
                            ui.label(
                                RichText::new(trf!(
                                    "{}  {}, bei {}", "{}  {}, at {}",
                                    ic::WARNING,
                                    discslim::i18n::count(c.audio_breaks.len(), "beschädigte Tonstelle", "beschädigte Tonstellen", "damaged audio spot", "damaged audio spots"),
                                    places(&c.audio_breaks)
                                ))
                                .size(13.5)
                                .color(p.warn),
                            );
                        }
                        if !c.time_jumps.is_empty() {
                            ui.label(
                                RichText::new(trf!(
                                    "{}  {} im Ton, bei {}", "{}  {} in the audio, at {}",
                                    ic::WARNING,
                                    discslim::i18n::count(c.time_jumps.len(), "Zeitsprung", "Zeitsprünge", "time jump", "time jumps"),
                                    places(&c.time_jumps)
                                ))
                                .size(13.5)
                                .color(p.warn),
                            );
                        }
                    }
                }
                if let Some(sheet) = &s.sheet {
                    ui.label(
                        RichText::new(trf!("{}  Übersichtsbild: {}", "{}  Overview picture: {}", ic::IMAGE, sheet.display()))
                            .size(13.0)
                            .color(p.muted),
                    );
                }
                if let Some(e) = &s.extra_error {
                    ui.label(RichText::new(format!("{}  {e}", ic::INFO)).size(13.0).color(p.warn));
                }
                ui.add_space(10.0);
                ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                    if primary_button(ui, &p, tr!("Schließen", "Close"), true).clicked() {
                        close = true;
                    }
                    if let Some(sheet) = &s.sheet {
                        if soft_button(ui, &p, &trf!("{}  Übersichtsbild öffnen", "{}  Open overview picture", ic::IMAGE)).clicked() {
                            let _ = std::process::Command::new("explorer").arg(sheet).spawn();
                        }
                    }
                    if soft_button(ui, &p, &trf!("{}  Im Explorer zeigen", "{}  Show in Explorer", ic::FOLDER_OPEN)).clicked() {
                        open_at = Some(s.path.clone());
                    }
                });
            }
            JobResult::Failed(e) => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new(ic::X_CIRCLE).size(28.0).color(p.bad));
                    ui.label(RichText::new(tr!("Export fehlgeschlagen", "Export failed")).family(semibold()).size(20.0));
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
                ui.label(RichText::new(tr!("Export abgebrochen", "Export cancelled")).family(semibold()).size(20.0));
                ui.label(RichText::new(tr!("Angefangene Dateien wurden entfernt.", "Partially written files have been removed.")).color(p.muted));
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
        let dirs_before = (self.prefs.load_dir.clone(), self.prefs.save_dir.clone(), self.prefs.project_dir.clone());
        modal(ctx, &p, "einstellungen", 520.0, |ui| {
            ui.label(RichText::new(tr!("Einstellungen", "Settings")).family(semibold()).size(20.0));
            ui.add_space(4.0);
            // Sprachnamen jeweils in der eigenen Sprache, damit man sie immer findet
            ui.label(RichText::new("Sprache / Language").family(semibold()).size(13.0).color(p.muted));
            let width = ui.available_width();
            let mut ui_lang = discslim::i18n::lang();
            let opts = [(Lang::De, "Deutsch"), (Lang::En, "English")];
            if segmented(ui, &p, &mut ui_lang, &opts, width) {
                discslim::i18n::set_lang(ui_lang);
            }

            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Design", "Theme")).family(semibold()).size(13.0).color(p.muted));
            let opts = [(true, tr!("Dunkel", "Dark")), (false, tr!("Hell", "Light"))];
            segmented(ui, &p, &mut dark, &opts, width);

            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Vorschau", "Preview")).family(semibold()).size(13.0).color(p.muted));
            style::check(ui, &p, &mut self.prefs.autoplay, tr!("Titel bei Klicken automatisch abspielen", "Play titles automatically when clicked"));
            style::check(ui, &p, &mut self.prefs.thumb_anim, tr!("Vorschaubilder der Einträge dauerhaft abspielen", "Play the entry thumbnails continuously"));
            ui.horizontal(|ui| {
                ui.label(RichText::new(tr!("Länge der Schleife", "Loop length")).color(p.muted));
                ui.add(egui::DragValue::new(&mut self.prefs.thumb_loop_secs).range(1..=60).speed(0.2).suffix(tr!(" s", " s")));
            });
            ui.label(
                RichText::new(tr!(
                    "Die kleinen Bilder für Anfang und Ende laufen dann ohne Maus als Schleife. Bei einer DVD im Laufwerk nur beim Überfahren mit der Maus, damit das Laufwerk nicht ständig springt.",
                    "The small start and end pictures then loop without the mouse. With a DVD in the drive only on mouse-over, so the drive does not keep seeking."
                ))
                .size(12.5)
                .color(p.faint),
            );

            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Ton- und Untertitelsprache", "Audio and subtitle language")).family(semibold()).size(13.0).color(p.muted));
            style::check(ui, &p, &mut self.prefs.only_lang, tr!("Beim Öffnen nur diese Sprache behalten", "Keep only this language when opening"));
            ui.add_enabled_ui(self.prefs.only_lang, |ui| {
                egui::ComboBox::from_id_salt("sprache").selected_text(lang_name(&self.prefs.lang)).show_ui(ui, |ui| {
                    for code in ["de", "en", "fr", "es", "it", "nl", "pl", "tr", "ru"] {
                        ui.selectable_value(&mut self.prefs.lang, code.to_string(), lang_name(code));
                    }
                });
            });
            ui.label(
                RichText::new(tr!("Gibt es die Sprache auf der DVD nicht, bleiben alle Spuren ausgewählt.", "If the DVD doesn't have this language, all tracks stay selected."))
                    .size(12.5)
                    .color(p.faint),
            );

            ui.add_space(6.0);
            ui.label(RichText::new(tr!("Verzeichnisse", "Folders")).family(semibold()).size(13.0).color(p.muted));
            for (label, value, title) in [
                (tr!("Laden", "Load"), &mut self.prefs.load_dir, tr!("Hauptverzeichnis zum Laden von DVDs, ISOs und Projekten", "Main folder for loading DVDs, ISOs and projects")),
                (tr!("Speichern", "Save"), &mut self.prefs.save_dir, tr!("Hauptverzeichnis für Exporte", "Main folder for exports")),
                (tr!("Projekte", "Projects"), &mut self.prefs.project_dir, tr!("Ordner zum Laden und Speichern von Projekten", "Folder for loading and saving projects")),
            ] {
                ui.horizontal(|ui| {
                    ui.add_sized([70.0, 24.0], egui::Label::new(RichText::new(label).color(p.muted)));
                    let edit = egui::TextEdit::singleline(value)
                        .hint_text(tr!("nicht festgelegt", "not set"))
                        .desired_width(ui.available_width() - 120.0)
                        .margin(Margin::symmetric(8, 5));
                    ui.add(edit);
                    if ui.button(RichText::new(trf!("{}  Wählen", "{}  Choose", ic::FOLDER_OPEN)).size(13.0)).clicked() {
                        let mut d = rfd::FileDialog::new().set_title(title);
                        if Path::new(value.trim()).is_dir() {
                            d = d.set_directory(value.trim());
                        }
                        if let Some(f) = d.pick_folder() {
                            *value = f.display().to_string();
                        }
                    }
                    if !value.is_empty() && style::icon_button(ui, &p, ic::X, tr!("Nicht mehr festlegen", "Clear")).clicked() {
                        value.clear();
                    }
                });
            }
            for v in [&self.prefs.load_dir, &self.prefs.save_dir, &self.prefs.project_dir] {
                if !v.trim().is_empty() && !Path::new(v.trim()).is_dir() {
                    ui.label(RichText::new(trf!("{}  „{}“ gibt es nicht.", "{}  \"{}\" does not exist.", ic::WARNING, v.trim())).size(12.5).color(p.warn));
                }
            }
            ui.label(
                RichText::new(tr!("Nach dem Start öffnen die Dateidialoge hier. Danach merkt sich das Programm den zuletzt gewählten Ort.", "After startup, file dialogs open here. After that, the program remembers the last location you chose."))
                    .size(12.5)
                    .color(p.faint),
            );

            ui.add_space(6.0);
            ui.label(RichText::new("Export").family(semibold()).size(13.0).color(p.muted));
            style::check(ui, &p, &mut self.prefs.deep_default, tr!("Tiefenanalyse vor dem Export vorschlagen", "Suggest deep analysis before export"));
            ui.add_space(2.0);
            ui.label(RichText::new(tr!("Dateiendung beim Export", "File extension on export")).size(13.0).color(p.muted));
            for (i, (name, ext)) in [(tr!("VOB-Datei", "VOB file"), "vob"), ("MPEG-2", "mpg"), ("MKV", "mkv")].into_iter().enumerate() {
                ui.horizontal(|ui| {
                    ui.add_sized([110.0, 28.0], egui::Label::new(RichText::new(name).color(p.text)));
                    if i == 1 {
                        // MPEG-2: .mpg oder .mpeg, jeweils klein oder groß (Wert: lang, groß)
                        let mut v = (self.prefs.ext.mpeg_long, self.prefs.ext.upper[1]);
                        let opts = [((false, false), ".mpg"), ((false, true), ".MPG"), ((true, false), ".mpeg"), ((true, true), ".MPEG")];
                        if segmented(ui, &p, &mut v, &opts, 360.0) {
                            self.prefs.ext.mpeg_long = v.0;
                            self.prefs.ext.upper[1] = v.1;
                        }
                    } else {
                        let lower = format!(".{ext}");
                        let upper = format!(".{}", ext.to_ascii_uppercase());
                        let opts = [(false, lower.as_str()), (true, upper.as_str())];
                        segmented(ui, &p, &mut self.prefs.ext.upper[i], &opts, 180.0);
                    }
                });
            }

            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, tr!("Fertig", "Done"), true).clicked() {
                    close = true;
                }
            });
        });
        if dark != p.dark {
            self.set_dark(ctx, dark);
        }
        // Ein neu festgelegtes Hauptverzeichnis gilt sofort, nicht erst nach dem Neustart.
        if self.prefs.load_dir != dirs_before.0 {
            self.last_load_dir = None;
        }
        if self.prefs.save_dir != dirs_before.1 {
            self.last_save_dir = None;
            self.prefs.batch_dir.clear();
        }
        if self.prefs.project_dir != dirs_before.2 {
            self.last_project_dir = None;
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
        modal(ctx, &p, "ueber", 520.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::DISC).size(30.0).color(p.accent));
                ui.label(RichText::new("DVDmanager").family(semibold()).size(20.0));
                ui.label(RichText::new(trf!("Version {}", "Version {}", discslim::version::version())).size(13.0).color(p.muted));
            });
            ui.add_space(4.0);
            ui.label(
                RichText::new(
                    tr!("Verkleinert eigene, ungeschützte Video-DVDs, damit sie auf einen Rohling passen. Das Bild wird dabei neu quantisiert, nicht komplett neu kodiert. Das geht schnell, und Struktur und Menüs bleiben erhalten.", "Shrinks your own unprotected video DVDs so they fit on a blank disc. The video is requantised rather than fully re-encoded. This is fast, and structure and menus are preserved."),
                )
                .color(p.muted),
            );
            ui.add_space(6.0);
            ui.label(
                RichText::new(tr!("Kopiergeschützte DVDs (CSS) werden erkannt und nicht verarbeitet.", "Copy-protected DVDs (CSS) are detected and not processed.")).size(13.0).color(p.faint),
            );
            ui.add_space(12.0);
            style::section(ui, &p, ic::CLOCK_COUNTER_CLOCKWISE, tr!("ÄNDERUNGEN", "CHANGES"));
            egui::ScrollArea::vertical().id_salt("aenderungen").max_height(300.0).auto_shrink([false, true]).show(ui, |ui| {
                for r in discslim::version::releases() {
                    ui.horizontal(|ui| {
                        ui.label(RichText::new(&r.version).family(semibold()).size(14.0));
                        ui.label(RichText::new(&r.date).size(12.5).color(p.faint));
                    });
                    for line in &r.intro {
                        ui.label(RichText::new(line).size(13.0).color(p.muted));
                    }
                    for item in &r.items {
                        ui.horizontal_top(|ui| {
                            ui.spacing_mut().item_spacing.x = 6.0;
                            ui.label(RichText::new("•").size(13.0).color(p.accent));
                            ui.add(egui::Label::new(RichText::new(item).size(13.0)).wrap());
                        });
                    }
                    ui.add_space(8.0);
                }
            });
            ui.add_space(10.0);
            ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                if primary_button(ui, &p, tr!("Schließen", "Close"), true).clicked() {
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

/// Dateiname aus der Bezeichnung einer DVD bzw. dem Namen der Quelldatei (ohne Zusatz).
/// Nur Zeichen, die Windows in Dateinamen nicht erlaubt, werden ersetzt.
pub(super) fn file_stem(label: &str) -> String {
    let s: String = label
        .chars()
        .map(|c| if c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*') { '_' } else { c })
        .collect();
    let s = s.trim().trim_end_matches('.').trim_end().to_string();
    if s.is_empty() { "DVD".into() } else { s }
}

/// Name für das Ergebnis eines Exports: Name der Quelle mit „_converted“ (Test.vob → Test_converted.vob).
pub(super) fn sanitize(label: &str) -> String {
    format!("{}_converted", file_stem(label))
}

/// Dateiendung der Datei-Ausgaben nach den Einstellungen: groß oder klein, bei MPEG-2
/// außerdem .mpg oder .mpeg.
/// Knopf zum Ein- und Ausschalten des mitlaufenden Vorschaubildes.
fn live_button(ui: &mut Ui, p: &style::Pal, on: bool) -> egui::Response {
    let text = if on {
        format!("{}  {}", ic::IMAGE, tr!("Bild ausblenden", "Hide picture"))
    } else {
        format!("{}  {}", ic::IMAGE, tr!("Bild anzeigen", "Show picture"))
    };
    soft_button(ui, p, &text).on_hover_text(tr!(
        "Zeigt ein kleines Bild der Stelle, die gerade verarbeitet wird.",
        "Shows a small picture of the part currently being processed."
    ))
}

/// Mitlaufendes Vorschaubild, mittig in einer 16:9-Kachel.
fn live_picture(ui: &mut Ui, p: &style::Pal, tex: Option<(egui::TextureId, Vec2)>) {
    let w = 240.0f32.min(ui.available_width());
    ui.vertical_centered(|ui| {
        let (tile, _) = ui.allocate_exact_size(Vec2::new(w, w * 9.0 / 16.0), egui::Sense::hover());
        ui.painter().rect_filled(tile, CornerRadius::same(6), p.screen);
        match tex {
            Some((id, size)) => {
                let scale = (tile.width() / size.x.max(1.0)).min(tile.height() / size.y.max(1.0));
                let r = egui::Rect::from_center_size(tile.center(), size * scale);
                ui.painter().image(id, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            None => {
                ui.painter().text(
                    tile.center(),
                    egui::Align2::CENTER_CENTER,
                    tr!("Bild folgt …", "Picture coming …"),
                    egui::FontId::proportional(12.0),
                    p.muted,
                );
            }
        }
    });
}

impl BackupDialog {
    /// Der Zusatz für die Dateinamen beim Export als Einzeldateien (leer, wenn aus).
    fn name_suffix(&self) -> &str {
        if self.suffix_on { self.suffix.trim() } else { "" }
    }
}

fn file_ext(kind: OutputKind, style: &ExtStyle) -> String {
    let (ext, up) = match kind {
        OutputKind::Mpeg if style.mpeg_long => ("mpeg", style.upper[1]),
        OutputKind::Mpeg => ("mpg", style.upper[1]),
        OutputKind::Mkv => ("mkv", style.upper[2]),
        _ => ("vob", style.upper[0]),
    };
    if up { ext.to_ascii_uppercase() } else { ext.to_string() }
}

/// Dateinamen (ohne Endung) für den Export als Einzeldateien: je gewähltem Titel sein
/// Name (leer: die übliche Bezeichnung), unzulässige Zeichen ersetzt, doppelte durchnummeriert.
/// `suffix` hängt an jeden Namen (z. B. „_Neu“), damit die Ausgabe neben gleichnamigen
/// Quelldateien liegen kann.
fn separate_names(names: &[String], rows: &[String], sel: &[bool], suffix: &str) -> Vec<(usize, String)> {
    let mut out: Vec<(usize, String)> = Vec::new();
    for (i, row) in rows.iter().enumerate() {
        if !sel.get(i).copied().unwrap_or(false) {
            continue;
        }
        let name = names.get(i).map(|n| n.trim()).filter(|n| !n.is_empty()).unwrap_or(row);
        let base = file_stem(&format!("{name}{suffix}"));
        let mut cand = base.clone();
        let mut k = 2;
        while out.iter().any(|(_, n)| n.eq_ignore_ascii_case(&cand)) {
            cand = format!("{base} ({k})");
            k += 1;
        }
        out.push((i, cand));
    }
    out
}

/// Dateien, die das Backup am gewählten Ziel ersetzen würde.
fn existing_files(d: &BackupDialog, rows: &[String]) -> Vec<String> {
    if d.separate && matches!(d.kind, OutputKind::Vob | OutputKind::Mpeg | OutputKind::Mkv) {
        let dir = PathBuf::from(&d.separate_dir);
        let ext = file_ext(d.kind, &d.ext);
        return separate_names(&d.names, rows, &d.vob_sel, d.name_suffix())
            .into_iter()
            .map(|(_, n)| dir.join(format!("{n}.{ext}")))
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
    }
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
            let p = with_ext_case(&d.mpg, &file_ext(d.kind, &d.ext));
            if p.exists() { vec![p.display().to_string()] } else { Vec::new() }
        }
        OutputKind::Mkv => {
            let p = with_ext_case(&d.mkv, &file_ext(d.kind, &d.ext));
            if p.exists() { vec![p.display().to_string()] } else { Vec::new() }
        }
        OutputKind::Vob => {
            let p = with_ext_case(&d.vob, &file_ext(d.kind, &d.ext));
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

/// Wie `with_ext`, aber die Endung steht danach genau so da wie gewünscht (groß/klein).
/// Steht schon eine der Video-Endungen da (auch .mpg statt .mpeg), wird sie ersetzt.
fn with_ext_case(s: &str, ext: &str) -> PathBuf {
    let p = PathBuf::from(s);
    let known = p.extension().is_some_and(|e| {
        let e = e.to_string_lossy().to_ascii_lowercase();
        let want = ext.to_ascii_lowercase();
        e == want || (matches!(e.as_str(), "mpg" | "mpeg") && matches!(want.as_str(), "mpg" | "mpeg"))
    });
    if known { p.with_extension(ext) } else { PathBuf::from(format!("{s}.{ext}")) }
}

/// Zeilen für die Titelauswahl der VOB-Ausgabe.
/// Zeile der Titelliste im Export: Bereich für die Standbilder und die Texte.
struct ExportRow {
    title: usize,
    first: usize,
    last: usize,
    start: u64,
    end: u64,
    name: String,
    sub: String,
    size: String,
}

/// Zeilen passend zu `vob_rows`: Einträge der neuen DVD bzw. die Titel der DVD.
fn export_rows(disc: &Disc, plan: &Plan) -> Vec<ExportRow> {
    match plan.mode {
        Mode::FullDisc => disc
            .titles
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let kind = if t.category == Category::MainMovie { tr!("Hauptfilm", "Main movie") } else { tr!("Extra", "Extra") };
                ExportRow {
                    title: i,
                    first: 0,
                    last: t.chapters.len().saturating_sub(1),
                    start: 0,
                    end: t.ms,
                    name: trf!("{kind} · Titel {}", "{kind} · Title {}", t.number),
                    sub: format!("{} · {}", fmt_time(t.ms), discslim::i18n::chapters(t.chapters.len())),
                    size: discslim::engine::plan::fmt_size(disc.title_stats(t).total),
                }
            })
            .collect(),
        Mode::Reauthor => plan
            .reauthor
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let (name, sub, size) = super::views::entry_info(disc, i, e)?;
                let total = discslim::engine::plan::entry_duration_ms(disc, e);
                let (s, end) = e.cut.effective(total);
                Some(ExportRow { title: e.title, first: e.first_chapter, last: e.last_chapter, start: s, end: end.min(total), name, sub, size })
            })
            .collect(),
    }
}

/// Zeichnet eine Zeile der Titelliste: Bilder von Anfang und Ende, Name, Unterzeile, Größe.
/// Mit `name` (Text und Platzhalter) steht statt des Namens ein Eingabefeld; der zweite
/// Rückgabewert sagt, ob es geändert wurde.
fn export_row(
    ui: &mut Ui,
    p: &style::Pal,
    disc: &Disc,
    row: &ExportRow,
    thumbs: Option<&mut super::thumbs::Thumbs>,
    name: Option<(&mut String, &str)>,
) -> (egui::Response, bool) {
    // Mit Eingabefeld etwas höher, damit Feld und Unterzeile übereinander Platz haben
    let h = if name.is_some() { 54.0 } else { 44.0 };
    let (rect, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), h), egui::Sense::click());
    let painter = ui.painter_at(rect);
    if resp.hovered() {
        painter.rect_filled(rect, egui::CornerRadius::same(8), p.hover);
    }
    let (tw, th) = (58.0, 34.0);
    let end_frame = row.end.saturating_sub(40).max(row.start);
    let mut thumbs = thumbs;
    for (k, at) in [row.start, end_frame].into_iter().enumerate() {
        let r = egui::Rect::from_min_size(egui::pos2(rect.left() + 4.0 + k as f32 * (tw + 4.0), rect.center().y - th / 2.0), egui::vec2(tw, th));
        painter.rect_filled(r, egui::CornerRadius::same(4), p.screen);
        let seg = || discslim::engine::preview::title_segments(disc, row.title, row.first, row.last);
        if let Some(tex) = thumbs.as_deref_mut().and_then(|t| t.get((row.title, row.first, row.last, at), &disc.source, seg)) {
            painter.image(tex, r, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), egui::Color32::WHITE);
        }
    }
    let tx = rect.left() + 4.0 + 2.0 * (tw + 4.0) + 6.0;
    let size_r = painter.text(egui::pos2(rect.right() - 6.0, rect.center().y), egui::Align2::RIGHT_CENTER, &row.size, egui::FontId::new(12.5, semibold()), p.text);
    // Lange Namen enden auf „…“, der volle Name steht im Tooltip
    let max_w = size_r.left() - 12.0 - tx;
    let sub_y = if name.is_some() { rect.bottom() - 10.0 } else { rect.center().y + 9.0 };
    style::text_elided(&painter, egui::pos2(tx, sub_y), &row.sub, egui::FontId::proportional(12.0), p.muted, max_w);
    if let Some((text, hint)) = name {
        let r = egui::Rect::from_min_size(egui::pos2(tx - 4.0, rect.top() + 2.0), egui::vec2(max_w + 4.0, 24.0));
        let edit = egui::TextEdit::singleline(text).hint_text(hint).font(egui::FontId::new(13.5, semibold())).margin(Margin::symmetric(4, 1)).desired_width(max_w);
        let changed = ui
            .put(r, edit)
            .on_hover_text(tr!("Name des Titels, zugleich Dateiname beim Export als Einzeldateien", "Name of the title, also the file name when exporting separate files"))
            .changed();
        return (resp, changed);
    }
    let cut = style::text_elided(&painter, egui::pos2(tx, rect.center().y - 9.0), &row.name, egui::FontId::new(13.5, semibold()), p.text, max_w);
    (if cut { resp.on_hover_text(&row.name) } else { resp }, false)
}

fn vob_rows(disc: &Disc, plan: &Plan) -> Vec<String> {
    match plan.mode {
        Mode::FullDisc => disc
            .titles
            .iter()
            .map(|t| {
                let kind = if t.category == Category::MainMovie { tr!("Hauptfilm", "Main movie") } else { tr!("Extra", "Extra") };
                trf!("{kind} · Titel {} · {}", "{kind} · Title {} · {}", t.number, fmt_time(t.ms))
            })
            .collect(),
        Mode::Reauthor => plan
            .reauthor
            .iter()
            .enumerate()
            .filter_map(|(i, e)| {
                let t = disc.titles.get(e.title)?;
                Some(trf!("{}. Titel {} · Kapitel {} bis {}", "{}. Title {} · Chapters {} to {}", i + 1, t.number, e.first_chapter + 1, e.last_chapter + 1))
            })
            .collect(),
    }
}

#[cfg(test)]
mod name_tests {
    use super::separate_names;

    #[test]
    fn suffix_is_appended_before_numbering() {
        let rows = vec!["Titel 1".to_string(), "Titel 2".to_string(), "Titel 3".to_string()];
        let names = vec!["Film".to_string(), String::new(), "Film".to_string()];
        let sel = vec![true, true, true];
        let got: Vec<String> = separate_names(&names, &rows, &sel, "_Neu").into_iter().map(|(_, n)| n).collect();
        assert_eq!(got, vec!["Film_Neu", "Titel 2_Neu", "Film_Neu (2)"]);
        let plain: Vec<String> = separate_names(&names, &rows, &sel, "").into_iter().map(|(_, n)| n).collect();
        assert_eq!(plain[0], "Film");
    }
}
