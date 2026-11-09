//! Programmsymbol: eine schlichte Scheibe. Wird zur Laufzeit für das Fenster und beim
//! Bauen (build.rs) für das Symbol der Exe-Datei gezeichnet, damit beide gleich aussehen.

/// RGBA-Pixel des Symbols in der Größe `size` × `size`.
pub fn disc_rgba(size: u32) -> Vec<u8> {
    // Die Maße sind für 64 Pixel entworfen und werden mitskaliert.
    let k = size as f32 / 64.0;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let c = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let (dx, dy) = (x as f32 - c, y as f32 - c);
            let r = (dx * dx + dy * dy).sqrt();
            let on = |lo: f32, hi: f32| ((r - lo * k + 0.5).clamp(0.0, 1.0)) * ((hi * k - r + 0.5).clamp(0.0, 1.0));
            let disc = on(10.0, 30.5);
            let hub = on(0.0, 4.5);
            let shine = {
                let a = dy.atan2(dx);
                0.75 + 0.25 * (a * 2.0).cos()
            };
            let alpha = disc.max(hub);
            rgba.extend_from_slice(&[
                (92.0 * shine) as u8,
                (137.0 * shine) as u8,
                (255.0 * shine) as u8,
                (alpha * 255.0) as u8,
            ]);
        }
    }
    rgba
}
