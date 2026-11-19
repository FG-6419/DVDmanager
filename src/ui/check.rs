//! „Datei prüfen“: durchleuchtet VOB- und MPEG-Dateien im Hintergrund und zeigt einen
//! Bericht: zerrissene oder verfälschte Tonrahmen, Zeitsprünge, Pakete, die zu spät beim
//! Player ankommen (dort stockt die Wiedergabe), und eine rückwärts laufende Systemuhr.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use eframe::egui::{self, Align, Layout, RichText};
use egui_phosphor::regular as ic;

use discslim::engine::inspect::{inspect, CheckReport, InspectOpts};

use super::dialogs::{modal, progress_bar};
use super::style::{primary_button, semibold, soft_button};
use super::App;

pub(super) struct FileCheck {
    files: Vec<PathBuf>,
    /// Gesamtfortschritt 0..1 und die gerade geprüfte Datei
    progress: Arc<Mutex<(f32, usize)>>,
    cancel: Arc<AtomicBool>,
    rx: Receiver<Vec<(PathBuf, Result<CheckReport, String>)>>,
    result: Option<Vec<(PathBuf, Result<CheckReport, String>)>>,
}

/// Zeitangaben einer Liste, höchstens `max` Stück: „6:40, 9:37 … (+12)“.
fn times(list: &[f64], max: usize) -> String {
    let fmt = |s: f64| {
        let s = s.max(0.0) as u64;
        if s >= 3600 {
            format!("{}:{:02}:{:02}", s / 3600, s / 60 % 60, s % 60)
        } else {
            format!("{}:{:02}", s / 60, s % 60)
        }
    };
    let mut out: Vec<String> = list.iter().take(max).map(|s| fmt(*s)).collect();
    if list.len() > max {
        out.push(format!("… (+{})", list.len() - max));
    }
    out.join(", ")
}

/// Befunde eines Berichts als Zeilen (Überschrift, Erklärung, Stellen).
pub(super) fn findings(r: &CheckReport) -> Vec<(String, &'static str, String)> {
    let mut out = Vec::new();
    let mut add = |list: &[f64], head: String, why: &'static str| {
        if !list.is_empty() {
            out.push((head, why, times(list, 8)));
        }
    };
    add(
        &r.late_audio,
        trf!("Ton kommt zu spät an ({} Stellen)", "Audio arrives too late ({} places)", r.late_audio.len()),
        tr!("Die Tonpakete liegen in der Datei hinter ihrer Abspielzeit. Viele Player stocken dort oder setzen kurz aus.", "The audio packets lie behind their playback time in the file. Many players stutter or drop out there."),
    );
    add(
        &r.late_video,
        trf!("Bild kommt zu spät an ({} Stellen)", "Video arrives too late ({} places)", r.late_video.len()),
        tr!("Bilddaten kommen erst nach ihrem Dekodier-Zeitpunkt. Das Bild kann dort hängen.", "Picture data arrives after its decoding time. The picture may freeze there."),
    );
    add(
        &r.crc_errors,
        trf!("Verfälschte Tonrahmen ({} Stellen)", "Corrupted audio frames ({} places)", r.crc_errors.len()),
        tr!("Die Prüfsumme der AC3-Rahmen stimmt nicht; meist hört man Knacken oder Aussetzer.", "The AC3 frame checksum is wrong; usually you hear clicks or dropouts."),
    );
    add(
        &r.audio_breaks,
        trf!("Zerrissene Tonrahmen ({} Stellen)", "Broken audio frames ({} places)", r.audio_breaks.len()),
        tr!("Bytes, die zu keinem vollständigen Rahmen gehören, z. B. nach einem Schnitt mitten im Rahmen.", "Bytes that belong to no complete frame, e.g. after a cut in the middle of a frame."),
    );
    add(
        &r.time_jumps,
        trf!("Zeitsprünge im Ton ({} Stellen)", "Time jumps in the audio ({} places)", r.time_jumps.len()),
        tr!("Die Zeitstempel des Tons springen; Player verlieren dort oft kurz die Synchronität.", "The audio timestamps jump; players often briefly lose sync there."),
    );
    add(
        &r.scr_back,
        trf!("Systemuhr läuft rückwärts ({} Stellen)", "System clock runs backwards ({} places)", r.scr_back.len()),
        tr!("Die Zeitangaben der Pakete fallen zurück; manche Player springen oder halten an.", "The packet clock falls back; some players jump or stop."),
    );
    out
}

impl App {
    /// Fragt nach VOB-/MPEG-Dateien und prüft sie im Hintergrund.
    pub(super) fn open_file_check(&mut self, ctx: &egui::Context) {
        let Some(files) = self
            .load_dialog(tr!("Dateien prüfen", "Check files"))
            .add_filter(tr!("Videodatei (MPEG-2)", "Video file (MPEG-2)"), &["vob", "mpg", "mpeg", "m2p"])
            .add_filter(tr!("Alle Dateien", "All files"), &["*"])
            .pick_files()
        else {
            return;
        };
        if files.is_empty() {
            return;
        }
        self.note_load(&files[0]);
        self.start_file_check(ctx, files);
    }

    pub(super) fn start_file_check(&mut self, ctx: &egui::Context, files: Vec<PathBuf>) {
        let progress = Arc::new(Mutex::new((0f32, 0usize)));
        let cancel = Arc::new(AtomicBool::new(false));
        let (tx, rx) = channel();
        let (p2, c2, ctx2, list) = (progress.clone(), cancel.clone(), ctx.clone(), files.clone());
        std::thread::spawn(move || {
            let n = list.len().max(1);
            let mut out = Vec::new();
            let mut last = Instant::now();
            for (k, f) in list.iter().enumerate() {
                // Einzelne Dateien haben eine fortlaufende Uhr; VOBs aus einem DVD-Ordner nicht
                // (dort beginnt sie an jeder Zelle neu).
                let continuous = discslim::engine::import::is_media_path(f);
                let opts = InspectOpts { verify: true, continuous, sheet: None };
                let res = inspect(&[vec![f.clone()]], &opts, &mut |x| {
                    if let Ok(mut g) = p2.lock() {
                        *g = ((k as f32 + x) / n as f32, k);
                    }
                    if last.elapsed().as_millis() > 100 {
                        last = Instant::now();
                        ctx2.request_repaint();
                    }
                    !c2.load(Ordering::Relaxed)
                });
                if c2.load(Ordering::Relaxed) {
                    break;
                }
                out.push((f.clone(), res.and_then(|(r, _)| r.ok_or_else(|| "–".to_string()))));
            }
            let _ = tx.send(out);
            ctx2.request_repaint();
        });
        self.file_check = Some(FileCheck { files, progress, cancel, rx, result: None });
    }

    pub(super) fn dialog_file_check(&mut self, ctx: &egui::Context) {
        let Some(mut dlg) = self.file_check.take() else { return };
        if dlg.result.is_none() {
            if let Ok(r) = dlg.rx.try_recv() {
                dlg.result = Some(r);
            }
        }
        let p = self.pal;
        let mut close = false;
        let mut again = false;
        modal(ctx, &p, "dateipruefung", 620.0, |ui| {
            ui.horizontal(|ui| {
                ui.label(RichText::new(ic::STETHOSCOPE).size(24.0).color(p.accent));
                ui.label(RichText::new(tr!("Datei prüfen", "Check file")).family(semibold()).size(20.0));
            });
            ui.add_space(8.0);
            match &dlg.result {
                None => {
                    let (f, k) = dlg.progress.lock().map(|g| *g).unwrap_or((0.0, 0));
                    let name = dlg.files.get(k).and_then(|f| f.file_name()).map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                    ui.label(RichText::new(trf!("Wird gelesen: {}", "Reading: {}", name)).color(p.muted));
                    ui.add_space(6.0);
                    progress_bar(ui, &p, f);
                    ui.add_space(12.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if soft_button(ui, &p, tr!("Abbrechen", "Cancel")).clicked() {
                            dlg.cancel.store(true, Ordering::Relaxed);
                            close = true;
                        }
                    });
                }
                Some(list) => {
                    egui::ScrollArea::vertical().id_salt("pruefbericht").max_height(460.0).auto_shrink([false, true]).show(ui, |ui| {
                        for (path, res) in list {
                            let name = path.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default();
                            match res {
                                Err(e) => {
                                    ui.label(RichText::new(format!("{}  {name}", ic::WARNING)).family(semibold()).color(p.bad));
                                    ui.label(RichText::new(e).size(13.0).color(p.muted));
                                }
                                Ok(r) => {
                                    let dur = times(&[r.seconds], 1);
                                    let tracks = discslim::i18n::count(r.tracks, "Tonspur", "Tonspuren", "audio track", "audio tracks");
                                    let found = findings(r);
                                    if found.is_empty() {
                                        ui.label(RichText::new(format!("{}  {name}", ic::CHECK_CIRCLE)).family(semibold()).color(p.good));
                                        ui.label(
                                            RichText::new(trf!("Keine Auffälligkeiten · {} · {}", "No problems found · {} · {}", dur, tracks)).size(13.0).color(p.muted),
                                        );
                                    } else {
                                        ui.label(RichText::new(format!("{}  {name}", ic::WARNING)).family(semibold()).color(p.warn));
                                        ui.label(RichText::new(format!("{dur} · {tracks}")).size(13.0).color(p.muted));
                                        for (head, why, at) in found {
                                            ui.add_space(4.0);
                                            ui.label(RichText::new(format!("•  {head}")).size(13.5));
                                            ui.label(RichText::new(why).size(12.5).color(p.muted));
                                            ui.label(RichText::new(trf!("bei {}", "at {}", at)).size(12.5).color(p.faint));
                                        }
                                    }
                                }
                            }
                            ui.add_space(10.0);
                        }
                    });
                    ui.label(
                        RichText::new(tr!(
                            "Geprüft wird der Aufbau der Datei. Fehler im Ton selbst (z. B. schon beim Erstellen eingebrannte Aussetzer) lassen sich so nicht finden.",
                            "This checks the structure of the file. Faults in the sound itself (e.g. dropouts baked in when it was made) cannot be found this way."
                        ))
                        .size(12.0)
                        .color(p.faint),
                    );
                    ui.add_space(10.0);
                    ui.with_layout(Layout::right_to_left(Align::Min), |ui| {
                        if primary_button(ui, &p, tr!("Schließen", "Close"), true).clicked() {
                            close = true;
                        }
                        if soft_button(ui, &p, &trf!("{}  Weitere prüfen …", "{}  Check more …", ic::STETHOSCOPE)).clicked() {
                            close = true;
                            again = true;
                        }
                    });
                }
            }
        });
        if !close {
            self.file_check = Some(dlg);
        }
        if again {
            self.open_file_check(ctx);
        }
    }
}
