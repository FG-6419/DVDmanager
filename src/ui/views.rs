//! Die sichtbaren Flächen: Kopfzeile, Startseite, Inhaltsliste, Einstellungen, Füllanzeige.

use eframe::egui::{
    self, Align, Align2, Color32, CornerRadius, FontId, Layout, Margin, Rect, RichText, ScrollArea, Sense, Stroke,
    StrokeKind, Ui, Vec2,
};
use egui_phosphor::regular as ic;

use discslim::dvd::ifo::lang_name;
use discslim::dvd::model::{fmt_time, Category, CellStats, Disc};
use discslim::engine::plan::{
    entry_duration_ms, entry_kept_fraction, Cut,
    dvd5_sectors, dvd9_sectors, fmt_size, reauthor_cells, Estimate, ItemId, Mode, ReauthorEntry,
    Smoothing, VideoMode, MIN_CHAPTER_GAP_MS, FadeKind,
};

use super::style::{self, card, item_row, legend, primary_button, section, segmented, semibold, soft_button};
use super::{App, Selection, TargetKind};

/// Oben, links und rechts dieselben Außenabstände.
const GAP: f32 = 12.0;

fn stat_sum_audio(s: &CellStats, keep: &[bool; 8]) -> f64 {
    (0..8).filter(|i| keep[*i]).map(|i| s.audio[i]).sum()
}

fn stat_sum_subs(s: &CellStats, keep: &[bool; 32]) -> f64 {
    (0..32).filter(|i| keep[*i]).map(|i| s.sub[i]).sum()
}

fn mode_label(m: VideoMode) -> &'static str {
    match m {
        VideoMode::Keep => tr!("Unverändert", "Unchanged"),
        VideoMode::Auto => tr!("Automatisch", "Automatic"),
        VideoMode::Custom(_) => tr!("Eigener Wert", "Custom value"),
        VideoMode::Still => tr!("Standbild", "Still image"),
    }
}

fn percent(v: f64) -> String {
    format!("{:.0} %", v * 100.0)
}

/// Zeit mit Hundertstelsekunden, für Schnittmarken.
pub fn fmt_time_precise(ms: u64) -> String {
    let s = ms / 1000;
    let sep = if discslim::i18n::is_en() { '.' } else { ',' };
    format!("{}:{:02}:{:02}{sep}{:02}", s / 3600, (s / 60) % 60, s % 60, (ms % 1000) / 10)
}

/// Einfache Qualitätseinschätzung für eine Verkleinerung.
pub fn quality_word(ratio: f32) -> (&'static str, usize) {
    if ratio >= 0.97 {
        (tr!("Original", "Original"), 0)
    } else if ratio >= 0.8 {
        (tr!("Sehr gut", "Very good"), 0)
    } else if ratio >= 0.65 {
        (tr!("Gut", "Good"), 0)
    } else if ratio >= 0.5 {
        (tr!("Mittel", "Medium"), 1)
    } else {
        (tr!("Sichtbare Verluste", "Visible losses"), 2)
    }
}

impl App {
    pub(super) fn view_topbar(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.pal;
        egui::Frame::new()
            .fill(p.panel)
            .stroke(Stroke::new(1.0, p.line))
            .inner_margin(Margin::symmetric(14, 10))
            .show(ui, |ui| {
                ui.set_width(ui.available_width());
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 8.0;
                    let logo = ui.add(egui::Label::new(RichText::new(ic::DISC).size(25.0).color(p.accent)).sense(Sense::click()));
                    let name = ui.add(egui::Label::new(RichText::new("DVDmanager").family(semibold()).size(17.0).color(p.text)).sense(Sense::click()));
                    // Versionsnummer; ein Klick zeigt die Änderungen
                    let ver = ui
                        .add(egui::Label::new(RichText::new(discslim::version::version()).size(12.0).color(p.faint)).sense(Sense::click()))
                        .on_hover_text(tr!("Version – Klick zeigt die Änderungen", "Version – click to see the changes"));
                    if ver.hovered() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if ver.clicked() {
                        self.about_open = true;
                    }
                    let back = logo.clicked() || name.clicked();
                    if (logo.hovered() || name.hovered()) && self.disc.is_some() && self.job.is_none() {
                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                    }
                    if back && self.job.is_none() && self.loader.is_none() {
                        // Zurück zur Startseite, um eine andere DVD einzulesen
                        self.disc = None;
                        self.preview.stop();
                        self.sel = Selection::None;
                        self.project_path = None;
                    }

                    let has_disc = self.disc.is_some();
                    if has_disc && self.is_single_file() {
                        // Einzelne Videodatei: nur Neu zusammenstellen
                        ui.add_space(14.0);
                        ui.label(RichText::new(trf!("{}  Einzeldatei", "{}  Single file", ic::FILE_VIDEO)).size(13.0).color(p.muted))
                            .on_hover_text(tr!("Eine einzelne VOB- oder MPEG-Datei: Schneiden, Zusammenstellen und Ausgeben wie bei einer DVD", "A single VOB or MPEG file: cut, assemble and write it like a DVD"));
                    } else if has_disc {
                        ui.add_space(14.0);
                        let mut mode = self.plan.mode;
                        let opts = [(Mode::FullDisc, tr!("Ganze DVD", "Whole DVD")), (Mode::Reauthor, tr!("Neu zusammenstellen", "Re-author"))];
                        if segmented(ui, &p, &mut mode, &opts, 310.0) {
                            self.plan.mode = mode;
                            self.sel = match mode {
                                Mode::FullDisc => self.default_selection(),
                                Mode::Reauthor => {
                                    if self.plan.reauthor.is_empty() {
                                        Selection::None
                                    } else {
                                        Selection::Entry(0)
                                    }
                                }
                            };
                            self.preview.dirty = true;
                        }
                    }

                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        if has_disc {
                            let ok = self.can_start_backup();
                            if primary_button(ui, &p, &trf!("{}  Backup erstellen", "{}  Create backup", ic::DOWNLOAD_SIMPLE), ok)
                                .clicked()
                            {
                                self.open_backup_dialog();
                            }
                        }
                        self.queue_badge(ui);
                        let theme_icon = if p.dark { ic::SUN } else { ic::MOON };
                        if style::icon_button(ui, &p, theme_icon, tr!("Design wechseln", "Switch theme")).clicked() {
                            let d = !p.dark;
                            self.set_dark(ctx, d);
                        }
                        if style::icon_button(ui, &p, ic::GEAR_SIX, tr!("Einstellungen", "Settings")).clicked() {
                            self.settings_open = true;
                        }
                        if style::icon_button(ui, &p, ic::INFO, tr!("Über dieses Programm", "About this program")).clicked() {
                            self.about_open = true;
                        }
                        if has_disc {
                            let full = self.disc.as_ref().map(|d| d.analysed_full).unwrap_or(false);
                            let tip = if full {
                                tr!("Die Tiefenanalyse ist bereits erledigt", "The deep analysis is already done")
                            } else {
                                tr!("Liest die ganze DVD für genaue Größenangaben. Sonst genügt eine Stichprobe.", "Reads the whole DVD for exact sizes. Otherwise a sample is enough.")
                            };
                            if style::icon_button(ui, &p, ic::MAGNIFYING_GLASS, tip).clicked() && !full {
                                self.deep_analyse(ctx);
                            }
                            let save_tip = match &self.project_path {
                                Some(p) => trf!("Projekt speichern (Strg+S)\n{}\nRechtsklick: Speichern unter …", "Save project (Ctrl+S)\n{}\nRight-click: Save as …", p.display()),
                                None => tr!("Projekt speichern: Auswahl, Schnitte, Kapitel und Kompression (Strg+S)", "Save project: selection, cuts, chapters and compression (Ctrl+S)").to_string(),
                            };
                            let r = style::icon_button(ui, &p, ic::FLOPPY_DISK, &save_tip);
                            if r.clicked() {
                                self.save_project(false);
                            }
                            r.context_menu(|ui| {
                                if ui.button(trf!("{}  Speichern unter …", "{}  Save as …", ic::FLOPPY_DISK)).clicked() {
                                    self.save_project(true);
                                    ui.close();
                                }
                            });
                            if style::icon_button(ui, &p, ic::FOLDER_OPEN, tr!("Andere DVD öffnen", "Open another DVD")).clicked() {
                                self.disc = None;
                                self.preview.stop();
                                self.sel = Selection::None;
                                self.project_path = None;
                            }
                        }
                        if let Some((text, _)) = &self.notice {
                            ui.label(RichText::new(format!("{}  {text}", ic::CHECK_CIRCLE)).size(13.0).color(p.good));
                        }
                    });
                });
            });
    }

    fn default_selection(&self) -> Selection {
        let Some(d) = &self.disc else { return Selection::None };
        d.titles
            .iter()
            .position(|t| t.category == Category::MainMovie)
            .map(|i| Selection::Item(ItemId::Title(i)))
            .unwrap_or(Selection::None)
    }

    pub(super) fn can_start_backup(&self) -> bool {
        let Some(_) = &self.disc else { return false };
        match self.plan.mode {
            Mode::FullDisc => true,
            Mode::Reauthor => !self.plan.reauthor.is_empty(),
        }
    }

    // ------------------------------------------------------------------ Startseite

    pub(super) fn view_start(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.pal;
        let drives: Vec<_> = self.drives().to_vec();
        let area = ui.available_rect_before_wrap();
        let content_w = 920.0f32.min(area.width() - 40.0);
        ui.scope_builder(
            egui::UiBuilder::new().max_rect(Rect::from_center_size(area.center(), Vec2::new(content_w, area.height()))),
            |ui| {
                ui.vertical_centered(|ui| {
                    ui.add_space((area.height() * 0.09).clamp(16.0, 80.0));
                    ui.label(RichText::new(ic::DISC).size(66.0).color(p.accent));
                    ui.add_space(4.0);
                    ui.label(RichText::new(tr!("DVD öffnen und passend verkleinern", "Open a DVD and shrink it to fit")).family(semibold()).size(26.0));
                    ui.label(
                        RichText::new(tr!("Hauptfilm behalten, Überflüssiges abwählen, und alles passt auf einen Rohling.", "Keep the main movie, deselect what you don't need, and everything fits on a blank disc."))
                            .size(15.0)
                            .color(p.muted),
                    );
                    ui.add_space(26.0);
                });

                let gap = 12.0;
                let w = (content_w - gap * 3.0) / 4.0;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = gap;
                    let has_drive = drives.iter().find(|d| d.2).cloned();
                    let drive_text = match (&has_drive, drives.is_empty()) {
                        (Some(d), _) => trf!("Video-DVD in Laufwerk {}", "Video DVD in drive {}", d.0),
                        (None, true) => tr!("Kein DVD-Laufwerk gefunden", "No DVD drive found").to_string(),
                        (None, false) => tr!("Keine Video-DVD eingelegt", "No video DVD inserted").to_string(),
                    };
                    if start_card(ui, &p, w, ic::DISC, tr!("Laufwerk", "Drive"), &drive_text, has_drive.is_some()) {
                        if let Some(d) = has_drive {
                            self.open_path(ctx, d.1);
                        }
                    }
                    if start_card(ui, &p, w, ic::FOLDER_OPEN, tr!("Ordner", "Folder"), tr!("VIDEO_TS-Ordner auf der Festplatte", "VIDEO_TS folder on the hard disk"), true) {
                        self.pick_folder(ctx);
                    }
                    if start_card(ui, &p, w, ic::FILE_ARROW_DOWN, tr!("ISO-Abbild", "ISO image"), tr!("Abbilddatei einer DVD", "Image file of a DVD"), true) {
                        self.pick_iso(ctx);
                    }
                    if start_card(ui, &p, w, ic::FILE_VIDEO, tr!("VOB-/MPEG-Datei", "VOB/MPEG file"), tr!("Einzelne Videodatei schneiden und ausgeben", "Cut and write a single video file"), true) {
                        self.pick_media(ctx);
                    }
                });

                ui.add_space(18.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(tr!("Oder ziehe einen Ordner, eine ISO-, VOB- oder MPEG-Datei oder ein Projekt einfach hierher.", "Or simply drag a folder, an ISO, VOB or MPEG file or a project here.")).color(p.muted));
                    ui.add_space(10.0);
                    // Drei Knöpfe nebeneinander, mittig
                    let labels = [
                        trf!("{}  Projekt öffnen", "{}  Open project", ic::FLOPPY_DISK),
                        trf!("{}  Mehrere DVDs nacheinander", "{}  Several DVDs in a row", ic::QUEUE),
                        trf!("{}  Beispiel-DVD ausprobieren", "{}  Try a sample DVD", ic::SPARKLE),
                    ];
                    let font = egui::FontId::proportional(14.0);
                    let widths: f32 = labels
                        .iter()
                        .map(|l| ui.painter().layout_no_wrap(l.clone(), font.clone(), p.text).size().x + 2.0 * ui.spacing().button_padding.x)
                        .sum();
                    let row_w = widths + 2.0 * 8.0;
                    ui.horizontal(|ui| {
                        ui.add_space(((ui.available_width() - row_w) / 2.0).max(0.0));
                        ui.spacing_mut().item_spacing.x = 8.0;
                        if soft_button(ui, &p, &labels[0]).on_hover_text(tr!("Eine gespeicherte Projektdatei (.dvdproj) laden", "Load a saved project file (.dvdproj)")).clicked() {
                            self.pick_project(ctx);
                        }
                        if soft_button(ui, &p, &labels[1]).on_hover_text(tr!("Warteschlange: mehrere DVDs, ISO-Dateien oder Projekte nacheinander sichern", "Queue: back up several DVDs, ISO files or projects one after another")).clicked() {
                            self.queue_open = true;
                        }
                        if soft_button(ui, &p, &labels[2]).clicked() {
                            self.open_sample(ctx);
                        }
                    });
                    ui.add_space(14.0);
                    ui.label(
                        RichText::new(trf!(
                            "{}  Kopiergeschützte Kauf-DVDs werden erkannt und nicht verarbeitet.", "{}  Copy-protected commercial DVDs are detected and not processed.",
                            ic::INFO
                        ))
                        .size(12.5)
                        .color(p.faint),
                    );
                });
            },
        );
    }

    // ------------------------------------------------------------------ Arbeitsfläche

    pub(super) fn view_workspace(&mut self, ui: &mut Ui, ctx: &egui::Context) {
        let p = self.pal;
        let Some(disc) = self.disc.clone() else { return };
        let est = self.plan.estimate(&disc);
        self.sync_preview();

        let _ = ctx;
        egui::Panel::bottom("kapazitaet")
            .frame(egui::Frame::new().fill(p.panel).stroke(Stroke::new(1.0, p.line)).inner_margin(Margin::symmetric(16, 12)))
            .resizable(false)
            .show(ui, |ui| self.view_capacity(ui, &est));

        let body = egui::Frame::new().fill(p.bg).inner_margin(Margin::same(GAP as i8));
        egui::CentralPanel::default().frame(body).show(ui, |ui| {
            ui.spacing_mut().item_spacing = Vec2::splat(GAP);
            let full = ui.available_rect_before_wrap();
            let left_w = (full.width() * 0.29).clamp(290.0, 400.0);
            let right_w = (full.width() * 0.27).clamp(290.0, 360.0);
            let left = Rect::from_min_size(full.min, Vec2::new(left_w, full.height()));
            let right = Rect::from_min_size(egui::pos2(full.right() - right_w, full.top()), Vec2::new(right_w, full.height()));
            let mid = Rect::from_min_max(egui::pos2(left.right() + GAP, full.top()), egui::pos2(right.left() - GAP, full.bottom()));

            ui.scope_builder(egui::UiBuilder::new().max_rect(left), |ui| {
                card_filled(ui, &p, |ui| self.view_contents(ui, &disc, &est));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(mid), |ui| {
                card_filled(ui, &p, |ui| self.view_preview(ui));
            });
            ui.scope_builder(egui::UiBuilder::new().max_rect(right), |ui| {
                card_filled(ui, &p, |ui| self.view_settings(ui, &disc, &est));
            });
        });
    }

    fn item_stats(&self, disc: &Disc, id: ItemId) -> CellStats {
        match id {
            ItemId::Title(i) => disc.titles.get(i).map(|t| disc.title_stats(t)).unwrap_or_default(),
            ItemId::Menu(v) => disc.menu_stats(v),
            ItemId::Unused(v) => {
                disc.unused.iter().find(|u| u.vts == v).map(|u| disc.unused_stats(u)).unwrap_or_default()
            }
        }
    }

    fn view_contents(&mut self, ui: &mut Ui, disc: &Disc, est: &Estimate) {
        let p = self.pal;
        ui.horizontal(|ui| {
            section(ui, &p, ic::DISC, &disc.source.label.to_uppercase());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let info = disc.vts.values().next().map(|v| v.video.describe()).unwrap_or_default();
                ui.label(RichText::new(info).size(12.0).color(p.faint));
            });
        });
        ui.separator();

        if self.plan.mode == Mode::Reauthor {
            self.view_reauthor_list(ui, disc, est);
            return;
        }

        ScrollArea::vertical().id_salt("inhalt").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            let titles: Vec<(usize, Category)> = disc.titles.iter().enumerate().map(|(i, t)| (i, t.category)).collect();
            let row = |this: &mut Self, ui: &mut Ui, id: ItemId, icon: &str, color: Color32, title: String, sub: String| {
                let stats = this.item_stats(disc, id);
                let s = this.plan.settings(id);
                let out = this.plan.out_stats_for_item(&stats, &s, est.auto_ratio);
                let selected = this.sel == Selection::Item(id);
                let r = item_row(
                    ui,
                    &p,
                    format!("{id:?}"),
                    selected,
                    icon,
                    color,
                    &title,
                    &sub,
                    &fmt_size(out),
                    &trf!("von {}", "of {}", fmt_size(stats.total)),
                );
                if r.clicked() && !selected {
                    this.sel = Selection::Item(id);
                    this.preview.dirty = true;
                }
                if let ItemId::Title(ti) = id {
                    let chapters = disc.titles.get(ti).map(|t| t.chapters.len()).unwrap_or(0);
                    r.context_menu(|ui| {
                        if ui
                            .add_enabled(chapters > 1, egui::Button::new(trf!("{}  In {chapters} Einzeltitel zerlegen", "{}  Split into {chapters} separate titles", ic::LIST_BULLETS)))
                            .on_hover_text(tr!("Legt für jedes Kapitel einen eigenen Eintrag in der neuen DVD an", "Creates a separate entry in the new DVD for each chapter"))
                            .clicked()
                        {
                            let at = this.plan.reauthor.len();
                            let settings = this.plan.settings(id);
                            this.split_chapters(disc, ti, 0, chapters.saturating_sub(1), settings, at);
                            ui.close();
                        }
                    });
                }
            };

            group_label(ui, &p, tr!("Hauptfilm", "Main movie"));
            for (i, cat) in &titles {
                if *cat == Category::MainMovie {
                    let t = &disc.titles[*i];
                    let sub = format!("{} · {}", fmt_time(t.ms), discslim::i18n::chapters(t.chapters.len()));
                    row(self, ui, ItemId::Title(*i), ic::FILM_STRIP, p.accent, trf!("Titel {}", "Title {}", t.number), sub);
                }
            }
            if titles.iter().any(|t| t.1 == Category::Extra) {
                group_label(ui, &p, tr!("Extras", "Extras"));
                for (i, cat) in &titles {
                    if *cat == Category::Extra {
                        let t = &disc.titles[*i];
                        let sub = format!("{} · {}", fmt_time(t.ms), discslim::i18n::chapters(t.chapters.len()));
                        row(self, ui, ItemId::Title(*i), ic::STACK, p.audio, trf!("Titel {}", "Title {}", t.number), sub);
                    }
                }
            }
            if !disc.menus.is_empty() {
                group_label(ui, &p, tr!("Menüs", "Menus"));
                for m in &disc.menus {
                    let name = if m.vts == 0 { tr!("Hauptmenü", "Main menu").to_string() } else { trf!("Menü von Titelset {}", "Menu of title set {}", m.vts) };
                    let pages = if m.pgc_count == 1 { tr!("1 Seite", "1 page").to_string() } else { trf!("{} Seiten", "{} pages", m.pgc_count) };
                    let sub = if m.ms > 0 { format!("{} · {pages}", fmt_time(m.ms)) } else { pages };
                    row(self, ui, ItemId::Menu(m.vts), ic::LIST_BULLETS, p.subs, name, sub);
                }
            }
            if !disc.unused.is_empty() {
                group_label(ui, &p, tr!("Ungenutzt", "Unused"));
                for u in &disc.unused {
                    let sub = discslim::i18n::count(u.cells.len(), "Abschnitt ohne Verweis", "Abschnitte ohne Verweis", "unreferenced section", "unreferenced sections");
                    row(self, ui, ItemId::Unused(u.vts), ic::TRASH, p.other, trf!("Titelset {}", "Title set {}", u.vts), sub);
                }
            }
        });
    }

    fn view_reauthor_list(&mut self, ui: &mut Ui, disc: &Disc, _est: &Estimate) {
        let p = self.pal;
        let full_h = ui.available_height();
        let new_h = (full_h * 0.55).max(120.0);
        ui.horizontal(|ui| {
            group_label(ui, &p, tr!("Neue DVD", "New DVD"));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let ok = !self.plan.reauthor.is_empty();
                let b = egui::Button::new(RichText::new(trf!("{}  Alle Schnitte abspielen", "{}  Play all cuts", ic::PLAY)).size(12.5));
                if ui.add_enabled(ok, b).on_hover_text(tr!("Spielt alle Einträge nacheinander so ab, wie sie ausgegeben werden", "Plays all entries one after another as they will be written")).clicked() {
                    self.update_program();
                    self.preview.program = true;
                    self.preview.prog_cur = 0;
                    self.preview.prog_seek = self.preview.prog.first().map(|c| c.1);
                    self.preview.prog_play = Some(true);
                }
            });
        });
        let mut remove = None;
        let mut swap = None;
        let mut dup = None;
        let mut split = None;
        let mut listen = None;
        let mut undo = false;
        ScrollArea::vertical().id_salt("neu").max_height(new_h).auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            if self.plan.reauthor.is_empty() {
                ui.add_space(16.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(ic::LIST_PLUS).size(30.0).color(p.faint));
                    ui.label(RichText::new(tr!("Füge unten Titel hinzu. Die Reihenfolge hier ist die Reihenfolge auf der neuen DVD.", "Add titles below. The order here is the order on the new DVD.")).size(13.0).color(p.muted));
                });
            }
            let n = self.plan.reauthor.len();
            for i in 0..n {
                let e = self.plan.reauthor[i].clone();
                let Some(t) = disc.titles.get(e.title) else { continue };
                let chapters = if e.first_chapter == 0 && e.last_chapter + 1 >= t.chapters.len() {
                    if t.chapters.len() == 1 { tr!("1 Kapitel", "1 chapter").to_string() } else { trf!("Alle {} Kapitel", "All {} chapters", t.chapters.len()) }
                } else if e.first_chapter == e.last_chapter {
                    trf!("Kapitel {}", "Chapter {}", e.first_chapter + 1)
                } else {
                    trf!("Kapitel {} bis {}", "Chapters {} to {}", e.first_chapter + 1, e.last_chapter + 1)
                };
                let mut size = 0.0;
                for k in reauthor_cells(disc, &e) {
                    size += disc.stats_for(&k).total;
                }
                // Geschnittene Einträge zeigen nur, was übrig bleibt.
                size *= entry_kept_fraction(disc, &e);
                let total_ms = entry_duration_ms(disc, &e);
                let chapters = if e.cut.is_trim(total_ms) {
                    let total = total_ms;
                    let len = e.cut.end().min(total).saturating_sub(e.cut.start());
                    trf!("{} {} geschnitten", "{} {} cut", fmt_time(len), ic::SCISSORS)
                } else {
                    chapters
                };
                // Eigene Kapitelmarken: Anzahl der neuen Kapitel anhängen
                let chapters = if e.chapter_marks.is_some() {
                    trf!("{chapters} · {} {} Kap.", "{chapters} · {} {} ch.", ic::BOOKMARK_SIMPLE, e.chapter_count(disc))
                } else {
                    chapters
                };
                // Bei "Alle Schnitte" folgt die Auswahl dem laufenden Eintrag (siehe sync_preview)
                let current = self.sel == Selection::Entry(i);
                let selected = current || self.marked.contains(&i);
                let r = self.entry_row(
                    ui,
                    disc,
                    i,
                    &e,
                    selected,
                    &trf!("{}. Titel {}", "{}. Title {}", i + 1, t.number),
                    &chapters,
                    &fmt_size(size),
                    mode_label(e.settings.video),
                );
                if r.clicked() {
                    let mods = ui.input(|inp| inp.modifiers);
                    self.click_entry(i, mods.command, mods.shift);
                }
                r.context_menu(|ui| {
                    if ui.button(trf!("{}  Nach oben", "{}  Move up", ic::ARROW_UP)).clicked() {
                        swap = Some((i, i.wrapping_sub(1)));
                        ui.close();
                    }
                    if ui.button(trf!("{}  Nach unten", "{}  Move down", ic::ARROW_DOWN)).clicked() {
                        swap = Some((i, i + 1));
                        ui.close();
                    }
                    if ui.button(trf!("{}  Kopieren (mit Schnitt)", "{}  Duplicate (with cut)", ic::COPY)).clicked() {
                        dup = Some(i);
                        ui.close();
                    }
                    if ui
                        .add_enabled(i + 1 < n, egui::Button::new(trf!("{}  Übergang zum nächsten anhören", "{}  Listen to the transition to the next", ic::EAR)))
                        .on_hover_text(tr!("Spielt das Ende dieses und den Anfang des nächsten Eintrags, so wie sie ausgegeben werden", "Plays the end of this entry and the start of the next one, as they will be written"))
                        .clicked()
                    {
                        listen = Some(i);
                        ui.close();
                    }
                    let n = e.last_chapter + 1 - e.first_chapter.min(e.last_chapter);
                    if ui
                        .add_enabled(n > 1, egui::Button::new(trf!("{}  In {n} Einzeltitel zerlegen", "{}  Split into {n} separate titles", ic::LIST_BULLETS)))
                        .on_hover_text(tr!("Ersetzt diesen Eintrag durch je einen Eintrag pro Kapitel", "Replaces this entry with one entry per chapter"))
                        .clicked()
                    {
                        split = Some(i);
                        ui.close();
                    }
                    // Gehört der Eintrag zur Mehrfachauswahl, gilt Entfernen für alle markierten.
                    let group = self.marked.len() > 1 && self.marked.contains(&i);
                    let label = if group {
                        trf!("{}  {} entfernen", "{}  Remove {}", ic::TRASH, discslim::i18n::count(self.marked.len(), "Eintrag", "Einträge", "entry", "entries"))
                    } else {
                        trf!("{}  Entfernen", "{}  Remove", ic::TRASH)
                    };
                    if ui.button(label).on_hover_text(tr!("Entf-Taste; Strg+Z holt den Eintrag zurück", "Delete key; Ctrl+Z brings it back")).clicked() {
                        remove = Some(if group { self.marked.iter().copied().collect() } else { vec![i] });
                        ui.close();
                    }
                });
            }
        });
        if self.undo_remove.is_some() && !matches!(self.sel, Selection::Entry(_)) {
            ui.horizontal(|ui| {
                let n = self.undo_remove.as_ref().map_or(0, |v| v.len());
                let tip = trf!("Entfernen rückgängig machen: {} (Strg+Z)", "Undo remove: {} (Ctrl+Z)", discslim::i18n::count(n, "Eintrag", "Einträge", "entry", "entries"));
                if style::icon_button(ui, &p, ic::ARROW_COUNTER_CLOCKWISE, &tip).clicked() {
                    undo = true;
                }
            });
        }
        if let Selection::Entry(i) = self.sel {
            ui.horizontal(|ui| {
                if style::icon_button(ui, &p, ic::ARROW_UP, tr!("Nach oben", "Move up")).clicked() && i > 0 {
                    swap = Some((i, i - 1));
                }
                if style::icon_button(ui, &p, ic::ARROW_DOWN, tr!("Nach unten", "Move down")).clicked() {
                    swap = Some((i, i + 1));
                }
                if style::icon_button(ui, &p, ic::COPY, tr!("Eintrag kopieren, samt Schnitt und Einstellungen", "Duplicate entry, including cut and settings")).clicked() {
                    dup = Some(i);
                }
                if i + 1 < self.plan.reauthor.len()
                    && style::icon_button(ui, &p, ic::EAR, tr!("Übergang zum nächsten Eintrag anhören (je 2,5 s davor und danach)", "Listen to the transition to the next entry (2.5 s before and after)")).clicked()
                {
                    listen = Some(i);
                }
                let chosen = self.chosen_entries();
                let tip = if chosen.len() > 1 {
                    trf!("{} entfernen (Entf)", "Remove {} (Delete)", discslim::i18n::count(chosen.len(), "Eintrag", "Einträge", "entry", "entries"))
                } else {
                    tr!("Entfernen (Entf)", "Remove (Delete)").to_string()
                };
                if style::icon_button(ui, &p, ic::TRASH, &tip).clicked() {
                    remove = Some(if chosen.is_empty() { vec![i] } else { chosen });
                }
                if let Some(v) = &self.undo_remove {
                    let tip = trf!("Entfernen rückgängig machen: {} (Strg+Z)", "Undo remove: {} (Ctrl+Z)", discslim::i18n::count(v.len(), "Eintrag", "Einträge", "entry", "entries"));
                    if style::icon_button(ui, &p, ic::ARROW_COUNTER_CLOCKWISE, &tip).clicked() {
                        undo = true;
                    }
                }
                if self.marked.len() > 1 {
                    ui.label(RichText::new(trf!("{} gewählt", "{} selected", self.marked.len())).size(12.5).color(p.muted));
                }
            });
        }
        if let Some(i) = listen {
            self.listen_junction(i);
        }
        if undo {
            self.undo_last_remove();
        }
        // Andere Änderungen an der Liste verschieben die Stellen: Mehrfachauswahl aufheben
        if split.is_some() || dup.is_some() || swap.is_some() {
            self.marked.clear();
            self.mark_anchor = None;
        }
        if let Some(i) = split {
            if i < self.plan.reauthor.len() {
                let e = self.plan.reauthor.remove(i);
                self.split_chapters(disc, e.title, e.first_chapter, e.last_chapter, e.settings, i);
            }
        }
        if let Some(i) = dup {
            if let Some(e) = self.plan.reauthor.get(i).cloned() {
                self.plan.reauthor.insert(i + 1, e);
                self.sel = Selection::Entry(i + 1);
                self.preview.dirty = true;
            }
        }
        if let Some((a, b)) = swap {
            if a < self.plan.reauthor.len() && b < self.plan.reauthor.len() {
                self.plan.reauthor.swap(a, b);
                self.sel = Selection::Entry(b);
                self.preview.dirty = true;
            }
        }
        if let Some(v) = remove {
            self.remove_entries(v);
        }

        ui.separator();
        let heading = if self.is_single_file() { tr!("Inhalt der Datei", "Contents of the file") } else { tr!("Titel dieser DVD", "Titles on this DVD") };
        group_label(ui, &p, heading);
        ScrollArea::vertical().id_salt("quelle").auto_shrink([false, false]).show(ui, |ui| {
            ui.spacing_mut().item_spacing.y = 4.0;
            for (idx, t) in disc.titles.iter().enumerate() {
                let sub = format!("{} · {}", fmt_time(t.ms), discslim::i18n::chapters(t.chapters.len()));
                let stats = disc.title_stats(t);
                let mut add = false;
                let mut split_src: Option<usize> = None;
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 4.0;
                    let b = egui::Button::new(RichText::new(ic::PLUS).size(18.0).color(p.on_accent))
                        .fill(p.good)
                        .stroke(Stroke::NONE)
                        .corner_radius(CornerRadius::same(8))
                        .min_size(Vec2::new(34.0, 34.0));
                    if ui.add(b).on_hover_text(tr!("Zur neuen DVD hinzufügen", "Add to the new DVD")).clicked() {
                        add = true;
                    }
                    let selected = self.sel == Selection::Source(idx);
                    let r = item_row(
                        ui,
                        &p,
                        ("src", idx),
                        selected,
                        ic::FILM_STRIP,
                        p.muted,
                        &trf!("Titel {}", "Title {}", t.number),
                        &sub,
                        &fmt_size(stats.total),
                        tr!("ansehen", "view"),
                    );
                    if r.clicked() && !selected {
                        self.sel = Selection::Source(idx);
                        self.preview.dirty = true;
                    }
                    let chapters = t.chapters.len();
                    r.context_menu(|ui| {
                        if ui.button(trf!("{}  Zur neuen DVD hinzufügen", "{}  Add to the new DVD", ic::PLUS)).clicked() {
                            add = true;
                            ui.close();
                        }
                        if ui
                            .add_enabled(chapters > 1, egui::Button::new(trf!("{}  In {chapters} Einzeltitel zerlegen", "{}  Split into {chapters} separate titles", ic::LIST_BULLETS)))
                            .on_hover_text(tr!("Fügt jedes Kapitel als eigenen Eintrag hinzu", "Adds each chapter as a separate entry"))
                            .clicked()
                        {
                            split_src = Some(idx);
                            ui.close();
                        }
                    });
                });
                if let Some(ti) = split_src.take() {
                    let at = self.plan.reauthor.len();
                    let settings = self.plan.items.get(&ItemId::Title(ti)).cloned().unwrap_or_else(|| self.plan.settings(ItemId::Title(ti)));
                    let last = disc.titles.get(ti).map(|t| t.chapters.len().saturating_sub(1)).unwrap_or(0);
                    self.split_chapters(disc, ti, 0, last, settings, at);
                }
                if add {
                    self.add_entry(disc, idx);
                }
            }
        });
    }

    /// Listenzeile eines Eintrags mit Vorschaubildern von Schnittanfang und -ende.
    /// Steht die Maus über einem Bild, laufen die ersten bzw. letzten 5 Sekunden als Schleife.
    #[allow(clippy::too_many_arguments)]
    fn entry_row(
        &mut self,
        ui: &mut Ui,
        disc: &Disc,
        i: usize,
        e: &ReauthorEntry,
        selected: bool,
        title: &str,
        subtitle: &str,
        right_top: &str,
        right_bottom: &str,
    ) -> egui::Response {
        let p = self.pal;
        let width = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(width, 54.0), Sense::hover());
        let resp = ui.interact(rect, ui.scope_id().with(("eintrag", i)), Sense::click());
        let painter = ui.painter_at(rect);
        let radius = CornerRadius::same(10);
        if selected {
            painter.rect_filled(rect, radius, p.accent_soft);
            painter.rect_stroke(rect, radius, Stroke::new(1.0, p.accent), StrokeKind::Inside);
        } else if resp.hovered() {
            painter.rect_filled(rect, radius, p.hover);
        }

        // Zeiten der beiden Bilder
        let total = entry_duration_ms(disc, e);
        let (s, end) = e.cut.effective(total);
        let end = end.min(total);
        let end_frame = end.saturating_sub(40).max(s);
        let seg = || discslim::engine::preview::title_segments(disc, e.title, e.first_chapter, e.last_chapter);
        let hover_pos = ui.ctx().pointer_hover_pos();
        let tw = 58.0;
        let th = 34.0;
        for (k, at) in [(0u8, s), (1u8, end_frame)] {
            let x = rect.left() + 8.0 + f32::from(k) * (tw + 4.0);
            let r = Rect::from_min_size(egui::pos2(x, rect.center().y - th / 2.0), Vec2::new(tw, th));
            painter.rect_filled(r, CornerRadius::same(4), p.screen);
            let hovered = hover_pos.is_some_and(|h| r.contains(h)) && resp.hovered();
            let mut tex = None;
            if hovered {
                let range = if k == 0 {
                    (s as f64, (s as f64 + 5000.0).min(end as f64))
                } else {
                    ((end as f64 - 5000.0).max(s as f64), end as f64)
                };
                let restart = self.hover_loop.as_ref().map(|l| l.key != (i, k)).unwrap_or(true);
                if restart {
                    self.hover_loop = Some(super::thumbs::HoverLoop::start((i, k), disc.source.clone(), seg(), range));
                }
                if let Some(l) = self.hover_loop.as_mut() {
                    l.seen_this_frame = true;
                    tex = l.texture();
                }
            }
            if tex.is_none() {
                if let Some(th) = self.thumbs.as_mut() {
                    tex = th.get((e.title, e.first_chapter, e.last_chapter, at), &disc.source, seg);
                }
            }
            if let Some(t) = tex {
                painter.image(t, r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
            }
            if hovered {
                painter.rect_stroke(r, CornerRadius::same(4), Stroke::new(1.5, p.good), StrokeKind::Inside);
            }
        }

        let tx = rect.left() + 8.0 + 2.0 * (tw + 4.0) + 6.0;
        let right_w = 76.0;
        let text_w = (rect.right() - tx - right_w - 8.0).max(30.0);
        let clip = Rect::from_min_size(egui::pos2(tx, rect.top()), Vec2::new(text_w, rect.height()));
        let tp = ui.painter_at(clip);
        tp.text(egui::pos2(tx, rect.center().y - 10.0), Align2::LEFT_CENTER, title, FontId::new(14.0, semibold()), p.text);
        tp.text(egui::pos2(tx, rect.center().y + 10.0), Align2::LEFT_CENTER, subtitle, FontId::proportional(12.0), p.muted);
        let rx = rect.right() - 10.0;
        painter.text(egui::pos2(rx, rect.center().y - 10.0), Align2::RIGHT_CENTER, right_top, FontId::new(13.0, semibold()), p.text);
        painter.text(egui::pos2(rx, rect.center().y + 10.0), Align2::RIGHT_CENTER, right_bottom, FontId::proportional(12.0), p.muted);
        if resp.hovered() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
        }
        resp
    }

    /// Legt für jedes Kapitel im Bereich einen eigenen Eintrag an (ab Position t).
    /// Klick auf Eintrag `i` der Liste „Neue DVD“, mit Strg (`toggle`) oder Umschalt (`range`).
    pub(super) fn click_entry(&mut self, i: usize, toggle: bool, range: bool) {
        let current = self.sel == Selection::Entry(i);
        let cur = match self.sel {
            Selection::Entry(c) => Some(c),
            _ => None,
        };
        let mut go = None;
        if toggle {
            // Strg+Klick: Eintrag dazu- oder abwählen; er wird neuer Ausgangspunkt
            if self.marked.is_empty() {
                self.marked.extend(cur);
            }
            self.mark_anchor = Some(i);
            if self.marked.insert(i) {
                go = Some(i);
            } else {
                self.marked.remove(&i);
                if current {
                    go = self.marked.iter().next().copied();
                }
            }
        } else if range {
            // Umschalt+Klick: Bereich vom Ausgangspunkt (letzter Klick ohne Umschalt) bis
            // hier; weitere Umschalt+Klicks ändern nur das Ende, wie im Explorer
            let n = self.plan.reauthor.len();
            let a = self.mark_anchor.filter(|a| *a < n).or(cur).unwrap_or(i);
            self.mark_anchor = Some(a);
            self.marked = (a.min(i)..=a.max(i)).collect();
            go = Some(i);
        } else {
            self.mark_anchor = Some(i);
            self.marked.clear();
            if !current {
                go = Some(i);
            }
        }
        // Nur noch einer markiert: das ist einfach die normale Auswahl
        if self.marked.len() == 1 {
            go = self.marked.iter().next().copied();
            self.marked.clear();
        }
        if let Some(g) = go.filter(|g| self.sel != Selection::Entry(*g)) {
            self.sel = Selection::Entry(g);
            self.preview.dirty = true;
            // Bei "Alle Schnitte" springt die Wiedergabe zu diesem Eintrag
            if self.preview.program {
                if let Some(c) = self.preview.prog.get(g).copied() {
                    self.preview.prog_goto(g, c.1);
                }
            }
        }
    }

    /// Einträge, die eine Aktion betrifft: die markierten, sonst der gewählte.
    pub(super) fn chosen_entries(&self) -> Vec<usize> {
        let n = self.plan.reauthor.len();
        if self.marked.len() > 1 {
            return self.marked.iter().copied().filter(|i| *i < n).collect();
        }
        match self.sel {
            Selection::Entry(i) if i < n => vec![i],
            _ => Vec::new(),
        }
    }

    /// Entfernt Einträge und merkt sie sich für „Rückgängig“.
    pub(super) fn remove_entries(&mut self, mut idx: Vec<usize>) {
        idx.sort_unstable();
        idx.dedup();
        idx.retain(|i| *i < self.plan.reauthor.len());
        let Some(&first) = idx.first() else { return };
        let mut removed = Vec::with_capacity(idx.len());
        for &i in idx.iter().rev() {
            removed.push((i, self.plan.reauthor.remove(i)));
        }
        removed.reverse();
        let n = removed.len();
        self.undo_remove = Some(removed);
        self.marked.clear();
        self.mark_anchor = None;
        self.sel = if self.plan.reauthor.is_empty() { Selection::None } else { Selection::Entry(first.min(self.plan.reauthor.len() - 1)) };
        self.preview.dirty = true;
        self.notice = Some((
            trf!("{} entfernt · Strg+Z macht es rückgängig", "{} removed · Ctrl+Z undoes it", discslim::i18n::count(n, "Eintrag", "Einträge", "entry", "entries")),
            std::time::Instant::now(),
        ));
    }

    /// Holt die zuletzt entfernten Einträge an ihre alte Stelle zurück.
    pub(super) fn undo_last_remove(&mut self) {
        let Some(removed) = self.undo_remove.take() else { return };
        self.marked.clear();
        self.mark_anchor = None;
        let mut first = None;
        for (i, e) in removed {
            let at = i.min(self.plan.reauthor.len());
            self.plan.reauthor.insert(at, e);
            first.get_or_insert(at);
            self.marked.insert(at);
        }
        if self.marked.len() < 2 {
            self.marked.clear();
        }
        let n = self.marked.len().max(1);
        if let Some(f) = first {
            self.plan.mode = Mode::Reauthor;
            self.sel = Selection::Entry(f);
        }
        self.preview.dirty = true;
        self.notice = Some((
            trf!("{} wiederhergestellt", "{} restored", discslim::i18n::count(n, "Eintrag", "Einträge", "entry", "entries")),
            std::time::Instant::now(),
        ));
    }

    pub(super) fn split_chapters(&mut self, disc: &Disc, title: usize, first: usize, last: usize, settings: discslim::engine::plan::ItemSettings, at: usize) {
        let at = at.min(self.plan.reauthor.len());
        for (k, ch) in (first..=last.max(first)).enumerate() {
            let mut entry = ReauthorEntry::new(title, ch, ch, settings.clone());
            let total = entry_duration_ms(disc, &entry);
            entry.cut = Cut::whole(total, self.prefs.cut_exact);
            self.plan.reauthor.insert(at + k, entry);
        }
        self.plan.mode = Mode::Reauthor;
        self.sel = Selection::Entry(at);
        self.preview.dirty = true;
    }

    /// Fügt einen Titel als neuen Eintrag hinzu (nur über den +-Knopf).
    fn add_entry(&mut self, disc: &Disc, idx: usize) {
        let Some(t) = disc.titles.get(idx) else { return };
        let settings = self.plan.items.get(&ItemId::Title(idx)).cloned().unwrap_or_default();
        let last = t.chapters.len().saturating_sub(1);
        let mut entry = ReauthorEntry::new(idx, 0, last, settings);
        let total = entry_duration_ms(disc, &entry);
        entry.cut = Cut::whole(total, self.prefs.cut_exact);
        self.plan.reauthor.push(entry);
        self.marked.clear();
        self.mark_anchor = None;
        self.sel = Selection::Entry(self.plan.reauthor.len() - 1);
        self.preview.dirty = true;
    }
    fn view_settings(&mut self, ui: &mut Ui, disc: &Disc, est: &Estimate) {
        let p = self.pal;
        let sel = self.sel;
        let (mut settings, stats, tracks_title, title_text) = match sel {
            Selection::Item(id) => {
                let stats = self.item_stats(disc, id);
                let (title_idx, text) = match id {
                    ItemId::Title(i) => {
                        let t = &disc.titles[i];
                        let name = if t.category == Category::MainMovie { tr!("Hauptfilm", "Main movie") } else { tr!("Extra", "Extra") };
                        (Some(i), trf!("{name} · Titel {}", "{name} · Title {}", t.number))
                    }
                    ItemId::Menu(0) => (None, tr!("Hauptmenü", "Main menu").to_string()),
                    ItemId::Menu(v) => (None, trf!("Menü von Titelset {v}", "Menu of title set {v}")),
                    ItemId::Unused(v) => (None, trf!("Ungenutzt in Titelset {v}", "Unused in title set {v}")),
                };
                (self.plan.settings(id), stats, title_idx, text)
            }
            Selection::Entry(i) => {
                let Some(e) = self.plan.reauthor.get(i) else { return };
                let mut stats = CellStats::default();
                for k in reauthor_cells(disc, e) {
                    stats.add(&disc.stats_for(&k));
                }
                stats.scale(entry_kept_fraction(disc, e));
                let t = &disc.titles[e.title];
                (e.settings.clone(), stats, Some(e.title), trf!("Eintrag {} · Titel {}", "Entry {} · Title {}", i + 1, t.number))
            }
            Selection::Source(idx) => {
                let Some(t) = disc.titles.get(idx) else { return };
                ui.label(RichText::new(trf!("Titel {}", "Title {}", t.number)).family(semibold()).size(15.0));
                ui.label(
                    RichText::new(format!("{} · {} · {}", fmt_time(t.ms), discslim::i18n::chapters(t.chapters.len()), fmt_size(disc.title_stats(t).total)))
                        .color(p.muted),
                );
                ui.add_space(8.0);
                ui.label(
                    RichText::new(tr!("Die Vorschau zeigt diesen Titel. In die neue DVD kommt er erst über den Plus-Knopf.", "The preview shows this title. It only goes into the new DVD via the plus button."))
                        .size(13.0)
                        .color(p.muted),
                );
                ui.add_space(8.0);
                if primary_button(ui, &p, &trf!("{}  Zur neuen DVD hinzufügen", "{}  Add to the new DVD", ic::PLUS), true).clicked() {
                    self.add_entry(disc, idx);
                }
                return;
            }
            Selection::None => {
                ui.add_space(30.0);
                ui.vertical_centered(|ui| {
                    ui.label(RichText::new(ic::SLIDERS_HORIZONTAL).size(34.0).color(p.faint));
                    ui.label(RichText::new(tr!("Wähle links etwas aus, um die Einstellungen zu sehen.", "Select something on the left to see its settings.")).color(p.muted));
                });
                return;
            }
        };

        ScrollArea::vertical().id_salt("einstellungen").auto_shrink([false, false]).show(ui, |ui| {
            let width = ui.available_width() - 2.0;
            ui.label(RichText::new(title_text).family(semibold()).size(15.0));
            ui.add_space(2.0);

            // Kapitelauswahl beim Neu-Zusammenstellen
            if let Selection::Entry(i) = sel {
                let t = &disc.titles[self.plan.reauthor[i].title];
                let count = t.chapters.len();
                section(ui, &p, ic::LIST_BULLETS, tr!("Kapitel", "Chapters"));
                let mut changed = false;
                ui.horizontal(|ui| {
                    let e = &mut self.plan.reauthor[i];
                    ui.label(tr!("Von", "From"));
                    egui::ComboBox::from_id_salt("von").width(70.0).selected_text((e.first_chapter + 1).to_string()).show_ui(ui, |ui| {
                        for c in 0..count {
                            changed |= ui.selectable_value(&mut e.first_chapter, c, (c + 1).to_string()).changed();
                        }
                    });
                    ui.label(tr!("bis", "to"));
                    egui::ComboBox::from_id_salt(tr!("bis", "to")).width(70.0).selected_text((e.last_chapter + 1).to_string()).show_ui(ui, |ui| {
                        for c in 0..count {
                            changed |= ui.selectable_value(&mut e.last_chapter, c, (c + 1).to_string()).changed();
                        }
                    });
                    if e.last_chapter < e.first_chapter {
                        e.last_chapter = e.first_chapter;
                    }
                });
                if changed {
                    // Schnittmarken beziehen sich auf die Kapitelauswahl und gelten danach nicht mehr.
                    // Anfang und Ende stehen gleich auf dem gewählten Kapitelbereich.
                    let e = &self.plan.reauthor[i];
                    let total = entry_duration_ms(disc, e);
                    let exact = e.cut.exact;
                    self.plan.reauthor[i].cut = Cut::whole(total, exact);
                    // Eigene Kapitelmarken gehören ebenfalls zum alten Bereich.
                    self.plan.reauthor[i].chapter_marks = None;
                    self.preview.dirty = true;
                }
                ui.add_space(4.0);
                self.view_cut(ui, disc, i);
                self.view_chapter_marks(ui, disc, i);
                // 1:1: Bild unverändert übernehmen, nur an bildgenauen Schnittstellen wird neu kodiert
                let mut one_to_one = matches!(settings.video, VideoMode::Keep);
                if style::check(ui, &p, &mut one_to_one, tr!("1:1 übernehmen (Bild nicht verkleinern)", "Copy 1:1 (do not shrink video)")) {
                    settings.video = if one_to_one { VideoMode::Keep } else { VideoMode::Auto };
                }
                ui.add_space(4.0);
            }

            // Video
            section(ui, &p, ic::FILM_STRIP, tr!("BILD", "VIDEO"));
            let mut kind = match settings.video {
                VideoMode::Keep => 0,
                VideoMode::Auto => 1,
                VideoMode::Custom(_) => 2,
                VideoMode::Still => 3,
            };
            let opts = [(0, tr!("Original", "Original")), (1, tr!("Auto", "Auto")), (2, tr!("Eigen", "Custom")), (3, tr!("Standbild", "Still image"))];
            if segmented(ui, &p, &mut kind, &opts, width) {
                settings.video = match kind {
                    0 => VideoMode::Keep,
                    1 => VideoMode::Auto,
                    2 => VideoMode::Custom(est.auto_ratio.clamp(0.3, 1.0)),
                    _ => VideoMode::Still,
                };
            }
            let (ratio, desc) = match settings.video {
                VideoMode::Keep => (1.0, tr!("Das Bild bleibt genau wie auf der DVD.", "The video stays exactly as on the DVD.").to_string()),
                VideoMode::Auto => (est.auto_ratio, tr!("Wird so weit verkleinert, dass alles auf den Rohling passt.", "Shrunk just enough for everything to fit on the blank disc.").to_string()),
                VideoMode::Custom(r) => (r, tr!("Du legst den Anteil selbst fest.", "You set the share yourself.").to_string()),
                VideoMode::Still => (0.12, tr!("Nur ein Standbild je Abschnitt, Zeitablauf und Ton bleiben.", "Only one still image per section; timing and audio are kept.").to_string()),
            };
            ui.label(RichText::new(desc).size(12.5).color(p.muted));
            if let VideoMode::Custom(r) = &mut settings.video {
                ui.spacing_mut().slider_width = width - 70.0;
                let mut v = *r * 100.0;
                let s = egui::Slider::new(&mut v, 20.0..=100.0).suffix(" %").step_by(1.0);
                if ui.add(s).changed() {
                    *r = v / 100.0;
                }
            }
            if !matches!(settings.video, VideoMode::Still) {
                let (word, level) = quality_word(ratio);
                let color = [p.good, p.warn, p.bad][level];
                ui.horizontal(|ui| {
                    ui.label(RichText::new(trf!("{} bleiben", "{} kept", percent(f64::from(ratio)))).family(semibold()).size(14.0));
                    ui.label(RichText::new(word).size(13.0).color(color));
                });
                if !matches!(settings.video, VideoMode::Keep) { ui.horizontal(|ui| {
                    ui.label(RichText::new(tr!("Glättung", "Smoothing")).color(p.muted)).on_hover_text(tr!("Entfernt feinste Bilddetails, das spart Platz und vermeidet Blockbildung.", "Removes the finest picture details; this saves space and avoids blockiness."));
                    egui::ComboBox::from_id_salt("glaettung").width(120.0).selected_text(settings.smoothing.label()).show_ui(ui, |ui| {
                        for sm in [Smoothing::Off, Smoothing::Low, Smoothing::Medium, Smoothing::High] {
                            ui.selectable_value(&mut settings.smoothing, sm, sm.label());
                        }
                    });
                }); }
            }

            let out_v = match settings.video {
                VideoMode::Keep => stats.video,
                VideoMode::Auto => stats.video * f64::from(est.auto_ratio),
                VideoMode::Custom(r) => stats.video * f64::from(r),
                VideoMode::Still => stats.video * 0.12,
            };
            ui.label(RichText::new(trf!("Bilddaten: {} → {}", "Video data: {} → {}", fmt_size(stats.video), fmt_size(out_v))).size(12.5).color(p.muted));

            // Spuren
            if let Some(ti) = tracks_title {
                let t = &disc.titles[ti];
                ui.add_space(6.0);
                section(ui, &p, ic::SPEAKER_HIGH, tr!("TON", "AUDIO"));
                if t.audio.is_empty() {
                    ui.label(RichText::new(tr!("Keine Tonspuren", "No audio tracks")).size(13.0).color(p.muted));
                }
                for a in &t.audio {
                    let on = &mut settings.audio[a.phys as usize];
                    let text = format!(
                        "{} · {} {}{}",
                        lang_name(&a.attr.lang),
                        a.attr.codec_name(),
                        a.attr.channels_name(),
                        if a.attr.ext_name().is_empty() { String::new() } else { format!(" · {}", a.attr.ext_name()) },
                    );
                    track_row(ui, &p, on, &text, &fmt_size(stats.audio[a.phys as usize]));
                }
                ui.add_space(6.0);
                section(ui, &p, ic::SUBTITLES, tr!("UNTERTITEL", "SUBTITLES"));
                if t.subs.is_empty() {
                    ui.label(RichText::new(tr!("Keine Untertitel", "No subtitles")).size(13.0).color(p.muted));
                }
                for a in &t.subs {
                    let idx = a.phys.first().copied().unwrap_or(0) as usize;
                    let mut on = a.phys.iter().any(|ph| settings.subs[*ph as usize]);
                    let size: f64 = a.phys.iter().map(|ph| stats.sub[*ph as usize]).sum();
                    let text = format!(
                        "{}{}",
                        lang_name(&a.attr.lang),
                        if a.attr.ext_name().is_empty() { String::new() } else { format!(" · {}", a.attr.ext_name()) }
                    );
                    let _ = idx;
                    if track_row(ui, &p, &mut on, &text, &fmt_size(size)) {
                        for ph in &a.phys {
                            settings.subs[*ph as usize] = on;
                        }
                    }
                }
                ui.add_space(6.0);
                let a_sum = stat_sum_audio(&stats, &settings.audio);
                let s_sum = stat_sum_subs(&stats, &settings.subs);
                ui.label(RichText::new(trf!("Ton und Untertitel: {}", "Audio and subtitles: {}", fmt_size(a_sum + s_sum))).size(12.5).color(p.muted));
            } else {
                ui.add_space(6.0);
                ui.label(RichText::new(trf!("{}  Menüs behalten immer alle Ton- und Untertitelspuren.", "{}  Menus always keep all audio and subtitle tracks.", ic::INFO)).size(12.5).color(p.faint));
            }
        });

        // Änderungen übernehmen
        match sel {
            Selection::Item(id) => {
                self.plan.items.insert(id, settings);
            }
            Selection::Entry(i) => {
                if let Some(e) = self.plan.reauthor.get_mut(i) {
                    e.settings = settings;
                }
            }
            Selection::None | Selection::Source(_) => {}
        }
    }

    /// Schnittmarken eines Eintrags: Anfang und Ende, wahlweise bildgenau.
    fn view_cut(&mut self, ui: &mut Ui, disc: &Disc, i: usize) {
        let p = self.pal;
        let now = self.preview.current_ms();
        let total = entry_duration_ms(disc, &self.plan.reauthor[i]);
        section(ui, &p, ic::SCISSORS, tr!("SCHNITT", "CUT"));
        let cut = &mut self.plan.reauthor[i].cut;
        let mut changed = false;
        let mut seek: Option<u64> = None;
        for (label, value, empty, fallback) in
            [(tr!("Anfang", "Start"), &mut cut.start_ms, tr!("Titelanfang", "Title start"), 0), (tr!("Ende", "End"), &mut cut.end_ms, tr!("Titelende", "Title end"), total.saturating_sub(40))]
        {
            ui.horizontal(|ui| {
                ui.add_sized([52.0, 24.0], egui::Label::new(RichText::new(label).color(p.muted)));
                let text = value.map(fmt_time_precise).unwrap_or_else(|| empty.to_string());
                ui.add_sized([86.0, 24.0], egui::Label::new(RichText::new(text).family(semibold())));
                // Zur Marke springen (ohne Marke: Titelanfang bzw. letztes Bild)
                let jump = egui::Button::new(RichText::new(ic::ARROW_LINE_RIGHT).size(15.0).color(p.text))
                    .fill(Color32::TRANSPARENT)
                    .stroke(Stroke::NONE)
                    .corner_radius(CornerRadius::same(6))
                    .min_size(Vec2::new(28.0, 28.0));
                if ui.add(jump).on_hover_text(tr!("In der Vorschau zu dieser Schnittmarke springen", "Jump to this cut marker in the preview")).clicked() {
                    seek = Some(value.unwrap_or(fallback));
                }
                let b = ui
                    .add_enabled(now.is_some(), egui::Button::new(RichText::new(ic::MAP_PIN).size(15.0)).min_size(Vec2::new(30.0, 26.0)))
                    .on_hover_text(tr!("Setzt die Marke auf das Bild, das die Vorschau gerade zeigt", "Sets the marker to the frame the preview is showing"));
                if b.clicked() {
                    *value = now;
                    changed = true;
                }
                if value.is_some() && style::icon_button(ui, &p, ic::X, tr!("Marke entfernen", "Remove marker")).clicked() {
                    *value = None;
                    changed = true;
                }
            });
        }
        if let Some(t) = seek {
            self.preview.pause();
            self.preview.seek_time(t.min(total) as f64, true);
        }
        let cut = &mut self.plan.reauthor[i].cut;
        if let (Some(a), Some(b)) = (cut.start_ms, cut.end_ms) {
            if b <= a {
                cut.end_ms = None;
                changed = true;
            }
        }
        if style::check(ui, &p, &mut cut.exact, tr!("Bildgenau schneiden", "Frame-accurate cut")) {
            self.prefs.cut_exact = cut.exact;
        }
        let hint = if cut.exact {
            tr!("Nur die Bildgruppen an den Schnittstellen werden neu kodiert, der Rest bleibt unverändert.", "Only the frame groups at the cut points are re-encoded; the rest stays unchanged.")
        } else {
            tr!("Geschnitten wird am nächsten Schlüsselbild, also auf etwa eine halbe Sekunde genau, ganz ohne Neukodierung.", "Cuts are made at the nearest keyframe, accurate to about half a second, without any re-encoding.")
        };
        ui.label(RichText::new(hint).size(12.5).color(p.muted));
        if cut.is_trim(total) {
            let len = cut.end().min(total).saturating_sub(cut.start());
            ui.label(RichText::new(trf!("Länge nach dem Schnitt: {}", "Length after cut: {}", fmt_time(len))).size(13.0));
        }
        let _ = changed;
        ui.add_space(2.0);
        let scenes = soft_button(ui, &p, &format!("{}  {}", ic::FILM_STRIP, tr!("Szenen erkennen …", "Detect scenes …")))
            .on_hover_text(tr!("Teilt den Eintrag an Szenenwechseln in einzelne Einträge", "Splits the entry into separate entries at scene changes"));
        if scenes.clicked() {
            let ctx = ui.ctx().clone();
            self.open_scene_dialog(i, &ctx);
        }
        let cut = &self.plan.reauthor[i].cut;
        ui.add_space(4.0);
        // Blenden brauchen Neukodierung, deshalb nur beim bildgenauen Schnitt
        if cut.exact {
            self.view_fades(ui, i);
        }
    }

    /// Blenden am Anfang und Ende eines Eintrags: keine, aus/nach Schwarz oder Weiß.
    fn view_fades(&mut self, ui: &mut Ui, i: usize) {
        let p = self.pal;
        let kinds = [
            (FadeKind::None, tr!("Keine", "None")),
            (FadeKind::Black, tr!("Schwarz", "Black")),
            (FadeKind::White, tr!("Weiß", "White")),
        ];
        let Some(e) = self.plan.reauthor.get_mut(i) else { return };
        let fade = &mut e.fade;
        ui.label(RichText::new(tr!("Blende", "Fade")).family(semibold()).size(13.0).color(p.muted));
        for (label, value, id) in [
            (tr!("Einblenden", "Fade in"), &mut fade.fade_in, "blende-an"),
            (tr!("Ausblenden", "Fade out"), &mut fade.fade_out, "blende-ab"),
        ] {
            ui.horizontal(|ui| {
                ui.add_sized([86.0, 24.0], egui::Label::new(RichText::new(label).color(p.muted)));
                let current = kinds.iter().find(|k| k.0 == *value).map(|k| k.1).unwrap_or("");
                egui::ComboBox::from_id_salt(id).width(110.0).selected_text(current).show_ui(ui, |ui| {
                    for (k, name) in kinds {
                        ui.selectable_value(value, k, name);
                    }
                });
            });
        }
        if fade.is_active() {
            ui.horizontal(|ui| {
                ui.add_sized([86.0, 24.0], egui::Label::new(RichText::new(tr!("Dauer", "Duration")).color(p.muted)));
                let mut secs = fade.duration_ms() / 1000.0;
                let dv = egui::DragValue::new(&mut secs)
                    .range(0.2..=5.0)
                    .speed(0.05)
                    .custom_formatter(|v, _| discslim::i18n::decimal(format!("{v:.1} s")))
                    .custom_parser(|s| s.trim().trim_end_matches('s').trim().replace(',', ".").parse().ok());
                if ui.add(dv).changed() {
                    fade.ms = (secs * 1000.0).round() as u32;
                }
            });
            style::check(ui, &p, &mut fade.audio, tr!("Ton mit ein- und ausblenden", "Fade the audio as well"));
            ui.label(
                RichText::new(if fade.audio {
                    tr!(
                        "Die Bilder im Blendenbereich werden neu kodiert, der Ton wird mit ein- und ausgeblendet.",
                        "Frames within the fade are re-encoded; the audio fades along with the picture."
                    )
                } else {
                    tr!(
                        "Die Bilder im Blendenbereich werden neu kodiert, der Ton bleibt unverändert.",
                        "Frames within the fade are re-encoded; the audio stays unchanged."
                    )
                })
                .size(12.5)
                .color(p.muted),
            );
        }
        // Dieselbe Blende für alle Einträge (z. B. alle Szenen nach der Szenenerkennung)
        let others = self.plan.reauthor.len().saturating_sub(1);
        if others > 0 {
            let r = ui
                .add_enabled(others > 0, egui::Button::new(RichText::new(trf!("{}  Auf alle Einträge übertragen", "{}  Apply to all entries", ic::COPY)).size(13.0)))
                .on_hover_text(trf!(
                    "Übernimmt diese Blende für die {others} anderen Einträge. Blenden wirken nur bei bildgenauem Schnitt, das wird dort mit eingeschaltet.",
                    "Applies this fade to the {others} other entries. Fades only work with frame-accurate cutting, which is switched on there as well."
                ));
            if r.clicked() {
                let fade = self.plan.reauthor[i].fade.clone();
                for (k, e) in self.plan.reauthor.iter_mut().enumerate() {
                    if k != i {
                        e.fade = fade.clone();
                        e.cut.exact = true;
                    }
                }
                self.preview.dirty = true;
                self.notice = Some((trf!("Blende auf {others} weitere Einträge übertragen", "Fade applied to {others} more entries"), std::time::Instant::now()));
            }
        }
        ui.add_space(4.0);
    }

    /// Kapitel-Editor: eigene Kapitelmarken setzen, verschieben, verteilen oder entfernen.
    fn view_chapter_marks(&mut self, ui: &mut Ui, disc: &Disc, i: usize) {
        let p = self.pal;
        let now = self.preview.current_ms();
        let Some(e) = self.plan.reauthor.get(i) else { return };
        let total = entry_duration_ms(disc, e);
        let (cut_a, cut_b) = e.cut.effective(total);
        let cut_b = cut_b.min(total);
        section(ui, &p, ic::BOOKMARK_SIMPLE, tr!("KAPITELMARKEN", "CHAPTER MARKERS"));

        let Some(marks) = e.chapter_marks.clone() else {
            let n = e.last_chapter + 1 - e.first_chapter.min(e.last_chapter);
            let text = if n == 1 { tr!("Die DVD hat hier 1 Kapitel.", "The DVD has 1 chapter here.").to_string() } else { trf!("Die {n} Kapitel der DVD werden übernommen.", "The DVD's {n} chapters are kept.") };
            ui.label(RichText::new(text).size(13.0).color(p.muted));
            ui.horizontal(|ui| {
                if ui
                    .button(RichText::new(trf!("{}  Kapitel bearbeiten", "{}  Edit chapters", ic::PENCIL_SIMPLE)).size(13.0))
                    .on_hover_text(tr!("Übernimmt die Kapitel der DVD als Marken, die du dann ändern kannst", "Takes over the DVD's chapters as markers that you can then change"))
                    .clicked()
                {
                    let m = self.plan.reauthor[i].original_marks(disc);
                    self.plan.reauthor[i].chapter_marks = Some(m);
                }
                if ui
                    .add_enabled(now.is_some(), egui::Button::new(RichText::new(trf!("{}  Neu: Marke hier", "{}  New: marker here", ic::MAP_PIN)).size(13.0)))
                    .on_hover_text(tr!("Verwirft die Kapitel der DVD und beginnt mit einer Marke an der Stelle der Vorschau", "Discards the DVD's chapters and starts with a marker at the preview position"))
                    .clicked()
                {
                    let mut m: Vec<u64> = now.into_iter().collect();
                    ReauthorEntry::normalize_marks(&mut m);
                    self.plan.reauthor[i].chapter_marks = Some(m);
                }
            });
            ui.add_space(4.0);
            return;
        };

        let mut marks = marks;
        let mut changed = false;
        let mut seek = None;
        let row = |ui: &mut Ui, n: usize, at: u64, outside: bool| {
            let color = if outside { p.faint } else { p.text };
            ui.add_sized([22.0, 22.0], egui::Label::new(RichText::new(format!("{n}.")).color(p.muted)));
            let r = ui.add_sized([82.0, 22.0], egui::Label::new(RichText::new(fmt_time_precise(at)).family(semibold()).color(color)));
            if outside {
                r.on_hover_text(tr!("Liegt nicht im geschnittenen Bereich und entfällt in der neuen DVD", "Outside the cut range; dropped in the new DVD"));
            }
        };
        // Kleine Symbolknöpfe, damit eine Zeile in die schmale Spalte passt
        let mini = |ui: &mut Ui, icon: &str, tip: &str| {
            let b = egui::Button::new(RichText::new(icon).size(14.0).color(p.text))
                .fill(Color32::TRANSPARENT)
                .stroke(Stroke::NONE)
                .corner_radius(CornerRadius::same(6))
                .min_size(Vec2::new(24.0, 24.0));
            ui.add(b).on_hover_text(tip).clicked()
        };
        egui::Frame::new().fill(p.raised).corner_radius(CornerRadius::same(8)).inner_margin(Margin::symmetric(8, 4)).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.spacing_mut().item_spacing.y = 0.0;
            ui.horizontal(|ui| {
                row(ui, 1, cut_a, false);
                ui.label(RichText::new(tr!("Anfang", "Start")).size(12.5).color(p.muted));
            });
            let mut remove = None;
            let mut nudge: Option<(usize, i64)> = None;
            for (k, m) in marks.iter().enumerate() {
                let outside = *m <= cut_a || *m >= cut_b;
                ui.horizontal(|ui| {
                    row(ui, k + 2, *m, outside);
                    ui.spacing_mut().item_spacing.x = 0.0;
                    if mini(ui, ic::ARROW_LINE_RIGHT, tr!("In der Vorschau zu dieser Marke springen", "Jump to this marker in the preview")) {
                        seek = Some(*m);
                    }
                    if mini(ui, ic::CARET_LEFT, tr!("Eine Sekunde früher", "One second earlier")) {
                        nudge = Some((k, -1000));
                    }
                    if mini(ui, ic::CARET_RIGHT, tr!("Eine Sekunde später", "One second later")) {
                        nudge = Some((k, 1000));
                    }
                    if let Some(t) = now {
                        if mini(ui, ic::MAP_PIN, tr!("Auf die Stelle der Vorschau setzen", "Set to the preview position")) {
                            nudge = Some((k, t as i64 - *m as i64));
                        }
                    }
                    if mini(ui, ic::X, tr!("Marke entfernen", "Remove marker")) {
                        remove = Some(k);
                    }
                });
            }
            if let Some(k) = remove {
                marks.remove(k);
                changed = true;
            }
            if let Some((k, d)) = nudge {
                let v = (marks[k] as i64 + d).clamp(0, total as i64) as u64;
                marks[k] = v;
                changed = true;
                // Vorschau zeigt das Bild an der neuen Stelle der Marke
                seek = Some(v);
            }
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let b = ui
                .add_enabled(now.is_some(), egui::Button::new(RichText::new(trf!("{}  Marke hier", "{}  Marker here", ic::PLUS)).size(13.0)))
                .on_hover_text(tr!("Setzt eine Kapitelmarke an das Bild, das die Vorschau gerade zeigt", "Sets a chapter marker at the frame the preview is showing"));
            if b.clicked() {
                if let Some(t) = now {
                    marks.push(t);
                    changed = true;
                }
            }
            if ui
                .button(RichText::new(trf!("{}  Original", "{}  Original", ic::ARROW_COUNTER_CLOCKWISE)).size(13.0))
                .on_hover_text(tr!("Wieder die Kapitel der DVD verwenden", "Use the DVD's chapters again"))
                .clicked()
            {
                self.plan.reauthor[i].chapter_marks = None;
            }
        });
        if self.plan.reauthor[i].chapter_marks.is_none() {
            return;
        }
        ui.horizontal(|ui| {
            ui.label(RichText::new(tr!("Alle", "Every")).color(p.muted));
            ui.add(egui::DragValue::new(&mut self.prefs.chapter_every_min).range(1..=60).suffix(tr!(" Min.", " min")));
            if ui
                .button(RichText::new(trf!("{}  verteilen", "{}  distribute", ic::LIST_NUMBERS)).size(13.0))
                .on_hover_text(tr!("Ersetzt alle Marken durch gleichmäßige Abstände im geschnittenen Bereich", "Replaces all markers with even spacing within the cut range"))
                .clicked()
            {
                let step = u64::from(self.prefs.chapter_every_min.max(1)) * 60_000;
                marks = (1..).map(|k| cut_a + k * step).take_while(|m| *m + MIN_CHAPTER_GAP_MS < cut_b).take(98).collect();
                changed = true;
            }
            if !marks.is_empty() && ui.button(RichText::new(trf!("{}  Alle weg", "{}  Remove all", ic::TRASH)).size(13.0)).clicked() {
                marks.clear();
                changed = true;
            }
        });
        if changed {
            ReauthorEntry::normalize_marks(&mut marks);
            marks.truncate(98);
            self.plan.reauthor[i].chapter_marks = Some(marks);
        }
        let count = self.plan.reauthor[i].chapter_count(disc);
        ui.label(
            RichText::new(trf!(
                "Die neue DVD hat hier {}. Sie beginnen am nächsten Schlüsselbild, also auf etwa eine halbe Sekunde genau.", "The new DVD has {} here. They start at the nearest keyframe, accurate to about half a second.", discslim::i18n::chapters(count)
            ))
            .size(12.5)
            .color(p.muted),
        );
        if let Some(t) = seek {
            self.preview.pause();
            self.preview.seek_time(t as f64, true);
        }
        ui.add_space(4.0);
    }

    fn view_capacity(&mut self, ui: &mut Ui, est: &Estimate) {
        let p = self.pal;
        let target = self.plan.target_sectors as f64;

        // Zielgröße rechts
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 16.0;
            legend(ui, &p, p.video, tr!("Bild", "Video"), &fmt_size(est.video));
            legend(ui, &p, p.audio, tr!("Ton", "Audio"), &fmt_size(est.audio));
            legend(ui, &p, p.subs, tr!("Untertitel", "Subtitles"), &fmt_size(est.subs));
            legend(ui, &p, p.other, tr!("Verwaltung", "Overhead"), &fmt_size(est.overhead));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let kinds = [
                    (TargetKind::Dvd5, tr!("DVD 4,7 GB", "DVD 4.7 GB")),
                    (TargetKind::Dvd9, tr!("DVD 8,5 GB", "DVD 8.5 GB")),
                    (TargetKind::Custom, tr!("Eigene Größe", "Custom size")),
                    (TargetKind::Unlimited, tr!("Unbegrenzt", "Unlimited")),
                ];
                if self.prefs.target_kind == TargetKind::Custom {
                    let dv = egui::DragValue::new(&mut self.prefs.custom_mb).range(10..=20000).suffix(" MB").speed(10);
                    if ui.add(dv).changed() {
                        self.plan.target_sectors = super::target_sectors(&self.prefs);
                    }
                }
                let current = kinds.iter().find(|k| k.0 == self.prefs.target_kind).map(|k| k.1).unwrap_or("");
                let mut chosen = self.prefs.target_kind;
                egui::ComboBox::from_id_salt("ziel").width(130.0).selected_text(current).show_ui(ui, |ui| {
                    for (k, label) in kinds {
                        ui.selectable_value(&mut chosen, k, label);
                    }
                });
                if chosen != self.prefs.target_kind {
                    self.prefs.target_kind = chosen;
                    self.plan.target_sectors = match chosen {
                        TargetKind::Dvd5 => dvd5_sectors(),
                        TargetKind::Dvd9 => dvd9_sectors(),
                        TargetKind::Custom | TargetKind::Unlimited => super::target_sectors(&self.prefs),
                    };
                }
                ui.label(RichText::new(tr!("Ziel", "Target")).color(p.muted));
            });
        });

        // Balken
        let w = ui.available_width();
        let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 20.0), Sense::hover());
        let radius = CornerRadius::same(7);
        ui.painter().rect_filled(rect, radius, p.track);
        let unlimited = self.plan.target_sectors >= discslim::engine::plan::NO_TARGET;
        let scale = if unlimited { rect.width() as f64 / est.total.max(1.0) } else { rect.width() as f64 / target.max(est.total).max(1.0) };
        let mut x = rect.left();
        let clip = ui.painter_at(rect);
        for (val, color) in [(est.video, p.video), (est.audio, p.audio), (est.subs, p.subs), (est.overhead, p.other)] {
            let seg = (val * scale) as f32;
            if seg > 0.0 {
                let r = Rect::from_min_size(egui::pos2(x, rect.top()), Vec2::new(seg, rect.height()));
                clip.rect_filled(r.shrink2(Vec2::new(0.0, 0.0)), CornerRadius::same(0), color);
                x += seg;
            }
        }
        ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, p.line), StrokeKind::Inside);

        // Zielmarke und Text
        let mark_x = rect.left() + (target * scale) as f32;
        if est.total > target && !unlimited {
            ui.painter().line_segment(
                [egui::pos2(mark_x, rect.top() - 3.0), egui::pos2(mark_x, rect.bottom() + 3.0)],
                Stroke::new(2.0, p.bad),
            );
        }
        let free = target - est.total;
        let (status, color) = if unlimited {
            (tr!("ohne Größengrenze", "no size limit").to_string(), p.muted)
        } else if free >= 0.0 {
            (trf!("{} frei", "{} free", fmt_size(free)), p.good)
        } else {
            (trf!("{} zu viel", "{} too much", fmt_size(-free)), p.bad)
        };
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(if unlimited { fmt_size(est.total) } else { trf!("{} von {}", "{} of {}", fmt_size(est.total), fmt_size(target)) })
                    .family(semibold())
                    .size(14.0),
            );
            ui.label(RichText::new(status).size(13.0).color(color));
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                let (word, level) = quality_word(est.auto_ratio);
                let c = [p.good, p.warn, p.bad][level];
                ui.label(RichText::new(word).size(13.0).color(c));
                ui.label(
                    RichText::new(trf!("Automatik behält {} des Bildes:", "Automatic mode keeps {} of the video:", percent(f64::from(est.auto_ratio))))
                        .size(13.0)
                        .color(p.muted),
                );
                let d = self.disc.as_ref();
                if d.map(|d| !d.analysed_full).unwrap_or(false) {
                    ui.label(RichText::new(tr!("Größen aus Stichprobe ·", "Sizes from sample ·")).size(12.0).color(p.faint))
                        .on_hover_text(tr!("Die Tiefenanalyse oben rechts liefert genaue Werte.", "The deep analysis at the top right gives exact values."));
                }
            });
        });

        if !est.fits && !unlimited {
            ui.label(
                RichText::new(trf!(
                    "{}  Das passt so nicht auf den Rohling. Wähle Tonspuren oder Extras ab.", "{}  This doesn't fit on the blank disc. Deselect audio tracks or extras.",
                    ic::WARNING
                ))
                .size(13.0)
                .color(p.bad),
            );
        }
    }
}

fn card_filled(ui: &mut Ui, p: &style::Pal, add: impl FnOnce(&mut Ui)) {
    let rect = ui.max_rect();
    card(ui, p, |ui| {
        ui.set_min_size(rect.size() - Vec2::splat(28.0));
        ui.set_max_size(rect.size() - Vec2::splat(28.0));
        add(ui);
    });
}

fn group_label(ui: &mut Ui, p: &style::Pal, text: &str) {
    ui.add_space(6.0);
    ui.label(RichText::new(text).family(semibold()).size(12.5).color(p.faint));
}

fn start_card(ui: &mut Ui, p: &style::Pal, w: f32, icon: &str, title: &str, sub: &str, enabled: bool) -> bool {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, 148.0), Sense::hover());
    let resp = ui.interact(rect, ui.scope_id().with(title), Sense::click());
    let hovered = resp.hovered() && enabled;
    let fill = if hovered { p.raised } else { p.card };
    let stroke = if hovered { p.accent } else { p.line };
    let radius = CornerRadius::same(14);
    ui.painter().rect_filled(rect, radius, fill);
    ui.painter().rect_stroke(rect, radius, Stroke::new(1.0, stroke), StrokeKind::Inside);
    let c = rect.center();
    let icon_color = if enabled { p.accent } else { p.faint };
    ui.painter().text(c - Vec2::new(0.0, 36.0), Align2::CENTER_CENTER, icon, FontId::proportional(36.0), icon_color);
    let tc = if enabled { p.text } else { p.muted };
    ui.painter().text(c + Vec2::new(0.0, 12.0), Align2::CENTER_CENTER, title, FontId::new(16.0, semibold()), tc);

    let galley = ui.painter().layout(sub.to_string(), FontId::proportional(12.5), p.muted, w - 28.0);
    let gp = egui::pos2(c.x - galley.size().x / 2.0, c.y + 28.0);
    ui.painter().galley(gp, galley, p.muted);
    if hovered {
        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
    }
    resp.clicked() && enabled
}

fn track_row(ui: &mut Ui, p: &style::Pal, on: &mut bool, text: &str, size: &str) -> bool {
    let mut changed = false;
    ui.horizontal(|ui| {
        changed = style::check(ui, p, on, text);
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(RichText::new(size).size(12.5).color(p.muted));
        });
    });
    changed
}
