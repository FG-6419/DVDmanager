//! Dialog "Szenen erkennen": analysiert einen Eintrag im Hintergrund, zeigt die
//! gefundenen Szenen als Vorschaubilder und übernimmt die gewählten als einzelne Einträge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Align, Align2, Color32, CornerRadius, FontId, Layout, Rect, RichText, Stroke, StrokeKind, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::model::fmt_time;
use discslim::engine::preview::title_segments;
use discslim::engine::scenes::{scan, split_entry, SceneScan};

use super::dialogs::{modal, progress_bar};
use super::style::{self, primary_button, semibold, soft_button};
use super::thumbs::HoverLoop;
use super::{App, Selection};

pub(super) struct SceneDialog {
    entry: usize,
    progress: Arc<Mutex<f32>>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Result<SceneScan, String>>,
    result: Option<Result<SceneScan, String>>,
    sensitivity: f32,
    min_secs: f32,
    exact: bool,
    /// Auswahl je Szene, gültig für die Szenenanfänge in `chosen_for`
    chosen: Vec<bool>,
    chosen_for: Vec<u64>,
}

/// Kennung der Vorschauschleife für Szene k (getrennt von denen der Einträge).
fn loop_key(k: usize) -> (usize, u8) {
    (usize::MAX - k, 9)
}

impl SceneDialog {
    /// Nur für Prüfbilder: wartet, bis die Analyse fertig ist.
    #[cfg(debug_assertions)]
    pub(super) fn wait_for_result(&mut self) {
        if let Ok(r) = self.rx.recv() {
            self.result = Some(r);
        }
    }

    /// Nur für Prüfbilder: wählt eine Szene ab.
    #[cfg(debug_assertions)]
    pub(super) fn deselect(&mut self, k: usize) {
        if let Some(c) = self.chosen.get_mut(k) {
            *c = false;
        }
    }
}

impl App {
    /// Startet die Analyse des Eintrags `i`.
    pub(super) fn open_scene_dialog(&mut self, i: usize, ctx: &egui::Context) {
        let (Some(disc), Some(entry)) = (self.disc.clone(), self.plan.reauthor.get(i).cloned()) else { return };
        let progress = Arc::new(Mutex::new(0f32));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (p2, c2, ctx2) = (progress.clone(), cancel.clone(), ctx.clone());
        std::thread::spawn(move || {
            let mut last = Instant::now();
            let res = scan(&disc, &entry, &c2, &mut |f| {
                if let Ok(mut g) = p2.lock() {
                    *g = f;
                }
                if last.elapsed().as_millis() > 100 {
                    last = Instant::now();
                    ctx2.request_repaint();
                }
            });
            let _ = tx.send(res.map_err(|e| e.to_string()));
            ctx2.request_repaint();
        });
        let exact = self.plan.reauthor[i].cut.exact;
        self.scene_dlg = Some(SceneDialog {
            entry: i,
            progress,
            cancel,
            rx,
            result: None,
            sensitivity: 0.5,
            min_secs: 2.0,
            exact,
            chosen: Vec::new(),
            chosen_for: Vec::new(),
        });
    }

    pub(super) fn dialog_scenes(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.scene_dlg.take() else { return };
        if dlg.result.is_none() {
            if let Ok(r) = dlg.rx.try_recv() {
                dlg.result = Some(r);
            }
        }
        let (Some(disc), Some(entry)) = (self.disc.clone(), self.plan.reauthor.get(dlg.entry).cloned()) else { return };
        let p = self.pal;
        let mut close = false;
        let mut apply: Option<Vec<usize>> = None;
        let progress = dlg.progress.lock().map(|g| *g).unwrap_or(0.0);
        let width = if matches!(dlg.result, Some(Ok(_))) { 780.0 } else { 520.0 };

        modal(ctx, &p, "szenen", width, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::FILM_STRIP).size(24.0).color(p.accent));
                ui.label(RichText::new(tr!("Szenen erkennen", "Detect scenes")).family(semibold()).size(20.0));
            });
            ui.label(
                RichText::new(tr!(
                    "Sucht Szenenwechsel im gewählten Bereich. Die ausgewählten Szenen werden einzelne Einträge.",
                    "Finds scene changes in the selected range. The selected scenes become separate entries."
                ))
                .size(13.0)
                .color(p.muted),
            );
            ui.add_space(10.0);
            match &dlg.result {
                None => {
                    ui.label(RichText::new(tr!("Bilder werden verglichen …", "Comparing frames …")).family(semibold()));
                    ui.add_space(6.0);
                    progress_bar(ui, &p, progress);
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                            dlg.cancel.store(true, Ordering::Relaxed);
                            close = true;
                        }
                    });
                }
                Some(Err(e)) => {
                    ui.label(RichText::new(e).color(p.warn));
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if primary_button(ui, &p, tr!("Schließen", "Close"), true).clicked() {
                            close = true;
                        }
                    });
                }
                Some(Ok(scan)) => {
                    // Einstellungen in einer Zeile
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 8.0;
                        ui.label(RichText::new(tr!("Empfindlichkeit", "Sensitivity")).color(p.muted));
                        ui.label(RichText::new(tr!("gering", "low")).size(12.0).color(p.faint));
                        ui.add(egui::Slider::new(&mut dlg.sensitivity, 0.0..=1.0).show_value(false));
                        ui.label(RichText::new(tr!("hoch", "high")).size(12.0).color(p.faint));
                        ui.add_space(16.0);
                        ui.label(RichText::new(tr!("Mindestlänge", "Minimum length")).color(p.muted));
                        let dv = egui::DragValue::new(&mut dlg.min_secs)
                            .range(0.5..=600.0)
                            .speed(0.1)
                            .custom_formatter(|v, _| format!("{} s", discslim::i18n::decimal(format!("{v:.1}"))))
                            .custom_parser(|s| s.trim().trim_end_matches('s').trim().replace(',', ".").parse().ok());
                        ui.add(dv).on_hover_text(tr!("Kürzere Szenen werden mit der Nachbarszene zusammengefasst", "Shorter scenes are merged with the neighbouring scene"));
                    });
                    ui.add_space(4.0);
                    style::check(ui, &p, &mut dlg.exact, tr!("Bildgenau schneiden", "Frame-accurate cut"));
                    ui.label(
                        RichText::new(if dlg.exact {
                            tr!("Geschnitten wird genau am ersten Bild der neuen Szene; die Bildgruppen dort werden neu kodiert.", "Cuts exactly at the first frame of the new scene; the frame groups there are re-encoded.")
                        } else {
                            tr!("Geschnitten wird am Anfang der Bildgruppe mit dem Wechsel, ohne Neukodierung. Am Ende einer Szene können wenige Bilder der nächsten zu sehen sein.", "Cuts at the start of the frame group containing the change, without re-encoding. A few frames of the next scene may show at the end of a scene.")
                        })
                        .size(12.5)
                        .color(p.muted),
                    );
                    ui.add_space(10.0);

                    let cuts = scan.cuts(dlg.sensitivity, (dlg.min_secs * 1000.0) as u64, dlg.exact);
                    let mut bounds = vec![scan.start_ms];
                    bounds.extend(&cuts);
                    bounds.push(scan.end_ms);
                    let scenes: Vec<(u64, u64)> = bounds.windows(2).map(|w| (w[0], w[1])).collect();
                    // Neue Einteilung: wieder alles ausgewählt
                    if dlg.chosen_for != cuts || dlg.chosen.len() != scenes.len() {
                        dlg.chosen = vec![true; scenes.len()];
                        dlg.chosen_for = cuts.clone();
                    }
                    let n_sel = dlg.chosen.iter().filter(|c| **c).count();

                    ui.horizontal(|ui| {
                        let text = trf!(
                            "{} von {} ausgewählt",
                            "{} of {} selected",
                            n_sel,
                            discslim::i18n::count(scenes.len(), "Szene", "Szenen", "scene", "scenes")
                        );
                        ui.label(RichText::new(text).family(semibold()));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if soft_button(ui, &p, tr!("Keine", "None")).clicked() {
                                dlg.chosen.iter_mut().for_each(|c| *c = false);
                            }
                            if soft_button(ui, &p, tr!("Alle", "All")).clicked() {
                                dlg.chosen.iter_mut().for_each(|c| *c = true);
                            }
                        });
                    });
                    ui.add_space(6.0);

                    // Kacheln: Vorschaubild, unter der Maus läuft die Szene ab; Klick wählt.
                    let seg = || title_segments(&disc, entry.title, entry.first_chapter, entry.last_chapter);
                    let cols = 4usize;
                    let gap = 10.0;
                    egui::ScrollArea::vertical().id_salt("szenen-kacheln").max_height(400.0).auto_shrink([false, true]).show(ui, |ui| {
                        let w = ((ui.available_width() - gap * (cols as f32 - 1.0)) / cols as f32).floor();
                        let img_h = (w * 9.0 / 16.0).round();
                        for (row_i, row) in scenes.chunks(cols).enumerate() {
                            ui.horizontal(|ui| {
                                ui.spacing_mut().item_spacing.x = gap;
                                for (c, &(a, b)) in row.iter().enumerate() {
                                    let k = row_i * cols + c;
                                    let on = dlg.chosen[k];
                                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, img_h + 40.0), egui::Sense::click());
                                    let img = Rect::from_min_size(rect.min, Vec2::new(w, img_h));
                                    let painter = ui.painter_at(rect);
                                    painter.rect_filled(img, CornerRadius::same(6), p.screen);
                                    let mut tex = None;
                                    if resp.hovered() {
                                        let range = (a as f64, (a as f64 + 6_000.0).min(b as f64));
                                        if self.hover_loop.as_ref().is_none_or(|l| l.key != loop_key(k)) {
                                            self.hover_loop = Some(HoverLoop::start(loop_key(k), disc.source.clone(), seg(), range));
                                        }
                                        if let Some(l) = self.hover_loop.as_mut() {
                                            l.seen_this_frame = true;
                                            tex = l.texture();
                                        }
                                        ui.ctx().set_cursor_icon(egui::CursorIcon::PointingHand);
                                    }
                                    if tex.is_none() {
                                        let at = a + ((b - a) / 2).min(1_500);
                                        if let Some(th) = self.thumbs.as_mut() {
                                            tex = th.get((entry.title, entry.first_chapter, entry.last_chapter, at), &disc.source, seg);
                                        }
                                    }
                                    if let Some(t) = tex {
                                        let tint = if on { Color32::WHITE } else { Color32::from_gray(90) };
                                        painter.image(t, img, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), tint);
                                    }
                                    // Auswahl
                                    let stroke = if on { Stroke::new(2.5, p.accent) } else { Stroke::new(1.0, p.line) };
                                    painter.rect_stroke(img, CornerRadius::same(6), stroke, StrokeKind::Inside);
                                    let mark = Rect::from_min_size(img.min + Vec2::new(6.0, 6.0), Vec2::new(24.0, 24.0));
                                    painter.rect_filled(mark, CornerRadius::same(5), if on { p.accent } else { Color32::from_black_alpha(140) });
                                    painter.text(
                                        mark.center(),
                                        Align2::CENTER_CENTER,
                                        if on { ic::CHECK } else { "" },
                                        FontId::proportional(16.0),
                                        Color32::WHITE,
                                    );
                                    let text_col = if on { p.text } else { p.faint };
                                    painter.text(
                                        egui::pos2(rect.left() + 2.0, img.bottom() + 12.0),
                                        Align2::LEFT_CENTER,
                                        trf!("Szene {}", "Scene {}", k + 1),
                                        FontId::new(13.5, semibold()),
                                        text_col,
                                    );
                                    painter.text(
                                        egui::pos2(rect.left() + 2.0, img.bottom() + 30.0),
                                        Align2::LEFT_CENTER,
                                        format!("{} · {}", fmt_time(a), discslim::i18n::decimal(format!("{:.1} s", (b - a) as f64 / 1000.0))),
                                        FontId::proportional(12.0),
                                        p.muted,
                                    );
                                    if resp.clicked() {
                                        dlg.chosen[k] = !on;
                                    }
                                }
                            });
                            ui.add_space(8.0);
                        }
                    });
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let label = match n_sel {
                            1 => tr!("1 Szene übernehmen", "Keep 1 scene").to_string(),
                            n => trf!("{} Szenen übernehmen", "Keep {} scenes", n),
                        };
                        if primary_button(ui, &p, &label, n_sel > 0).clicked() {
                            apply = Some(dlg.chosen.iter().enumerate().filter(|(_, c)| **c).map(|(k, _)| k).collect());
                        }
                        if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });

        if let Some(keep) = apply {
            let cuts = dlg.chosen_for.clone();
            let parts: Vec<_> =
                split_entry(&disc, &entry, &cuts, dlg.exact).into_iter().enumerate().filter(|(k, _)| keep.contains(k)).map(|(_, e)| e).collect();
            let n = parts.len();
            let i = dlg.entry;
            self.plan.reauthor.splice(i..=i, parts);
            self.sel = Selection::Entry(i);
            self.preview.dirty = true;
            self.notice = Some((
                match n {
                    1 => tr!("1 Szene übernommen", "1 scene kept").to_string(),
                    n => trf!("{n} Szenen übernommen", "{n} scenes kept"),
                },
                Instant::now(),
            ));
            close = true;
        }
        if !close {
            self.scene_dlg = Some(dlg);
        }
    }
}
