//! Gehaltene Maustaste auf einem Knopf: egui meldet "gedrückt" nur bis 0,8 s,
//! die Maustaste selbst bleibt aber unten. Darauf baut das fortlaufende Vor/Zurück.
use eframe::egui;

#[test]
fn held_button_stays_down() {
    let ctx = egui::Context::default();
    let pos = egui::pos2(30.0, 20.0);
    let mut log = Vec::new();
    for frame in 0..40 {
        let t = frame as f64 * 0.1;
        let mut input = egui::RawInput { time: Some(t), ..Default::default() };
        input.screen_rect = Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(400.0, 300.0)));
        input.events.push(egui::Event::PointerMoved(pos));
        if frame == 1 {
            input.events.push(egui::Event::PointerButton {
                pos,
                button: egui::PointerButton::Primary,
                pressed: true,
                modifiers: Default::default(),
            });
        }
        let mut out = ctx.run_ui(input, |ui| {
            {
                let r = ui.allocate_response(egui::vec2(60.0, 40.0), egui::Sense::click());
                let primary = ui.ctx().input(|i| i.pointer.primary_down());
                log.push((t, r.is_pointer_button_down_on(), primary));
            }
        });
        out.textures_delta.clear();
    }
    let late: Vec<_> = log.iter().filter(|(t, _, _)| *t > 2.0).collect();
    assert!(late.iter().all(|(_, _, p)| *p), "Maustaste muss unten bleiben");
    println!("egui 'gedrückt' nach 2 s: {}", late.iter().any(|(_, d, _)| *d));
}

