//! Dialog "Szenen erkennen": analysiert einen Eintrag im Hintergrund und teilt ihn an
//! den gefundenen Szenenwechseln in einzelne Einträge.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Align, CornerRadius, Layout, RichText, Stroke, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::model::fmt_time;
use discslim::engine::scenes::{scan, split_entry, SceneScan};

use super::dialogs::{modal, progress_bar};
use super::style::{self, primary_button, semibold, soft_button};
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
}

impl SceneDialog {
    /// Nur für Prüfbilder: wartet, bis die Analyse fertig ist.
    #[cfg(debug_assertions)]
    pub(super) fn wait_for_result(&mut self) {
        if let Ok(r) = self.rx.recv() {
            self.result = Some(r);
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
        });
    }

    pub(super) fn dialog_scenes(&mut self, ctx: &egui::Context) {
        let Some(dlg) = self.scene_dlg.as_mut() else { return };
        if dlg.result.is_none() {
            if let Ok(r) = dlg.rx.try_recv() {
                dlg.result = Some(r);
            }
        }
        let p = self.pal;
        let mut close = false;
        let mut apply: Option<(Vec<u64>, bool)> = None;
        let progress = dlg.progress.lock().map(|g| *g).unwrap_or(0.0);

        modal(ctx, &p, "szenen", 560.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::FILM_STRIP).size(24.0).color(p.accent));
                ui.label(RichText::new(tr!("Szenen erkennen", "Detect scenes")).family(semibold()).size(20.0));
            });
            ui.label(
                RichText::new(tr!(
                    "Sucht Szenenwechsel im gewählten Bereich und teilt den Eintrag dort in einzelne Einträge.",
                    "Finds scene changes in the selected range and splits the entry there into separate entries."
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
                    egui::Grid::new("szenen-einst").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                        ui.label(RichText::new(tr!("Empfindlichkeit", "Sensitivity")).color(p.muted));
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(tr!("gering", "low")).size(12.0).color(p.faint));
                            ui.add(egui::Slider::new(&mut dlg.sensitivity, 0.0..=1.0).show_value(false));
                            ui.label(RichText::new(tr!("hoch", "high")).size(12.0).color(p.faint));
                        });
                        ui.end_row();
                        ui.label(RichText::new(tr!("Mindestlänge", "Minimum length")).color(p.muted));
                        let dv = egui::DragValue::new(&mut dlg.min_secs)
                            .range(0.5..=600.0)
                            .speed(0.1)
                            .custom_formatter(|v, _| format!("{} s", discslim::i18n::decimal(format!("{v:.1}"))))
                            .custom_parser(|s| s.trim().trim_end_matches('s').trim().replace(',', ".").parse().ok());
                        ui.add(dv).on_hover_text(tr!("Kürzere Szenen werden mit der Nachbarszene zusammengefasst", "Shorter scenes are merged with the neighbouring scene"));
                        ui.end_row();
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
                    // Zeitleiste mit den Wechseln
                    let (rect, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 26.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, CornerRadius::same(6), p.track);
                    let span = scan.end_ms.saturating_sub(scan.start_ms).max(1) as f32;
                    for t in &cuts {
                        let x = rect.left() + rect.width() * (t.saturating_sub(scan.start_ms)) as f32 / span;
                        ui.painter().line_segment(
                            [egui::pos2(x, rect.top() + 3.0), egui::pos2(x, rect.bottom() - 3.0)],
                            Stroke::new(2.0, p.accent),
                        );
                    }
                    let (row, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 16.0), egui::Sense::hover());
                    let font = egui::FontId::proportional(11.5);
                    ui.painter().text(row.left_center(), egui::Align2::LEFT_CENTER, fmt_time(scan.start_ms), font.clone(), p.faint);
                    ui.painter().text(row.right_center(), egui::Align2::RIGHT_CENTER, fmt_time(scan.end_ms), font, p.faint);
                    ui.add_space(4.0);
                    let n = cuts.len();
                    let text = if n == 0 {
                        tr!("Keine Szenenwechsel gefunden. Versuche eine höhere Empfindlichkeit.", "No scene changes found. Try a higher sensitivity.").to_string()
                    } else {
                        trf!(
                            "{} gefunden, das ergibt {} Einträge.",
                            "{} found, giving {} entries.",
                            discslim::i18n::count(n, "Szenenwechsel", "Szenenwechsel", "scene change", "scene changes"),
                            n + 1
                        )
                    };
                    ui.label(RichText::new(text).family(semibold()));
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        let label = trf!("In {} Einträge aufteilen", "Split into {} entries", n + 1);
                        if primary_button(ui, &p, &label, n > 0).clicked() {
                            apply = Some((cuts.clone(), dlg.exact));
                        }
                        if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                            close = true;
                        }
                    });
                }
            }
        });

        let entry = dlg.entry;
        if let Some((cuts, exact)) = apply {
            if let (Some(disc), Some(e)) = (self.disc.clone(), self.plan.reauthor.get(entry).cloned()) {
                let parts = split_entry(&disc, &e, &cuts, exact);
                let n = parts.len();
                self.plan.reauthor.splice(entry..=entry, parts);
                self.sel = Selection::Entry(entry);
                self.preview.dirty = true;
                self.notice = Some((trf!("In {n} Einträge aufgeteilt", "Split into {n} entries"), Instant::now()));
            }
            close = true;
        }
        if close {
            self.scene_dlg = None;
        }
    }
}
