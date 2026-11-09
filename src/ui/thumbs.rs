//! Kleine Vorschaubilder für Schnittanfang und -ende, im Hintergrund erzeugt, und eine
//! kurze Endlosschleife, solange die Maus über einem Bild steht.

use std::collections::{HashMap, HashSet};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};

use eframe::egui::{self, ColorImage, TextureHandle, TextureOptions};

use discslim::dvd::source::Source;
use discslim::engine::preview::{self, PreviewCmd, PreviewHandle, Segment};

/// Titel, Kapitelbereich und Zeit (Millisekunden ab Bereichsbeginn).
pub type ThumbKey = (usize, usize, usize, u64);

struct Request {
    key: ThumbKey,
    source: Source,
    segments: Vec<Segment>,
}

pub struct Thumbs {
    tx: Sender<Request>,
    rx: Receiver<(ThumbKey, ColorImage)>,
    cache: HashMap<ThumbKey, TextureHandle>,
    requested: HashSet<ThumbKey>,
}

const THUMB_W: usize = 128;

/// Verkleinert ein RGBA-Bild auf Vorschaugröße (Mittelwert über Blöcke).
fn shrink(rgba: &[u8], w: usize, h: usize, aspect: f32) -> ColorImage {
    let tw = THUMB_W;
    let th = ((tw as f32 / aspect.max(0.5)).round() as usize).max(8);
    let mut out = Vec::with_capacity(tw * th * 4);
    for y in 0..th {
        let y0 = y * h / th;
        let y1 = ((y + 1) * h / th).max(y0 + 1);
        for x in 0..tw {
            let x0 = x * w / tw;
            let x1 = ((x + 1) * w / tw).max(x0 + 1);
            let mut acc = [0u32; 3];
            let mut n = 0u32;
            for yy in (y0..y1).step_by(2) {
                for xx in (x0..x1).step_by(2) {
                    let i = (yy * w + xx) * 4;
                    acc[0] += u32::from(rgba[i]);
                    acc[1] += u32::from(rgba[i + 1]);
                    acc[2] += u32::from(rgba[i + 2]);
                    n += 1;
                }
            }
            let n = n.max(1);
            out.extend_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, 255]);
        }
    }
    ColorImage::from_rgba_unmultiplied([tw, th], &out)
}

impl Thumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (res_tx, rx) = channel();
        std::thread::spawn(move || {
            for req in req_rx.iter() {
                let handle = preview::spawn(req.source, req.segments, 0.0, None, 0, None);
                let _ = handle.cmd.send(PreviewCmd::SeekTime { ms: req.key.3 as f64, exact: true, id: 1 });
                let deadline = Instant::now() + Duration::from_secs(6);
                while Instant::now() < deadline {
                    match handle.frames.recv_timeout(Duration::from_millis(200)) {
                        Ok(f) if f.seek_id == 1 && !f.eos => {
                            let img = shrink(&f.rgba, f.width, f.height, f.aspect);
                            if res_tx.send((req.key, img)).is_err() {
                                return;
                            }
                            ctx.request_repaint();
                            break;
                        }
                        Ok(_) => {}
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        Err(_) => break,
                    }
                }
            }
        });
        Self { tx, rx, cache: HashMap::new(), requested: HashSet::new() }
    }

    /// Fertige Bilder übernehmen; einmal je Frame aufrufen.
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((key, img)) = self.rx.try_recv() {
            let tex = ctx.load_texture(format!("thumb-{key:?}"), img, TextureOptions::LINEAR);
            self.cache.insert(key, tex);
        }
    }

    pub fn clear(&mut self) {
        self.cache.clear();
        self.requested.clear();
    }

    /// Liefert das Bild, falls schon fertig; sonst wird es angefordert.
    pub fn get(&mut self, key: ThumbKey, source: &Source, segments: impl FnOnce() -> Vec<Segment>) -> Option<egui::TextureId> {
        if let Some(t) = self.cache.get(&key) {
            return Some(t.id());
        }
        if self.requested.insert(key) {
            let _ = self.tx.send(Request { key, source: source.clone(), segments: segments() });
        }
        None
    }
}

/// Endlosschleife über wenige Sekunden in einem Vorschaubild.
pub struct HoverLoop {
    /// Eintrag und Seite (0 = Anfang, 1 = Ende)
    pub key: (usize, u8),
    handle: PreviewHandle,
    texture: Option<TextureHandle>,
    range: (f64, f64),
    seek_id: u64,
    next_at: Instant,
    pub seen_this_frame: bool,
}

impl HoverLoop {
    pub fn start(key: (usize, u8), source: Source, segments: Vec<Segment>, range: (f64, f64)) -> Self {
        let handle = preview::spawn(source, segments, 0.0, None, 0, None);
        let _ = handle.cmd.send(PreviewCmd::SeekTime { ms: range.0, exact: true, id: 1 });
        Self { key, handle, texture: None, range, seek_id: 1, next_at: Instant::now(), seen_this_frame: true }
    }

    pub fn texture(&self) -> Option<egui::TextureId> {
        self.texture.as_ref().map(|t| t.id())
    }

    pub fn pump(&mut self, ctx: &egui::Context) {
        ctx.request_repaint_after(Duration::from_millis(10));
        if Instant::now() < self.next_at {
            return;
        }
        while let Ok(f) = self.handle.frames.try_recv() {
            if f.seek_id != self.seek_id {
                continue;
            }
            if f.eos || f.time_ms >= self.range.1 {
                // Von vorn
                self.seek_id += 1;
                let _ = self.handle.cmd.send(PreviewCmd::SeekTime { ms: self.range.0, exact: true, id: self.seek_id });
                return;
            }
            let img = shrink(&f.rgba, f.width, f.height, f.aspect);
            match self.texture.as_mut() {
                Some(t) => t.set(img, TextureOptions::LINEAR),
                None => self.texture = Some(ctx.load_texture("schleife", img, TextureOptions::LINEAR)),
            }
            self.next_at = Instant::now() + Duration::from_secs_f64(f.duration);
            return;
        }
    }
}
