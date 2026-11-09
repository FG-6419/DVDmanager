#![cfg_attr(not(test), windows_subsystem = "windows")]

mod icon;
mod ui;

use std::path::PathBuf;

use eframe::egui;

/// Programmsymbol für das Fenster (gleiche Zeichnung wie das Symbol der Exe).
fn app_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    egui::IconData { rgba: icon::disc_rgba(SIZE), width: SIZE, height: SIZE }
}

fn main() -> eframe::Result {
    let mut screenshot = None;
    let mut open = None;
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match a.to_str() {
            Some("--screenshot") => screenshot = args.next().map(PathBuf::from),
            Some("--open") => open = args.next().map(PathBuf::from),
            _ => {
                // Ein einfach übergebener Pfad (z. B. "Öffnen mit") wird geöffnet.
                if open.is_none() {
                    open = Some(PathBuf::from(a));
                }
            }
        }
    }

    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("DVDmanager")
            .with_inner_size([1320.0, 820.0])
            .with_min_inner_size([1040.0, 660.0])
            .with_drag_and_drop(true)
            .with_icon(app_icon()),
        ..Default::default()
    };
    // Der interne Name bleibt, damit gespeicherte Einstellungen erhalten bleiben.
    eframe::run_native(
        "DiscSlim",
        options,
        Box::new(move |cc| Ok(Box::new(ui::App::new(cc, screenshot, open)))),
    )
}
