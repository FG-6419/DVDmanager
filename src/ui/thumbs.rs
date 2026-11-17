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

/// Mitlaufendes Vorschaubild beim Einlesen oder Export. Ein eigener Thread holt etwa alle
/// Viertelsekunde das zuletzt gelesene Stück ab und dekodiert es; er endet von selbst,
/// sobald Arbeit und Anzeige vorbei sind.
pub struct LiveView {
    pub frame: std::sync::Arc<discslim::engine::live::LiveFrame>,
    rx: Receiver<ColorImage>,
    tex: Option<TextureHandle>,
}

impl LiveView {
    pub fn start(enabled: bool) -> Self {
        let frame = discslim::engine::live::LiveFrame::new(enabled);
        let (tx, rx) = channel();
        let f2 = frame.clone();
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_millis(250));
            // Nur noch dieser Thread hält das Bild: Arbeit und Anzeige sind vorbei.
            if std::sync::Arc::strong_count(&f2) <= 1 {
                return;
            }
            let Some(data) = f2.take() else { continue };
            let img = std::panic::catch_unwind(|| discslim::engine::live::decode(&data)).ok().flatten();
            if let Some((rgba, w, h)) = img {
                if tx.send(ColorImage::from_rgba_unmultiplied([w, h], &rgba)).is_err() {
                    return;
                }
            }
        });
        Self { frame, rx, tex: None }
    }

    pub fn enabled(&self) -> bool {
        self.frame.enabled()
    }

    pub fn set_enabled(&mut self, on: bool) {
        self.frame.set_enabled(on);
        if !on {
            self.tex = None;
        }
    }

    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok(img) = self.rx.try_recv() {
            if self.frame.enabled() {
                self.tex = Some(ctx.load_texture("live-bild", img, TextureOptions::LINEAR));
            }
        }
    }

    pub fn texture(&self) -> Option<(egui::TextureId, egui::Vec2)> {
        self.tex.as_ref().map(|t| (t.id(), t.size_vec2()))
    }
}

/// Dekodiert ein einzelnes Bild an einer Stelle und verkleinert es. Blockiert bis zu `timeout`.
pub fn grab_frame(source: Source, segments: Vec<Segment>, ms: f64, timeout: Duration) -> Option<ColorImage> {
    let handle = preview::spawn(source, segments, 0.0, None, 0, None);
    let _ = handle.cmd.send(PreviewCmd::SeekTime { ms, exact: true, id: 1 });
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        match handle.frames.recv_timeout(Duration::from_millis(200)) {
            Ok(f) if f.seek_id == 1 && !f.eos => return Some(shrink(&f.rgba, f.width, f.height, f.aspect)),
            Ok(_) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    None
}

impl Thumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let (tx, req_rx) = channel::<Request>();
        let (res_tx, rx) = channel();
        std::thread::spawn(move || {
            for req in req_rx.iter() {
                if let Some(img) = grab_frame(req.source, req.segments, req.key.3 as f64, Duration::from_secs(6)) {
                    if res_tx.send((req.key, img)).is_err() {
                        return;
                    }
                    ctx.request_repaint();
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

/// Bild im Filmstreifen der Zeitleiste: Quelle (Titel/Kapitelbereich), Zeit, bildgenau.
pub type StripKey = (u64, u64, bool);

#[derive(Default)]
struct StripShared {
    /// Quellen, für die gerade Bilder gebraucht werden (bei "Alle Schnitte" mehrere)
    srcs: Vec<(u64, Source, Vec<Segment>)>,
    /// Noch fehlende Bilder, wichtigste zuerst
    want: Vec<StripKey>,
    /// Bild, an dem der Hintergrund-Thread gerade arbeitet
    busy: Option<StripKey>,
}

/// Standbilder für den Filmstreifen der Zeitleiste. Ein Hintergrund-Thread arbeitet die
/// gerade sichtbaren, noch fehlenden Bilder ab; fertige bleiben im Zwischenspeicher.
pub struct StripThumbs {
    shared: std::sync::Arc<(std::sync::Mutex<StripShared>, std::sync::Condvar)>,
    rx: Receiver<(StripKey, ColorImage)>,
    cache: HashMap<StripKey, TextureHandle>,
    order: std::collections::VecDeque<StripKey>,
}

/// So viele Bilder bleiben höchstens im Zwischenspeicher (je etwa 40 KB).
const STRIP_CACHE: usize = 900;

impl StripThumbs {
    pub fn new(ctx: egui::Context) -> Self {
        let shared = std::sync::Arc::new((std::sync::Mutex::new(StripShared::default()), std::sync::Condvar::new()));
        let (tx, rx) = channel();
        let sh = shared.clone();
        std::thread::spawn(move || loop {
            let (key, source, segments) = {
                let (lock, cv) = &*sh;
                let Ok(mut g) = lock.lock() else { return };
                loop {
                    // Bilder ohne bekannte Quelle entfallen
                    let tags: Vec<u64> = g.srcs.iter().map(|s| s.0).collect();
                    g.want.retain(|k| tags.contains(&k.0));
                    if !g.want.is_empty() {
                        break;
                    }
                    g = match cv.wait(g) {
                        Ok(g) => g,
                        Err(_) => return,
                    };
                }
                let key = g.want.remove(0);
                g.busy = Some(key);
                let s = g.srcs.iter().find(|s| s.0 == key.0).expect("Quelle geprüft");
                (key, s.1.clone(), s.2.clone())
            };
            let img = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| preview::still(&source, &segments, key.1 as f64, key.2)))
                .ok()
                .flatten()
                .map(|f| shrink(&f.rgba, f.width, f.height, f.aspect))
                // Kein Bild an dieser Stelle: schwarze Kachel, damit nicht immer wieder angefragt wird
                .unwrap_or_else(|| ColorImage::from_rgba_unmultiplied([4, 3], &[16, 16, 16, 255].repeat(12)));
            if let Ok(mut g) = sh.0.lock() {
                g.busy = None;
            }
            if tx.send((key, img)).is_err() {
                return;
            }
            ctx.request_repaint();
        });
        Self { shared, rx, cache: HashMap::new(), order: std::collections::VecDeque::new() }
    }

    /// Fertige Bilder übernehmen; einmal je Frame aufrufen.
    pub fn pump(&mut self, ctx: &egui::Context) {
        while let Ok((key, img)) = self.rx.try_recv() {
            let tex = ctx.load_texture(format!("streifen-{key:?}"), img, TextureOptions::LINEAR);
            if self.cache.insert(key, tex).is_none() {
                self.order.push_back(key);
            }
            while self.order.len() > STRIP_CACHE {
                if let Some(old) = self.order.pop_front() {
                    self.cache.remove(&old);
                }
            }
        }
    }

    pub fn get(&self, key: &StripKey) -> Option<&TextureHandle> {
        self.cache.get(key)
    }

    /// Ein schon fertiges Bild derselben Quelle, höchstens `within` ms von `ms` entfernt.
    pub fn nearest(&self, tag: u64, ms: f64, within: f64) -> Option<&TextureHandle> {
        self.cache
            .iter()
            .filter(|(k, _)| k.0 == tag)
            .map(|(k, t)| ((k.1 as f64 - ms).abs(), t))
            .filter(|(d, _)| *d <= within)
            .min_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal))
            .map(|(_, t)| t)
    }

    /// Legt fest, welche Bilder gebraucht werden (wichtigste zuerst); ersetzt die vorige Liste.
    /// `sources`: Quelle (Kennung, Abschnitte) für jede Kennung, die in `keys` vorkommt.
    pub fn request(&mut self, source: &Source, sources: &[(u64, &[Segment])], keys: Vec<StripKey>) {
        let (lock, cv) = &*self.shared;
        let Ok(mut g) = lock.lock() else { return };
        let same = g.srcs.len() == sources.len() && g.srcs.iter().zip(sources).all(|(a, b)| a.0 == b.0);
        if !same {
            g.srcs = sources.iter().map(|(t, s)| (*t, source.clone(), s.to_vec())).collect();
        }
        let busy = g.busy;
        let want: Vec<StripKey> = keys.into_iter().filter(|k| !self.cache.contains_key(k) && Some(*k) != busy).collect();
        if want != g.want {
            g.want = want;
            cv.notify_one();
        }
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
    /// Wofür die Schleife läuft (Titel, Kapitel von/bis, Bereich); ändert sich das, neu starten
    pub tag: (usize, usize, usize, u64, u64),
}

impl HoverLoop {
    pub fn start(key: (usize, u8), source: Source, segments: Vec<Segment>, range: (f64, f64)) -> Self {
        let handle = preview::spawn(source, segments, 0.0, None, 0, None);
        let _ = handle.cmd.send(PreviewCmd::SeekTime { ms: range.0, exact: true, id: 1 });
        Self { key, handle, texture: None, range, seek_id: 1, next_at: Instant::now(), seen_this_frame: true, tag: (0, 0, 0, 0, 0) }
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
