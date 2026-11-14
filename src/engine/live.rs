//! Mitlaufendes Vorschaubild: zeigt beim Einlesen und beim Export die Stelle, die gerade
//! verarbeitet wird.
//!
//! Der Arbeits-Thread meldet sich mit [`bind`] an. Die Lese-Schleifen rufen [`offer`] mit den
//! gerade gelesenen Daten (MPEG-Programmstrom) auf; das kostet nichts, solange niemand
//! zusieht. Ist das Bild eingeschaltet, wird höchstens etwa jede Sekunde ein Stück
//! aufbewahrt. Die Oberfläche holt es mit [`LiveFrame::take`] ab und dekodiert es mit
//! [`decode`] in ihrem eigenen Hintergrund-Thread, damit die Arbeit nicht gebremst wird.

use std::cell::RefCell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::mpeg2::decode::Decoder;

/// Abstand zwischen zwei aufbewahrten Stücken
const INTERVAL: Duration = Duration::from_millis(900);
/// Höchstens so viele Bytes je Stück (reicht für zwei Bildgruppen)
const MAX_CHUNK: usize = 1536 * 1024;
/// Breite des fertigen Bildes
pub const LIVE_W: usize = 240;

#[derive(Default)]
pub struct LiveFrame {
    enabled: AtomicBool,
    slot: Mutex<(Option<Vec<u8>>, Option<Instant>)>,
}

impl LiveFrame {
    pub fn new(enabled: bool) -> Arc<Self> {
        let l = Self::default();
        l.enabled.store(enabled, Ordering::Relaxed);
        Arc::new(l)
    }

    pub fn set_enabled(&self, on: bool) {
        self.enabled.store(on, Ordering::Relaxed);
        if !on {
            if let Ok(mut s) = self.slot.lock() {
                *s = (None, None);
            }
        }
    }

    pub fn enabled(&self) -> bool {
        self.enabled.load(Ordering::Relaxed)
    }

    /// Holt das zuletzt aufbewahrte Stück (falls seit dem letzten Abholen ein neues kam).
    pub fn take(&self) -> Option<Vec<u8>> {
        self.slot.lock().ok().and_then(|mut s| s.0.take())
    }

    fn store(&self, data: &[u8]) {
        if !self.enabled() {
            return;
        }
        let Ok(mut s) = self.slot.lock() else { return };
        if s.1.is_some_and(|t| t.elapsed() < INTERVAL) {
            return;
        }
        s.1 = Some(Instant::now());
        s.0 = Some(data[..data.len().min(MAX_CHUNK)].to_vec());
    }
}

thread_local! {
    static SINK: RefCell<Option<Arc<LiveFrame>>> = const { RefCell::new(None) };
}

/// Meldet den aktuellen Thread an (oder mit `None` ab).
pub fn bind(sink: Option<Arc<LiveFrame>>) {
    SINK.with(|s| *s.borrow_mut() = sink);
}

/// Bietet gerade gelesene Daten an. Ohne angemeldeten Zuschauer passiert nichts.
#[inline]
pub fn offer(data: &[u8]) {
    SINK.with(|s| {
        if let Some(l) = s.borrow().as_ref() {
            l.store(data);
        }
    });
}

/// Holt den Bild-Elementarstrom aus einem Programmstrom (beliebige Paketgrößen).
fn video_es(ps: &[u8]) -> Vec<u8> {
    let mut es = Vec::new();
    let mut i = 0;
    while i + 6 <= ps.len() {
        if ps[i] != 0 || ps[i + 1] != 0 || ps[i + 2] != 1 {
            i += 1;
            continue;
        }
        let id = ps[i + 3];
        match id {
            0xBA => {
                // MPEG-2: 14 Bytes plus Füllbytes, MPEG-1: 12 Bytes
                if i + 14 <= ps.len() && ps[i + 4] >> 6 == 1 {
                    i += 14 + usize::from(ps[i + 13] & 7);
                } else {
                    i += 12;
                }
            }
            0xBB..=0xFF => {
                let len = usize::from(u16::from_be_bytes([ps[i + 4], ps[i + 5]]));
                let end = (i + 6 + len).min(ps.len());
                if (0xE0..=0xEF).contains(&id) && i + 9 <= end && ps[i + 6] & 0xC0 == 0x80 {
                    let start = i + 9 + usize::from(ps[i + 8]);
                    if start < end {
                        es.extend_from_slice(&ps[start..end]);
                    }
                }
                i = end.max(i + 4);
            }
            _ => i += 4,
        }
    }
    es
}

/// Dekodiert aus einem Stück Programmstrom ein verkleinertes Bild: RGBA, Breite, Höhe.
pub fn decode(ps: &[u8]) -> Option<(Vec<u8>, usize, usize)> {
    let es = video_es(ps);
    // Ab dem ersten Sequenzkopf, höchstens zwei Bildgruppen
    let start = es.windows(4).position(|w| w == [0, 0, 1, 0xB3])?;
    let es = &es[start..];
    let mut seqs = 0;
    let mut end = es.len();
    for (k, w) in es.windows(4).enumerate() {
        if w == [0, 0, 1, 0xB3] {
            seqs += 1;
            if seqs == 3 {
                end = k;
                break;
            }
        }
    }
    let mut dec = Decoder::new();
    dec.push(&es[..end]);
    dec.flush();
    let mut frames = Vec::new();
    while let Some(f) = dec.pop_frame() {
        frames.push(f);
    }
    // Ein Bild aus der Mitte: dort sind alle Bezugsbilder vorhanden.
    let f = frames.get(frames.len() / 2)?;
    let rgba = f.to_rgba();
    let (w, h) = (f.width, f.height);
    if w == 0 || h == 0 {
        return None;
    }
    let tw = LIVE_W;
    let th = ((tw as f32 / f.aspect.max(0.5)).round() as usize).max(8);
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
                    let k = (yy * w + xx) * 4;
                    acc[0] += u32::from(rgba[k]);
                    acc[1] += u32::from(rgba[k + 1]);
                    acc[2] += u32::from(rgba[k + 2]);
                    n += 1;
                }
            }
            let n = n.max(1);
            out.extend_from_slice(&[(acc[0] / n) as u8, (acc[1] / n) as u8, (acc[2] / n) as u8, 255]);
        }
    }
    Some((out, tw, th))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offer_only_when_bound_and_enabled() {
        let l = LiveFrame::new(false);
        bind(Some(l.clone()));
        offer(b"abc");
        assert!(l.take().is_none(), "ausgeschaltet: nichts aufbewahren");
        l.set_enabled(true);
        offer(b"abc");
        assert_eq!(l.take().as_deref(), Some(&b"abc"[..]));
        offer(b"def");
        assert!(l.take().is_none(), "zu kurz danach: nichts Neues");
        bind(None);
    }
}
