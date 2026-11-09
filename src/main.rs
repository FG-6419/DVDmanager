#![cfg_attr(not(test), windows_subsystem = "windows")]

mod ui;

use std::path::PathBuf;

use eframe::egui;

/// Programmsymbol: eine schlichte Scheibe, zur Laufzeit gezeichnet.
fn app_icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = Vec::with_capacity((SIZE * SIZE * 4) as usize);
    let c = (SIZE as f32 - 1.0) / 2.0;
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let on = |lo: f32, hi: f32| ((r - lo + 0.5).clamp(0.0, 1.0)) * ((hi - r + 0.5).clamp(0.0, 1.0));
            let disc = on(10.0, 30.5);
            let hub = on(0.0, 4.5);
            let shine = {
                let a = dy.atan2(dx);
                0.75 + 0.25 * (a * 2.0).cos()
            };
            let alpha = disc.max(hub);
            let px = [
                (92.0 * shine) as u8,
                (137.0 * shine) as u8,
                (255.0 * shine) as u8,
                (alpha * 255.0) as u8,
            ];
            rgba.extend_from_slice(&px);
        }
    }
    egui::IconData { rgba, width: SIZE, height: SIZE }
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
