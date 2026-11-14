//! Vorschaufläche: holt Einzelbilder aus dem Hintergrund und spielt sie zeitrichtig ab.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, TextureOptions, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::model::{fmt_time, Disc};
use discslim::dvd::source::Source;
use discslim::engine::plan::{CellDecision, ItemId};
use discslim::engine::audio::{AudioBus, Codec, TrackChoice};
use discslim::engine::preview::{self, menu_segments, title_segments, PreviewFrame, PreviewHandle, Segment};
use discslim::mpeg2::requant::VideoOpts;

use super::{App, Selection};

#[derive(Default)]
pub struct PreviewState {
    pub dirty: bool,
    handle: Option<PreviewHandle>,
    segments: Vec<Segment>,
    texture: Option<egui::TextureHandle>,
    aspect: f32,
    playing: bool,
    want_frame: bool,
    next_at: Option<Instant>,
    seek_id: u64,
    pub compressed: bool,
    last_opts: Option<VideoOpts>,
    total_ms: u64,
    /// Kapitel: Beginn (Millisekunden) und Bezeichnung
    chapters: Vec<(u64, String)>,
    drag: Option<f32>,
    /// Beim Ziehen auf der Zeitleiste: zuletzt angesprungene Stelle (Titelzeit) und wann
    drag_live: Option<(f64, Instant)>,
    has_frame: bool,
    /// Anzeigezeit des gezeigten Bildes
    time_ms: Option<u64>,
    /// Beginn und Ende des Schlüsselbild-Abschnitts des gezeigten Bildes
    key_t0: f64,
    key_t1: f64,
    frame_ms: f64,
    /// Schritte bildgenau (sonst von Schlüsselbild zu Schlüsselbild)
    step_exact: bool,
    /// Schnittmarken (Anteil der Länge) für die Zeitleiste
    cut_marks: (Option<f32>, Option<f32>),
    /// Länge nach dem Schnitt, falls geschnitten
    cut_len: Option<u64>,
    /// Nur den geschnittenen Bereich zeigen (so, wie er ausgegeben wird)
    pub cut_view: bool,
    /// Sichtbarer Bereich in der Schnittansicht: Anfang, Ende, bildgenau
    window: Option<(f64, f64, bool)>,
    /// Tonausgabe (gemeinsam für alle Vorschauen)
    pub audio: Option<AudioBus>,
    /// Nächstes Bild, das auf seinen Zeitpunkt wartet
    pending: Option<PreviewFrame>,
    /// Was gerade geladen ist
    loaded: Option<Selection>,
    /// "Alle Schnitte": alle Einträge nacheinander auf einer Zeitleiste
    pub program: bool,
    /// Abschnitte: Eintrag, Anfang, Ende, bildgenau
    pub prog: Vec<(usize, f64, f64, bool)>,
    /// "An Schnitten Kapitelmarken": Übergänge als Kapitel zeigen
    pub chapter_cuts: bool,
    pub prog_cur: usize,
    /// Nach dem Wechsel zum nächsten Abschnitt: dorthin springen, weiterspielen?
    pub prog_seek: Option<f64>,
    pub prog_play: Option<bool>,
    /// Stelle des letzten Rechtsklicks auf der Zeitleiste (Anteil)
    ctx_frac: Option<f32>,
    /// Gehaltener Schritt-Knopf: Richtung, gedrückt seit, letzter Schritt
    hold: Option<(i8, Instant, Instant)>,
    /// Fortlaufendes Fahren beim Halten: 1 vorwärts, -1 rückwärts, 0 aus
    scrub: i8,
    /// Nach dem Wechsel zum vorigen Abschnitt ab hier rückwärts weiterfahren
    prog_reverse: Option<f64>,
    /// Nur für Prüfbilder: Knopf gilt als gehalten
    dev_hold: bool,
    /// Übergang anhören: anhalten, sobald Abschnitt k die Zeit t erreicht (spätestens bis dann)
    pub listen_until: Option<(usize, f64, Instant)>,
    /// Zoombare Zeitleiste mit Standbildern
    pub strip: Strip,
}

/// Zustand der zoombaren Zeitleiste.
#[derive(Default)]
pub struct Strip {
    thumbs: Option<super::thumbs::StripThumbs>,
    /// Quelle der Standbilder (Titel und Kapitelbereich bzw. Menü)
    pub tag: u64,
    /// Linker Rand des sichtbaren Ausschnitts (Titelzeit) und Millisekunden je Bildpunkt
    view_lo: f64,
    ms_per_px: f64,
    /// Bereich, für den der Ausschnitt gilt; ändert er sich, wird wieder alles gezeigt
    range: (f64, f64),
    /// Beim Ziehen im Streifen: angezeigte Zeit
    drag: Option<f64>,
    drag_live: Option<(f64, Instant)>,
    /// Ziehen am Rollbalken: Abstand des Griffs zum Zeiger (Millisekunden)
    pan_grab: Option<f64>,
    /// Zeit des letzten Rechtsklicks im Streifen
    ctx_ms: Option<f64>,
    /// Abspielposition beim letzten Durchlauf (zum Nachführen des Ausschnitts)
    last_pos: f64,
    /// Lautstärkeverläufe (zuletzt gebrauchte), je Quelle und Tonspur
    waves: Vec<Wave>,
}

/// Lautstärkeverlauf einer Tonspur, im Hintergrund gelesen.
struct Wave {
    key: (u64, u8),
    data: std::sync::Arc<std::sync::Mutex<WaveData>>,
    cancel: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Default)]
struct WaveData {
    peaks: Vec<u16>,
    done_ms: f64,
    finished: bool,
    failed: bool,
}

impl Wave {
    fn start(key: (u64, u8), source: Source, segments: Vec<Segment>, track: TrackChoice, ctx: egui::Context) -> Self {
        use std::sync::atomic::Ordering;
        let data = std::sync::Arc::new(std::sync::Mutex::new(WaveData::default()));
        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let (d2, c2) = (data.clone(), cancel.clone());
        std::thread::spawn(move || {
            let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                preview::audio_peaks(&source, &segments, &track, &mut |p, done| {
                    if c2.load(Ordering::Relaxed) {
                        return false;
                    }
                    if let Ok(mut g) = d2.lock() {
                        g.peaks.clear();
                        g.peaks.extend_from_slice(p);
                        g.done_ms = done;
                    }
                    ctx.request_repaint();
                    true
                })
            }))
            .ok()
            .flatten();
            if let Ok(mut g) = d2.lock() {
                match res {
                    Some(p) => {
                        g.peaks = p;
                        g.finished = true;
                    }
                    None => g.failed = !c2.load(Ordering::Relaxed),
                }
            }
            ctx.request_repaint();
        });
        Self { key, data, cancel }
    }
}

impl Drop for Wave {
    fn drop(&mut self) {
        self.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Höhe der ausgeklappten Zeitleiste samt Werkzeugzeile.
pub const STRIP_H: f32 = 156.0;
/// Zusätzliche Höhe für die Tonspur.
pub const WAVE_H: f32 = 36.0;

/// Spitzenpegel (0..32768) als Anteil der Höhe: −48 dB … 0 dB.
fn level(v: u16) -> f32 {
    if v == 0 {
        return 0.0;
    }
    let db = 20.0 * (f32::from(v) / 32768.0).log10();
    ((db + 48.0) / 48.0).clamp(0.0, 1.0)
}

/// Wie lange vor und nach einer Schnittstelle beim Anhören gespielt wird.
pub const LISTEN_MS: f64 = 2_500.0;

impl PreviewState {
    /// Zeit des Bildes, das gerade zu sehen ist.
    pub fn current_ms(&self) -> Option<u64> {
        if self.has_frame { self.time_ms } else { None }
    }

    pub fn pause(&mut self) {
        self.set_playing(false);
    }

    /// Startet oder hält die Wiedergabe an, Ton inklusive.
    pub fn set_playing(&mut self, on: bool) {
        self.playing = on;
        if let Some(bus) = &self.audio {
            if on {
                bus.play(self.current_f() + self.frame_len());
            } else {
                bus.pause();
            }
        }
    }

    /// Nur für Prüfbilder: so tun, als würde der Rückwärts-Knopf gehalten.
    #[cfg(debug_assertions)]
    pub fn dev_hold_reverse(&mut self) {
        self.dev_hold = true;
    }

    #[cfg(debug_assertions)]
    pub fn play(&mut self) {
        self.set_playing(true);
    }

    /// Nur für Prüfläufe: Zustand der Wiedergabe als Text.
    #[cfg(debug_assertions)]
    pub fn dev_status(&self) -> String {
        format!(
            "alle={} leiste={:.0}/{:.0} drag={:?} abschnitt={} zeit={:?} bild={} spielt={} anhalten_bei={:?}",
            self.program,
            self.prog_position(),
            self.prog_offsets().1,
            self.drag,
            self.prog_cur,
            self.time_ms,
            self.has_frame,
            self.playing,
            self.listen_until.map(|l| (l.0, l.1))
        )
    }

    /// Nur für Prüfbilder: Zeitleiste auf eine Zoomstufe stellen.
    #[cfg(debug_assertions)]
    pub fn dev_zoom(&mut self, ms_per_px: f64, view_lo: f64) {
        self.strip.ms_per_px = ms_per_px;
        self.strip.view_lo = view_lo;
        self.strip.last_pos = self.current_f();
    }

    #[cfg(debug_assertions)]
    pub fn is_playing(&self) -> bool {
        self.playing
    }

    pub fn stop(&mut self) {
        self.handle = None;
        self.segments.clear();
        self.texture = None;
        self.set_playing(false);
        self.want_frame = false;
        self.has_frame = false;
        self.time_ms = None;
        self.pending = None;
        self.window = None;
        self.loaded = None;
        self.chapters.clear();
    }

    /// Springt zu einer Zeit; bildgenau oder zum Schlüsselbild davor.
    pub fn seek_time(&mut self, ms: f64, exact: bool) {
        self.seek_id += 1;
        self.want_frame = true;
        self.next_at = None;
        let ms = ms.clamp(0.0, self.total_ms as f64);
        self.time_ms = Some(ms as u64);
        self.pending = None;
        if self.playing {
            if let Some(bus) = &self.audio {
                bus.play(ms);
            }
        }
        if let Some(h) = &self.handle {
            let _ = h.cmd.send(preview::PreviewCmd::SeekTime { ms, exact, id: self.seek_id });
        }
    }

    /// Sichtbarer Zeitbereich: ganzer Titel oder nur der Schnitt.
    fn range(&self) -> (f64, f64) {
        match self.window {
            Some((a, b, _)) => (a, b.max(a + 1.0)),
            None => (0.0, self.total_ms.max(1) as f64),
        }
    }

    fn frame_len(&self) -> f64 {
        if self.frame_ms > 1.0 { self.frame_ms } else { 40.0 }
    }

    /// Übernimmt den Bereich der Schnittansicht; springt hinein, wenn das Bild außerhalb liegt.
    fn set_window(&mut self, w: Option<(f64, f64, bool)>) {
        if w == self.window {
            return;
        }
        self.window = w;
        if let Some((a, b, exact)) = w {
            let cur = self.current_f();
            if !self.has_frame || cur < a || cur >= b {
                self.seek_time(a, exact);
            }
        }
    }

    /// Prüft ein neues Bild gegen die Schnittansicht: (zeigen, vor dem Bereich).
    fn in_window(&self, f: &PreviewFrame) -> (bool, bool) {
        let Some((a, b, exact)) = self.window else { return (true, false) };
        let half = f.frame_ms / 2.0;
        let past = if exact { f.time_ms + half >= b } else { f.key_t0 >= b - 0.5 };
        let before = if exact { f.time_ms + half < a } else { f.key_t1 <= a + 0.5 };
        (!past && !before, before)
    }

    /// Gesamtlänge und Beginn jedes Abschnitts im Modus "Alle Schnitte".
    pub fn prog_offsets(&self) -> (Vec<f64>, f64) {
        let mut acc = 0.0;
        let mut offs = Vec::with_capacity(self.prog.len());
        for c in &self.prog {
            offs.push(acc);
            acc += (c.2 - c.1).max(0.0);
        }
        (offs, acc.max(1.0))
    }

    /// Position im Modus "Alle Schnitte" (Millisekunden ab Beginn aller Abschnitte).
    pub fn prog_position(&self) -> f64 {
        let (offs, _) = self.prog_offsets();
        match (self.prog.get(self.prog_cur), offs.get(self.prog_cur)) {
            (Some(c), Some(o)) => o + (self.current_f() - c.1).clamp(0.0, (c.2 - c.1).max(0.0)),
            _ => 0.0,
        }
    }

    /// Springt zu einer Stelle der gemeinsamen Zeitleiste.
    pub fn prog_seek_global(&mut self, g: f64) {
        let (offs, _) = self.prog_offsets();
        let k = offs.iter().rposition(|o| *o <= g).unwrap_or(0);
        if let (Some(c), Some(o)) = (self.prog.get(k).copied(), offs.get(k)) {
            self.prog_goto(k, c.1 + (g - o));
        }
    }

    /// Bei "Alle Schnitte" zum nächsten Abschnitt weiterschalten; false am Ende.
    fn advance_program(&mut self) -> bool {
        if !self.program || self.prog_cur + 1 >= self.prog.len() {
            return false;
        }
        let next = self.prog_cur + 1;
        let start = self.prog[next].1;
        self.prog_cur = next;
        // Zeitleiste gleich an den Anfang des nächsten Abschnitts, nicht erst mit dem ersten Bild
        self.time_ms = Some(start as u64);
        self.prog_seek = Some(start);
        self.prog_play = Some(self.playing);
        true
    }

    /// Bei "Alle Schnitte" ans Ende des vorigen Abschnitts wechseln; false, wenn es keinen gibt.
    /// `reverse`: dort rückwärts weiterfahren.
    fn previous_clip(&mut self, reverse: bool) -> bool {
        if !self.program || self.prog_cur == 0 {
            return false;
        }
        let k = self.prog_cur - 1;
        let clip = self.prog[k];
        if reverse {
            self.prog_cur = k;
            self.prog_reverse = Some(clip.2);
            self.prog_play = Some(false);
        } else {
            let end = if clip.3 { clip.2 - self.frame_len() } else { clip.2 - 1.0 };
            self.prog_goto(k, end);
        }
        true
    }

    /// Rückwärts abspielen ab `from` (die Vorschau liefert die Bilder in umgekehrter Folge).
    fn start_reverse(&mut self, from: f64) {
        self.seek_id += 1;
        self.pending = None;
        self.want_frame = false;
        self.next_at = None;
        self.scrub = -1;
        if let Some(h) = &self.handle {
            let _ = h.cmd.send(preview::PreviewCmd::Reverse { ms: from, id: self.seek_id });
        }
    }

    /// Wechselt zu Abschnitt k und springt dort zu `local` (Zeit im Titel).
    pub fn prog_goto(&mut self, k: usize, local: f64) {
        let Some(clip) = self.prog.get(k).copied() else { return };
        let local = local.clamp(clip.1, (clip.2 - self.frame_len()).max(clip.1));
        if k == self.prog_cur && self.handle.is_some() {
            self.seek_time(local, clip.3);
        } else {
            let play = self.playing;
            self.prog_cur = k;
            self.time_ms = Some(local as u64);
            self.prog_seek = Some(local);
            self.prog_play = Some(play);
        }
    }

    fn current_f(&self) -> f64 {
        self.time_ms.unwrap_or(0) as f64
    }

    /// Springt um eine Zeitspanne vor oder zurück, innerhalb des sichtbaren Bereichs.
    pub fn jump(&mut self, delta_ms: f64) {
        if self.segments.is_empty() {
            return;
        }
        let (lo, hi) = self.range();
        let target = (self.current_f() + delta_ms).clamp(lo, (hi - self.frame_len()).max(lo));
        let exact = self.step_exact;
        self.seek_time(target, exact);
    }

    /// Schritt-Knopf: ein Schritt beim Drücken, danach fortlaufend, solange er gehalten wird.
    fn hold_step(&mut self, resp: &egui::Response, dir: i8, enabled: bool) {
        // egui sieht einen Knopf nach 0,8 s Halten nicht mehr als "gedrückt" an (dann gilt es
        // nicht mehr als Klick). Deshalb: begonnen wird mit dem Druck auf den Knopf, weiter geht
        // es, solange die linke Maustaste unten bleibt.
        let dev = self.dev_hold && dir < 0;
        let holding = matches!(self.hold, Some((d, _, _)) if d == dir);
        let down = enabled
            && if holding {
                resp.ctx.input(|i| i.pointer.primary_down()) || dev
            } else {
                resp.is_pointer_button_down_on() || dev
            };
        let now = Instant::now();
        match self.hold {
            Some((d, start, last)) if d == dir => {
                if !down {
                    self.hold = None;
                    if self.scrub < 0 {
                        // Rückwärtsfahrt beenden: Vorschau wieder normal an dieser Stelle
                        let at = self.current_f();
                        self.scrub = 0;
                        self.seek_time(at, true);
                    }
                    self.scrub = 0;
                    return;
                }
                if now.duration_since(start) > Duration::from_millis(300) {
                    if self.step_exact {
                        // Bildgenau: flüssig fahren, vorwärts wie Abspielen, rückwärts Bild für Bild
                        if dir < 0 && self.scrub != -1 {
                            let at = self.current_f();
                            self.start_reverse(at);
                        } else if dir > 0 && self.scrub != 1 {
                            self.scrub = 1;
                            self.next_at = None;
                        }
                    } else if now.duration_since(last) > Duration::from_millis(180) {
                        self.hold = Some((d, start, now));
                        self.step(dir);
                    }
                }
                resp.ctx.request_repaint_after(Duration::from_millis(10));
            }
            _ => {
                if down {
                    self.hold = Some((dir, now, now));
                    self.scrub = 0;
                    self.step(dir);
                    resp.ctx.request_repaint_after(Duration::from_millis(10));
                }
            }
        }
    }

    fn step(&mut self, dir: i8) {
        if dir < 0 {
            self.step_back();
        } else {
            self.step_forward();
        }
    }

    /// Ein Bild oder ein Schlüsselbild zurück.
    pub fn step_back(&mut self) {
        self.set_playing(false);
        let (lo, _) = self.range();
        if self.step_exact {
            let target = self.current_f() - self.frame_len();
            // Am Anfang eines Abschnitts: weiter am Ende des vorigen
            if target < lo - 0.5 && self.previous_clip(false) {
                return;
            }
            self.seek_time(target.max(lo), true);
        } else {
            let target = self.key_t0 - 1.0;
            if target < lo && self.previous_clip(false) {
                return;
            }
            self.seek_time(if target < lo { lo } else { target }, false);
        }
    }

    /// Ein Bild oder ein Schlüsselbild weiter.
    pub fn step_forward(&mut self) {
        self.set_playing(false);
        if self.step_exact {
            self.want_frame = true;
        } else {
            let next = self.key_t1 + self.frame_len() / 2.0;
            if next < self.range().1 {
                self.seek_time(next, false);
            } else {
                // Am Ende eines Abschnitts: weiter mit dem nächsten
                self.advance_program();
            }
        }
    }

    fn upload(&mut self, ctx: &egui::Context, f: PreviewFrame) {
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width, f.height], &f.rgba);
        match self.texture.as_mut() {
            Some(t) => t.set(img, TextureOptions::LINEAR),
            None => self.texture = Some(ctx.load_texture("vorschau", img, TextureOptions::LINEAR)),
        }
        self.aspect = f.aspect;
        self.time_ms = Some(f.time_ms.max(0.0) as u64);
        self.key_t0 = f.key_t0;
        self.key_t1 = f.key_t1;
        self.frame_ms = f.frame_ms;
        self.has_frame = true;
        // Das Bild kommt nach dem Zeichnen dieses Durchlaufs an: gleich noch einmal
        // zeichnen, sonst erscheint es (bei angehaltener Vorschau) erst bei der
        // nächsten Mausbewegung.
        ctx.request_repaint();
    }
}

/// Kapitelanfänge eines Titels oder eines Kapitelbereichs, relativ zu dessen Beginn.
fn chapter_list(disc: &Disc, title: usize, first: usize, last: usize) -> Vec<(u64, String)> {
    let Some(t) = disc.titles.get(title) else { return Vec::new() };
    let base = t.chapters.get(first).map(|c| c.start_ms).unwrap_or(0);
    t.chapters
        .iter()
        .enumerate()
        .take(last + 1)
        .skip(first)
        .map(|(i, c)| (c.start_ms.saturating_sub(base), trf!("Kapitel {}", "Chapter {}", i + 1)))
        .collect()
}
fn strip_tag(kind: u64, title: usize, first: usize, last: usize) -> u64 {
    (kind << 60) ^ ((title as u64) << 40) ^ ((first as u64) << 20) ^ last as u64
}

/// Abstand der Skalenstriche: kleinster Schritt, der mindestens `min_ms` lang ist.
fn ruler_step(min_ms: f64) -> f64 {
    const STEPS: [f64; 19] = [
        40.0, 100.0, 200.0, 500.0, 1_000.0, 2_000.0, 5_000.0, 10_000.0, 15_000.0, 30_000.0, 60_000.0, 120_000.0, 300_000.0,
        600_000.0, 900_000.0, 1_800_000.0, 3_600_000.0, 7_200_000.0, 14_400_000.0,
    ];
    STEPS.iter().copied().find(|s| *s >= min_ms).unwrap_or(14_400_000.0)
}

/// Zeit für die Skala: Stunden nur, wenn nötig; Zehntel nur bei feinen Schritten.
fn ruler_label(ms: f64, step: f64) -> String {
    let ms = ms.max(0.0).round() as u64;
    let (h, m, s) = (ms / 3_600_000, ms / 60_000 % 60, ms / 1000 % 60);
    let base = if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") };
    if step < 1000.0 {
        format!("{base}.{:02}", ms % 1000 / 10)
    } else {
        base
    }
}

/// Abstand der Standbilder: Bilddauer mal Zweierpotenz, damit sich die Zeiten beim Zoomen
/// wiederholen und schon fertige Bilder weiterverwendet werden.
fn tile_step(min_ms: f64, frame_ms: f64) -> f64 {
    let mut step = frame_ms.max(1.0);
    while step < min_ms {
        step *= 2.0;
    }
    step
}

/// Zoombare Zeitleiste: Werkzeugzeile, Zeitskala, Standbilder und Rollbalken.
/// `cut`: Schnittmarken des gezeigten Eintrags, `entry`: Eintrag für das Kontextmenü.
#[allow(clippy::too_many_arguments)]
fn view_strip(
    st: &mut PreviewState,
    ui: &mut egui::Ui,
    p: &super::style::Pal,
    source: Option<&Source>,
    cut: (Option<u64>, Option<u64>),
    entry: Option<usize>,
    cut_action: &mut Option<(usize, bool, u64)>,
    wave: &mut bool,
    track: Option<TrackChoice>,
) {
    let ctx = ui.ctx().clone();
    if st.strip.thumbs.is_none() {
        st.strip.thumbs = Some(super::thumbs::StripThumbs::new(ctx.clone()));
    }
    if let Some(t) = st.strip.thumbs.as_mut() {
        t.pump(&ctx);
    }
    let (lo, hi) = st.range();
    let fl = st.frame_len();
    let pos = st.current_f();
    let active = st.playing || st.scrub != 0;
    let enabled = !st.segments.is_empty() && st.total_ms > 0;
    let aspect = if st.aspect > 0.1 { st.aspect } else { 16.0 / 9.0 };
    let width = ui.available_width().max(60.0);
    let full = ((hi - lo) / f64::from(width)).max(0.01);
    // Feinste Stufe: ein Bild etwa 30 Bildpunkte breit
    let finest = (fl / 30.0).min(full);

    let s = &mut st.strip;
    if s.range != (lo, hi) || s.ms_per_px <= 0.0 {
        s.range = (lo, hi);
        s.ms_per_px = full;
        s.view_lo = lo;
    }
    s.ms_per_px = s.ms_per_px.clamp(finest, full);

    // Werkzeugzeile
    let mut zoom_to: Option<f64> = None;
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        if super::style::icon_button(ui, p, ic::MAGNIFYING_GLASS_MINUS, tr!("Verkleinern (Mausrad über der Zeitleiste)", "Zoom out (mouse wheel over the timeline)")).clicked() {
            zoom_to = Some(s.ms_per_px * 2.0);
        }
        let can = enabled && full > finest * 1.01;
        let mut z = if can { ((s.ms_per_px / full).ln() / (finest / full).ln()).clamp(0.0, 1.0) } else { 0.0 };
        ui.spacing_mut().slider_width = 150.0;
        let r = ui.add_enabled(can, egui::Slider::new(&mut z, 0.0..=1.0).show_value(false));
        if r.changed() {
            zoom_to = Some(full * (finest / full).powf(z));
        }
        if super::style::icon_button(ui, p, ic::MAGNIFYING_GLASS_PLUS, tr!("Vergrößern, bis hin zu einzelnen Bildern", "Zoom in, down to single frames")).clicked() {
            zoom_to = Some(s.ms_per_px / 2.0);
        }
        if super::style::icon_button(ui, p, ic::ARROWS_OUT_LINE_HORIZONTAL, tr!("Ganze Länge zeigen", "Show whole length")).clicked() {
            zoom_to = Some(full);
        }
        ui.add_space(6.0);
        if ui
            .selectable_label(*wave, RichText::new(trf!("{}  Tonspur", "{}  Audio", ic::WAVEFORM)).size(12.5))
            .on_hover_text(tr!(
                "Lautstärkeverlauf der Tonspur unter den Standbildern zeigen. Dafür wird der Titel einmal im Hintergrund ganz gelesen.",
                "Show the audio level below the stills. The title is read once in the background for this."
            ))
            .clicked()
        {
            *wave = !*wave;
        }
        ui.add_space(6.0);
        let span = s.ms_per_px * f64::from(width);
        ui.label(
            RichText::new(trf!("Ausschnitt {}", "Showing {}", super::views::fmt_time_precise(span.min(hi - lo) as u64)))
                .size(12.0)
                .color(p.faint),
        );
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            ui.label(
                RichText::new(tr!("Mausrad: Zoom · Umschalt+Rad oder mittlere Taste: verschieben", "Wheel: zoom · Shift+wheel or middle button: pan"))
                    .size(11.5)
                    .color(p.faint),
            );
        });
    });

    let ruler_h = 18.0f32;
    let thumb_h = 58.0f32;
    let wave_h = if *wave { WAVE_H } else { 0.0 };
    let (area, resp) = ui.allocate_exact_size(Vec2::new(width, ruler_h + thumb_h + 4.0 + wave_h), Sense::click_and_drag());
    let span_of = |mpp: f64| mpp * f64::from(area.width());

    // Zoomen um einen Ankerpunkt (Zeit bleibt an derselben Stelle)
    let zoom_at = |s: &mut Strip, mpp: f64, anchor: f64| {
        let mpp = mpp.clamp(finest, full);
        let off = (anchor - s.view_lo) / s.ms_per_px;
        s.view_lo = anchor - off * mpp;
        s.ms_per_px = mpp;
    };
    if let Some(mpp) = zoom_to {
        // Um die Abspielposition, wenn sie zu sehen ist, sonst um die Mitte
        let span = span_of(s.ms_per_px);
        let anchor = if pos >= s.view_lo && pos <= s.view_lo + span { pos } else { s.view_lo + span / 2.0 };
        zoom_at(s, mpp, anchor);
    }
    if resp.hovered() && enabled {
        let (scroll, zd, shift, command) = ui.input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.modifiers.shift, i.modifiers.command));
        let mouse_t = resp.hover_pos().map(|h| s.view_lo + f64::from(h.x - area.left()) * s.ms_per_px).unwrap_or(pos);
        if (zd - 1.0).abs() > 1e-3 {
            zoom_at(s, s.ms_per_px / f64::from(zd), mouse_t);
        } else if !command {
            if shift || scroll.x.abs() > scroll.y.abs() {
                let d = if scroll.x.abs() > 0.0 { scroll.x } else { scroll.y };
                s.view_lo -= f64::from(d) * s.ms_per_px;
            } else if scroll.y.abs() > 0.0 {
                let f = (-f64::from(scroll.y) * 0.003).exp();
                zoom_at(s, s.ms_per_px * f, mouse_t);
            }
        }
    }
    if resp.dragged_by(egui::PointerButton::Middle) {
        s.view_lo -= f64::from(resp.drag_delta().x) * s.ms_per_px;
    }
    // Ausschnitt folgt der Abspielposition, wenn sie ihn verlässt
    let span = span_of(s.ms_per_px);
    if s.drag.is_none() && s.pan_grab.is_none() && (pos - s.last_pos).abs() > 0.5 && (pos < s.view_lo || pos > s.view_lo + span) {
        s.view_lo = if active { pos - span * 0.05 } else { pos - span / 2.0 };
    }
    s.last_pos = pos;
    s.view_lo = s.view_lo.clamp(lo, (hi - span).max(lo));
    let view_lo = s.view_lo;
    let mpp = s.ms_per_px;
    let x_of = |t: f64| area.left() + ((t - view_lo) / mpp) as f32;
    let t_of = |x: f32| view_lo + f64::from(x - area.left()) * mpp;

    let painter = ui.painter_at(area);
    painter.rect_filled(area, CornerRadius::same(6), p.screen);
    let ruler = Rect::from_min_size(area.min, Vec2::new(area.width(), ruler_h));
    let thumbs_r = Rect::from_min_size(egui::pos2(area.left(), ruler.bottom() + 2.0), Vec2::new(area.width(), thumb_h));
    let wave_r = Rect::from_min_max(egui::pos2(area.left(), thumbs_r.bottom() + 2.0), egui::pos2(area.right(), area.bottom() - 2.0));

    // Tonspur: Spitzenpegel je Bildpunktspalte, gespiegelt um die Mittellinie
    if *wave {
        let wave_data = match (enabled, source, track) {
            (true, Some(src), Some(tr)) => {
                let key = (s.tag, tr.phys);
                if !s.waves.iter().any(|w| w.key == key) {
                    s.waves.push(Wave::start(key, src.clone(), st.segments.clone(), tr, ctx.clone()));
                    if s.waves.len() > 3 {
                        s.waves.remove(0);
                    }
                }
                s.waves.iter().find(|w| w.key == key).map(|w| w.data.clone())
            }
            _ => None,
        };
        let mid = wave_r.center().y;
        let half = wave_r.height() / 2.0;
        painter.line_segment([egui::pos2(area.left(), mid), egui::pos2(area.right(), mid)], egui::Stroke::new(1.0, p.track));
        let mut note = None;
        match wave_data.as_ref().and_then(|d| d.lock().ok()) {
            Some(g) => {
                let n = g.peaks.len();
                let mut x = area.left();
                while x < area.right() && n > 0 {
                    let ta = view_lo + f64::from(x - area.left()) * mpp;
                    let tb = ta + mpp;
                    if ta > g.done_ms && !g.finished {
                        break;
                    }
                    let a = ((ta / discslim::engine::preview::PEAK_MS).floor().max(0.0) as usize).min(n - 1);
                    let b = ((tb / discslim::engine::preview::PEAK_MS).ceil() as usize).clamp(a + 1, n);
                    let v = g.peaks[a..b].iter().copied().max().unwrap_or(0);
                    let h = (level(v) * half).max(0.5);
                    painter.line_segment([egui::pos2(x + 0.5, mid - h), egui::pos2(x + 0.5, mid + h)], egui::Stroke::new(1.0, p.audio));
                    x += 1.0;
                }
                if g.failed {
                    note = Some(tr!("Ton dieser Spur lässt sich nicht lesen", "This audio track cannot be read").to_string());
                } else if !g.finished {
                    let total = (st.total_ms as f64).max(1.0);
                    note = Some(trf!("Ton wird gelesen … {} %", "Reading audio … {} %", ((g.done_ms / total) * 100.0).clamp(0.0, 99.0) as u32));
                }
            }
            None if enabled => note = Some(tr!("Keine Tonspur", "No audio track").to_string()),
            None => {}
        }
        if let Some(text) = note {
            painter.text(egui::pos2(wave_r.right() - 8.0, mid), Align2::RIGHT_CENTER, text, FontId::proportional(11.5), p.muted);
        }
    }

    // Standbilder: Abstand als Zweierpotenz der Bilddauer, damit sie beim Zoomen wiederkehren
    if enabled {
        let tile_w = thumbs_r.height() * aspect;
        let step = tile_step(f64::from(tile_w) * mpp, fl);
        let exact = step < 1_500.0;
        let first = ((view_lo / step).floor() as i64).max(0);
        let last = ((view_lo + span) / step).ceil() as i64;
        let tag = s.tag;
        let mut keys = Vec::new();
        for k in first..=last {
            let t = k as f64 * step;
            if t >= hi {
                break;
            }
            let key: super::thumbs::StripKey = (tag, t.round() as u64, exact);
            let r = Rect::from_min_size(egui::pos2(x_of(t), thumbs_r.top()), Vec2::new(tile_w, thumbs_r.height()));
            match s.thumbs.as_ref().and_then(|th| th.get(&key)) {
                Some(tex) => {
                    painter.image(tex.id(), r, Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                }
                None => {
                    painter.rect_filled(r.shrink(1.0), CornerRadius::same(2), p.track);
                    keys.push(key);
                }
            }
        }
        // Zuerst die Bilder nahe der Abspielposition (bzw. der Mitte)
        let focus = if pos >= view_lo && pos <= view_lo + span { pos } else { view_lo + span / 2.0 };
        keys.sort_by(|a, b| {
            let da = (a.1 as f64 - focus).abs();
            let db = (b.1 as f64 - focus).abs();
            da.partial_cmp(&db).unwrap_or(std::cmp::Ordering::Equal)
        });
        if let (Some(th), Some(src)) = (s.thumbs.as_mut(), source) {
            th.request(tag, src, &st.segments, keys);
        }
    }

    // Zeitskala mit beschrifteten Hauptstrichen und kurzen Zwischenstrichen
    painter.rect_filled(ruler, CornerRadius::ZERO, p.card);
    let step = ruler_step(f64::from(84.0f32) * mpp);
    let minor = step / 5.0;
    let first = (view_lo / minor).floor() as i64;
    let last = ((view_lo + span) / minor).ceil() as i64;
    for k in first..=last {
        let t = k as f64 * minor;
        let x = x_of(t);
        if k % 5 == 0 {
            painter.line_segment([egui::pos2(x, ruler.top()), egui::pos2(x, ruler.bottom())], egui::Stroke::new(1.0, p.muted));
            painter.text(egui::pos2(x + 3.0, ruler.center().y), Align2::LEFT_CENTER, ruler_label(t, step), FontId::proportional(11.0), p.muted);
        } else {
            painter.line_segment([egui::pos2(x, ruler.bottom() - 4.0), egui::pos2(x, ruler.bottom())], egui::Stroke::new(1.0, p.faint));
        }
    }

    // Weggeschnittenes abdunkeln, Schnitt- und Kapitelmarken
    if st.window.is_none() && (cut.0.is_some() || cut.1.is_some()) {
        let a = cut.0.map(|v| v as f64).unwrap_or(lo);
        let b = cut.1.map(|v| (v as f64).min(hi)).unwrap_or(hi);
        let shade = Color32::from_black_alpha(150);
        if a > view_lo {
            painter.rect_filled(Rect::from_min_max(egui::pos2(area.left(), thumbs_r.top()), egui::pos2(x_of(a), area.bottom() - 2.0)), CornerRadius::ZERO, shade);
        }
        if b < view_lo + span {
            painter.rect_filled(Rect::from_min_max(egui::pos2(x_of(b), thumbs_r.top()), egui::pos2(area.right(), area.bottom() - 2.0)), CornerRadius::ZERO, shade);
        }
    }
    for (ms, _) in &st.chapters {
        let t = *ms as f64;
        if t > view_lo && t < view_lo + span {
            let x = x_of(t);
            painter.line_segment([egui::pos2(x, ruler.top()), egui::pos2(x, area.bottom())], egui::Stroke::new(1.5, p.subs));
        }
    }
    for (t, open) in [(cut.0, true), (cut.1, false)] {
        let Some(t) = t.map(|v| v as f64) else { continue };
        if t >= view_lo && t <= view_lo + span {
            let x = x_of(t);
            painter.line_segment([egui::pos2(x, ruler.top()), egui::pos2(x, area.bottom())], egui::Stroke::new(2.0, p.good));
            // Klammer zeigt zur behaltenen Seite
            let dir = if open { 6.0 } else { -6.0 };
            for y in [thumbs_r.top() + 1.0, thumbs_r.bottom() - 1.0] {
                painter.line_segment([egui::pos2(x, y), egui::pos2(x + dir, y)], egui::Stroke::new(2.0, p.good));
            }
        }
    }

    // Abspielposition
    let shown = s.drag.unwrap_or(pos);
    if enabled && shown >= view_lo && shown <= view_lo + span {
        let x = x_of(shown);
        painter.line_segment([egui::pos2(x, ruler.top()), egui::pos2(x, area.bottom())], egui::Stroke::new(1.5, Color32::WHITE));
        painter.add(egui::Shape::convex_polygon(
            vec![egui::pos2(x - 6.0, ruler.top()), egui::pos2(x + 6.0, ruler.top()), egui::pos2(x, ruler.top() + 8.0)],
            Color32::WHITE,
            egui::Stroke::NONE,
        ));
    }
    let mut hover_t = None;
    if let Some(h) = resp.hover_pos().filter(|_| enabled) {
        let t = t_of(h.x).clamp(lo, hi);
        hover_t = Some(t);
        painter.line_segment([egui::pos2(h.x, ruler.top()), egui::pos2(h.x, area.bottom())], egui::Stroke::new(1.0, p.faint));
    }
    if !enabled {
        painter.text(area.center(), Align2::CENTER_CENTER, tr!("Keine Vorschau für diese Auswahl", "No preview for this selection"), FontId::proportional(12.0), p.faint);
    }

    // Klicken und Ziehen: Abspielposition setzen (beim Loslassen bildgenau)
    let mut live_seek = None;
    let mut final_seek = None;
    if enabled && resp.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_down()) {
        if let Some(pp) = resp.interact_pointer_pos() {
            let t = t_of(pp.x).clamp(lo, (hi - fl).max(lo));
            s.drag = Some(t);
            live_seek = Some(t);
            ctx.request_repaint_after(Duration::from_millis(16));
        }
    }
    if resp.drag_stopped() || resp.clicked() {
        s.drag_live = None;
        final_seek = s.drag.take();
    }
    if resp.secondary_clicked() {
        s.ctx_ms = resp.interact_pointer_pos().map(|pp| t_of(pp.x).clamp(lo, hi));
    }
    if let (Some(e), Some(at)) = (entry, s.ctx_ms) {
        resp.context_menu(|ui| {
            ui.label(RichText::new(trf!("Eintrag {} · {}", "Entry {} · {}", e + 1, super::views::fmt_time_precise(at as u64))).size(12.5).color(p.muted));
            if ui.button(trf!("{}  Schnittanfang hier setzen", "{}  Set cut start here", ic::BRACKETS_SQUARE)).clicked() {
                *cut_action = Some((e, true, at as u64));
                ui.close();
            }
            if ui.button(trf!("{}  Schnittende hier setzen", "{}  Set cut end here", ic::BRACKETS_SQUARE)).clicked() {
                *cut_action = Some((e, false, at as u64));
                ui.close();
            }
        });
    }
    if let Some(t) = hover_t.filter(|_| s.drag.is_none()) {
        let name = st_chapter_name(&st.chapters, t);
        let text = match name {
            Some(n) => format!("{n} · {}", super::views::fmt_time_precise(t as u64)),
            None => super::views::fmt_time_precise(t as u64),
        };
        resp.clone().on_hover_text_at_pointer(text);
    }

    // Rollbalken: zeigt und verschiebt den sichtbaren Ausschnitt
    ui.add_space(3.0);
    let (bar, bresp) = ui.allocate_exact_size(Vec2::new(width, 10.0), Sense::click_and_drag());
    let len = (hi - lo).max(1.0);
    let bx = |t: f64| bar.left() + ((t - lo) / len) as f32 * bar.width();
    let s = &mut st.strip;
    let mut handle = Rect::from_min_max(egui::pos2(bx(s.view_lo), bar.top()), egui::pos2(bx(s.view_lo + span), bar.bottom()));
    if handle.width() < 16.0 {
        handle = Rect::from_center_size(handle.center(), Vec2::new(16.0, bar.height()));
    }
    ui.painter().rect_filled(bar, CornerRadius::same(5), p.track);
    if span < len - 1.0 {
        let c = if bresp.hovered() || s.pan_grab.is_some() { p.muted } else { p.faint };
        ui.painter().rect_filled(handle, CornerRadius::same(5), c);
    }
    if bresp.is_pointer_button_down_on() {
        if let Some(pp) = bresp.interact_pointer_pos() {
            let t = lo + f64::from((pp.x - bar.left()) / bar.width().max(1.0)) * len;
            let grab = *s.pan_grab.get_or_insert_with(|| if handle.contains(pp) { t - s.view_lo } else { span / 2.0 });
            s.view_lo = (t - grab).clamp(lo, (hi - span).max(lo));
        }
    } else {
        s.pan_grab = None;
    }

    // Sprünge erst jetzt, da sie die ganze Vorschau brauchen
    if let Some(t) = live_seek {
        let ready = !st.want_frame || st.strip.drag_live.is_none_or(|(_, at)| at.elapsed().as_millis() > 300);
        let moved = st.strip.drag_live.is_none_or(|(last, _)| (last - t).abs() > 20.0);
        if ready && moved {
            // Fein gezoomt gleich bildgenau, sonst schnell am Schlüsselbild
            let exact = st.step_exact && mpp * 30.0 < 2_000.0;
            st.seek_time(t, exact);
            st.strip.drag_live = Some((t, Instant::now()));
        }
    }
    if let Some(t) = final_seek {
        let exact = st.step_exact;
        st.seek_time(t, exact);
    }
}

fn st_chapter_name(chapters: &[(u64, String)], t: f64) -> Option<String> {
    if chapters.len() < 2 {
        return None;
    }
    chapters.iter().rev().find(|(ms, _)| (*ms as f64) <= t).map(|c| c.1.clone())
}

impl App {
    /// Aktuell gewählte Kompression, passend zur gewählten Liste.
    fn preview_opts(&self) -> Option<VideoOpts> {
        if !self.preview.compressed {
            return None;
        }
        let disc = self.disc.as_ref()?;
        let est = self.plan.estimate(disc);
        let s = match self.preview_sel() {
            Selection::Item(id) => self.plan.settings(id),
            Selection::Entry(i) => self.plan.reauthor.get(i)?.settings.clone(),
            Selection::Source(i) => self.plan.settings(ItemId::Title(i)),
            Selection::None => return None,
        };
        Some(CellDecision::from_settings(&s).video_opts(est.auto_ratio))
    }

    /// Was die Vorschau zeigt: die Auswahl oder, bei "Alle Schnitte", den laufenden Abschnitt.
    pub(super) fn preview_sel(&self) -> Selection {
        if self.preview.program && self.plan.mode == discslim::engine::plan::Mode::Reauthor {
            if let Some(c) = self.preview.prog.get(self.preview.prog_cur) {
                return Selection::Entry(c.0);
            }
        }
        self.sel
    }

    /// Spielt den Übergang von Eintrag `i` zum nächsten: das Ende von `i` und den Anfang
    /// von `i + 1`, so wie sie ausgegeben werden, und hält danach an.
    pub(super) fn listen_junction(&mut self, i: usize) {
        self.update_program();
        let (Some(a), Some(b)) = (self.preview.prog.get(i).copied(), self.preview.prog.get(i + 1).copied()) else { return };
        let st = &mut self.preview;
        st.program = true;
        st.prog_cur = i;
        let from = (a.2 - LISTEN_MS).max(a.1);
        st.time_ms = Some(from as u64);
        st.prog_seek = Some(from);
        st.prog_play = Some(true);
        let until = (b.1 + LISTEN_MS).min(b.2);
        st.listen_until = Some((i + 1, until, Instant::now() + Duration::from_secs_f64(LISTEN_MS * 2.0 / 1000.0 + 8.0)));
        self.sel = Selection::Entry(i);
    }

    /// Abschnitte für "Alle Schnitte" aus der Liste der neuen DVD.
    pub(super) fn update_program(&mut self) {
        let Some(disc) = self.disc.clone() else { return };
        self.preview.prog = self
            .plan
            .reauthor
            .iter()
            .enumerate()
            .map(|(i, e)| {
                let total = discslim::engine::plan::entry_duration_ms(&disc, e);
                let (s, end) = e.cut.effective(total);
                (i, s as f64, end.min(total) as f64, e.cut.exact)
            })
            .collect();
        if self.preview.prog_cur >= self.preview.prog.len() {
            self.preview.prog_cur = 0;
        }
        if self.preview.prog.is_empty() || self.plan.mode != discslim::engine::plan::Mode::Reauthor {
            self.preview.program = false;
        }
    }

    /// Tonspur für die Vorschau: die erste ausgewählte Spur des Titels.
    fn preview_track(&self, disc: &Disc) -> Option<TrackChoice> {
        let (title, settings) = match self.preview_sel() {
            Selection::Item(ItemId::Title(i)) | Selection::Source(i) => (i, self.plan.settings(ItemId::Title(i))),
            Selection::Entry(i) => {
                let e = self.plan.reauthor.get(i)?;
                (e.title, e.settings.clone())
            }
            _ => return None,
        };
        let t = disc.titles.get(title)?;
        let pick = t.audio.iter().find(|a| settings.audio[a.phys as usize] && Codec::of(&a.attr).is_some());
        let a = pick.or_else(|| t.audio.iter().find(|a| Codec::of(&a.attr).is_some()))?;
        Some(TrackChoice { phys: a.phys, codec: Codec::of(&a.attr)?, channels: a.attr.channels })
    }

    /// Lädt die Vorschau neu, wenn sich die Auswahl geändert hat.
    /// Eigene Kapitelmarken eines Eintrags für die Zeitleiste.
    fn custom_chapters(&self, sel: Selection) -> Option<Vec<(u64, String)>> {
        let Selection::Entry(i) = sel else { return None };
        let marks = self.plan.reauthor.get(i)?.chapter_marks.as_ref()?;
        let mut out = vec![(0, tr!("Kapitel 1", "Chapter 1").to_string())];
        out.extend(marks.iter().enumerate().map(|(k, m)| (*m, trf!("Kapitel {}", "Chapter {}", k + 2))));
        Some(out)
    }

    pub(super) fn sync_preview(&mut self) {
        self.update_program();
        self.preview.chapter_cuts = self.plan.chapters_at_cuts && self.plan.reauthor.len() > 1;
        // Bei "Alle Schnitte" ist immer der laufende Eintrag gewählt
        if self.preview.program && self.plan.mode == discslim::engine::plan::Mode::Reauthor {
            if let Some(c) = self.preview.prog.get(self.preview.prog_cur) {
                self.sel = Selection::Entry(c.0);
            }
        }
        let want = self.preview_sel();
        if !self.preview.dirty && self.preview.loaded == Some(want) {
            // Kapitelmarken können sich ändern, ohne dass die Vorschau neu lädt.
            if let Selection::Entry(i) = want {
                let chapters = match self.custom_chapters(want) {
                    Some(c) => c,
                    None => match (self.disc.as_ref(), self.plan.reauthor.get(i)) {
                        (Some(d), Some(e)) => chapter_list(d, e.title, e.first_chapter, e.last_chapter),
                        _ => return,
                    },
                };
                if chapters != self.preview.chapters {
                    self.preview.chapters = chapters;
                }
            }
            return;
        }
        self.preview.dirty = false;
        let switched = self.preview.loaded != Some(want);
        self.preview.stop();
        self.preview.loaded = Some(want);
        // Wechsel zu einem geschnittenen Eintrag: auch in „Ganzer Titel“ am Schnittanfang
        // beginnen, nicht am Titelanfang. (Bei „Alle Schnitte“ springt die Wiedergabe selbst.)
        if switched && !self.preview.program {
            if let Selection::Entry(i) = want {
                if let Some(e) = self.plan.reauthor.get(i) {
                    let start = e.cut.start();
                    if start > discslim::engine::plan::CUT_EDGE_MS && self.preview.prog_seek.is_none() {
                        self.preview.prog_seek = Some(start as f64);
                    }
                }
            }
        }
        let Some(disc) = self.disc.clone() else { return };
        let (segments, ms, chapters) = match want {
            Selection::Item(ItemId::Title(i)) | Selection::Source(i) => {
                let Some(t) = disc.titles.get(i) else { return };
                let last = t.chapters.len().saturating_sub(1);
                let seg = title_segments(&disc, i, 0, last);
                (seg, t.ms, chapter_list(&disc, i, 0, last))
            }
            Selection::Item(ItemId::Menu(v)) => {
                let ms = disc.menus.iter().find(|m| m.vts == v).map(|m| m.ms).unwrap_or(0);
                (menu_segments(&disc, v), ms, Vec::new())
            }
            Selection::Entry(i) => {
                let Some(e) = self.plan.reauthor.get(i) else { return };
                let seg = title_segments(&disc, e.title, e.first_chapter, e.last_chapter);
                let ms = seg.iter().map(|s| s.ms).sum();
                let chapters = self
                    .custom_chapters(want)
                    .unwrap_or_else(|| chapter_list(&disc, e.title, e.first_chapter, e.last_chapter));
                (seg, ms, chapters)
            }
            _ => (Vec::new(), 0, Vec::new()),
        };
        if segments.is_empty() {
            return;
        }
        let opts = self.preview_opts();
        self.preview.last_opts = opts;
        self.preview.total_ms = ms;
        self.preview.chapters = chapters;
        let audio = self.preview.audio.clone().and_then(|bus| self.preview_track(&disc).map(|t| (bus, t)));
        let seek_id = self.preview.seek_id;
        // Standbilder der Zeitleiste gehören zu Titel und Kapitelbereich (gleich für Titel und Eintrag)
        self.preview.strip.tag = match want {
            Selection::Item(ItemId::Title(i)) | Selection::Source(i) => {
                let last = disc.titles.get(i).map(|t| t.chapters.len().saturating_sub(1)).unwrap_or(0);
                strip_tag(0, i, 0, last)
            }
            Selection::Item(ItemId::Menu(v)) => strip_tag(1, usize::from(v), 0, 0),
            Selection::Entry(i) => self.plan.reauthor.get(i).map(|e| strip_tag(0, e.title, e.first_chapter, e.last_chapter)).unwrap_or(0),
            _ => 0,
        };
        self.preview.handle = Some(preview::spawn(disc.source.clone(), segments.clone(), 0.0, opts, seek_id, audio));
        self.preview.segments = segments;
        self.preview.want_frame = true;
    }

    /// Einmal je Frame: nächstes Bild holen, wenn es Zeit dafür ist.
    pub(super) fn pump_preview(&mut self, ctx: &egui::Context) {
        // Auswahl geändert (z. B. Klick in der Liste): gleich noch einen Durchlauf, damit
        // die Vorschau lädt, ohne auf die nächste Mausbewegung zu warten
        if self.disc.is_some() && (self.preview.dirty || self.preview.loaded != Some(self.preview_sel())) {
            ctx.request_repaint();
        }
        let compressed_changed = {
            let opts = self.preview_opts();
            if opts != self.preview.last_opts {
                self.preview.last_opts = opts;
                true
            } else {
                false
            }
        };
        let st = &mut self.preview;
        // Übergang anhören: kurz nach der Schnittstelle anhalten
        if let Some((k, t, deadline)) = st.listen_until {
            if Instant::now() > deadline || !st.program {
                st.listen_until = None;
            } else if st.playing && (st.prog_cur > k || (st.prog_cur == k && st.has_frame && st.current_f() >= t)) {
                st.set_playing(false);
                st.listen_until = None;
            }
        }
        let Some(handle) = st.handle.as_ref() else { return };
        if compressed_changed {
            let _ = handle.cmd.send(preview::PreviewCmd::Compression(st.last_opts));
            if !st.playing {
                let at = st.current_f();
                let exact = st.step_exact;
                st.seek_time(at, exact);
            }
        }
        let now = Instant::now();
        // Mit Ton: Bilder genau dann zeigen, wenn die Tonuhr ihre Zeit erreicht.
        let clock = if st.playing { st.audio.as_ref().and_then(|a| a.clock_ms()) } else { None };
        if let Some(c) = clock {
            for _ in 0..8 {
                if st.pending.is_none() {
                    let Some(h) = st.handle.as_ref() else { break };
                    loop {
                        match h.frames.try_recv() {
                            Ok(f) if f.seek_id == st.seek_id => {
                                st.pending = Some(f);
                                break;
                            }
                            Ok(_) => continue,
                            Err(_) => break,
                        }
                    }
                }
                match st.pending.as_ref() {
                    // Ende der Abschnitte: nach dem letzten Bild weiterschalten oder anhalten
                    Some(f) if f.eos => {
                        if c >= st.current_f() + st.frame_len() {
                            st.pending = None;
                            if !st.advance_program() {
                                st.set_playing(false);
                            }
                        }
                        break;
                    }
                    Some(f) if f.time_ms <= c + 8.0 => {
                        let f = st.pending.take().unwrap();
                        let (show, before) = st.in_window(&f);
                        if show {
                            st.upload(ctx, f);
                        } else if !before {
                            if !st.advance_program() {
                                st.set_playing(false);
                            }
                            break;
                        }
                    }
                    _ => break,
                }
            }
            ctx.request_repaint_after(Duration::from_millis(5));
            return;
        }
        let due = st.want_frame || ((st.playing || st.scrub != 0) && st.next_at.map(|t| now >= t).unwrap_or(true));
        if due {
            let mut got = None;
            let frames: Vec<PreviewFrame> = {
                let Some(h) = st.handle.as_ref() else { return };
                let mut v: Vec<PreviewFrame> = st.pending.take().into_iter().collect();
                while v.is_empty() {
                    match h.frames.try_recv() {
                        Ok(f) if f.seek_id == st.seek_id => v.push(f),
                        Ok(_) => continue,
                        Err(_) => break,
                    }
                }
                v
            };
            for f in frames {
                got = Some(f);
            }
            if got.as_ref().is_some_and(|f| f.eos) {
                got = None;
                if !st.advance_program() {
                    st.set_playing(false);
                    st.scrub = 0;
                }
                st.want_frame = false;
            }
            if let Some(f) = got {
                let (show, before) = st.in_window(&f);
                if show {
                    let dur = f.duration;
                    st.upload(ctx, f);
                    st.want_frame = false;
                    st.next_at = Some(now + Duration::from_secs_f64(dur));
                } else if !before {
                    // Ende des Schnitts erreicht: nächster Abschnitt oder anhalten.
                    if !st.advance_program() {
                        st.set_playing(false);
                        st.scrub = 0;
                    }
                    st.want_frame = false;
                } else if st.scrub < 0 {
                    // Rückwärts am Abschnittsanfang angekommen: im vorigen weiter oder aufhören
                    if !st.previous_clip(true) {
                        st.scrub = 0;
                        let at = st.current_f();
                        st.seek_time(at, true);
                    }
                }
            }
        }
        if st.playing || st.scrub != 0 {
            ctx.request_repaint_after(Duration::from_millis(5));
        } else if st.want_frame {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
    }

    pub(super) fn view_preview(&mut self, ui: &mut egui::Ui) {
        let p = self.pal;
        let sel = self.preview_sel();
        // Schnittmarken des gewählten Eintrags auf die Zeitleiste übertragen
        self.preview.cut_marks = match sel {
            Selection::Entry(i) if self.preview.total_ms > 0 => self
                .plan
                .reauthor
                .get(i)
                .map(|e| {
                    let f = |ms: u64| ms as f32 / self.preview.total_ms as f32;
                    (e.cut.start_ms.map(f), e.cut.end_ms.map(f))
                })
                .unwrap_or((None, None)),
            _ => (None, None),
        };
        // Schritte folgen der Schnittart: bildgenau oder von Schlüsselbild zu Schlüsselbild
        let (exact, cut_len) = match sel {
            Selection::Entry(i) => match (self.plan.reauthor.get(i), self.disc.as_ref()) {
                (Some(e), Some(d)) if e.cut.is_trim(discslim::engine::plan::entry_duration_ms(d, e)) => {
                    let total = discslim::engine::plan::entry_duration_ms(d, e);
                    (e.cut.exact, Some(e.cut.end().min(total).saturating_sub(e.cut.start())))
                }
                (Some(e), _) => (e.cut.exact, None),
                _ => (true, None),
            },
            _ => (true, None),
        };
        self.preview.step_exact = exact;
        self.preview.cut_len = cut_len;
        let window = if self.preview.program {
            self.preview.prog.get(self.preview.prog_cur).map(|c| (c.1, c.2.min(self.preview.total_ms as f64), c.3))
        } else {
            match (sel, cut_len, self.preview.cut_view) {
                (Selection::Entry(i), Some(_), true) => self.plan.reauthor.get(i).map(|e| {
                    let end = (e.cut.end() as f64).min(self.preview.total_ms as f64);
                    (e.cut.start() as f64, end, e.cut.exact)
                }),
                _ => None,
            }
        };
        // Sprünge und Start erst, wenn die richtige Vorschau geladen ist; sonst gingen sie
        // an die alte und beim Neuladen verloren.
        let ready = !self.preview.dirty && self.preview.loaded == Some(self.preview_sel());
        if ready && self.preview.handle.is_some() && self.preview.total_ms > 0 {
            self.preview.set_window(window);
            // Nach dem Wechsel zum nächsten Abschnitt dort weitermachen
            if let Some(t) = self.preview.prog_seek.take() {
                let exact = window.map(|w| w.2).unwrap_or(true);
                self.preview.seek_time(t, exact);
            }
            if let Some(play) = self.preview.prog_play.take() {
                if play {
                    self.preview.set_playing(true);
                }
            }
            // Rückwärtsfahrt im vorigen Abschnitt fortsetzen
            if let Some(t) = self.preview.prog_reverse.take() {
                if self.preview.hold.is_some() {
                    self.preview.start_reverse(t);
                } else {
                    let exact = window.map(|w| w.2).unwrap_or(true);
                    self.preview.seek_time(t - self.preview.frame_len(), exact);
                }
            }
        }
        let mut cut_action: Option<(usize, bool, u64)> = None;
        let can_all = self.plan.mode == discslim::engine::plan::Mode::Reauthor && !self.plan.reauthor.is_empty();
        let sel_entry = if let Selection::Entry(i) = self.sel { Some(i) } else { None };
        let avail = ui.available_size();
        // Entweder die schmale Leiste oder die zoombare Zeitleiste (an deren Stelle)
        let strip_open = self.prefs.timeline_open;
        let controls_h = 124.0 + if strip_open { STRIP_H - 18.0 + if self.prefs.timeline_wave { WAVE_H } else { 0.0 } } else { 0.0 };
        let strip_track = self.disc.as_ref().and_then(|d| self.preview_track(d));
        // Schnittmarken (Millisekunden) und Eintrag für die zoombare Zeitleiste
        let strip_cut = match sel {
            Selection::Entry(i) => self.plan.reauthor.get(i).map(|e| (e.cut.start_ms, e.cut.end_ms)).unwrap_or((None, None)),
            _ => (None, None),
        };
        let strip_entry = if let Selection::Entry(i) = sel { Some(i) } else { None };
        let strip_source = self.disc.as_ref().map(|d| &d.source);
        let screen_h = (avail.y - controls_h).max(120.0);
        let (screen, _) = ui.allocate_exact_size(Vec2::new(avail.x, screen_h), Sense::hover());
        ui.painter().rect_filled(screen, CornerRadius::same(10), p.screen);

        // Blende des gezeigten Eintrags als Überlagerung, so wie sie ausgegeben wird
        let fade_spec = match (self.preview_sel(), self.disc.as_ref()) {
            (Selection::Entry(i), Some(disc)) => self.plan.reauthor.get(i).filter(|e| e.active_fade().is_active()).map(|e| {
                let total = discslim::engine::plan::entry_duration_ms(disc, e);
                let (a, b) = e.cut.effective(total);
                (e.fade, a as f64, b.min(total) as f64)
            }),
            _ => None,
        };
        if let Some(bus) = self.preview.audio.as_ref() {
            bus.set_fade(fade_spec.filter(|f| f.0.audio));
        }
        let fade_overlay = match (fade_spec, self.preview.current_ms()) {
            (Some((f, a, b)), Some(now)) => f.at(now as f64, a, b),
            _ => None,
        };

        let st = &mut self.preview;
        if let (Some(tex), true) = (st.texture.as_ref(), st.has_frame) {
            let aspect = if st.aspect > 0.1 { st.aspect } else { 4.0 / 3.0 };
            let mut size = Vec2::new(screen.width(), screen.width() / aspect);
            if size.y > screen.height() {
                size = Vec2::new(screen.height() * aspect, screen.height());
            }
            let rect = Rect::from_center_size(screen.center(), size);
            ui.painter().image(
                tex.id(),
                rect,
                Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                Color32::WHITE,
            );
            if let Some((a, kind)) = fade_overlay {
                let cover = ((1.0 - a).clamp(0.0, 1.0) * 255.0) as u8;
                let c = match kind {
                    discslim::engine::plan::FadeKind::White => Color32::from_rgba_unmultiplied(235, 235, 235, cover),
                    _ => Color32::from_rgba_unmultiplied(16, 16, 16, cover),
                };
                if cover > 0 {
                    ui.painter().rect_filled(rect, CornerRadius::ZERO, c);
                }
            }
        } else if st.segments.is_empty() {
            // Während ein Bild lädt, bleibt die Fläche einfach schwarz (kein Hinweis, der kurz aufblitzt).
            let text = tr!("Für diese Auswahl gibt es keine Vorschau", "No preview is available for this selection");
            ui.painter().text(
                screen.center() - Vec2::new(0.0, 14.0),
                Align2::CENTER_CENTER,
                ic::MONITOR_PLAY,
                FontId::proportional(40.0),
                p.faint,
            );
            ui.painter().text(
                screen.center() + Vec2::new(0.0, 22.0),
                Align2::CENTER_CENTER,
                text,
                FontId::proportional(13.0),
                p.faint,
            );
        }

        let prog_mode = st.program && !st.prog.is_empty();
        let (lo, hi) = st.range();
        // Bei "Alle Schnitte" gilt eine gemeinsame Zeitleiste über alle Abschnitte.
        let (g_pos, g_total) = if prog_mode {
            let (_, total) = st.prog_offsets();
            (st.prog_position(), total)
        } else {
            (st.current_f() - lo, hi - lo)
        };
        ui.add_space(8.0);
        if strip_open {
            // Zoombare Zeitleiste mit Standbildern an Stelle der schmalen Leiste
            view_strip(st, ui, &p, strip_source, strip_cut, strip_entry, &mut cut_action, &mut self.prefs.timeline_wave, strip_track);
        } else {
            let width = ui.available_width();
            let (bar, resp) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::click_and_drag());
            let track = Rect::from_center_size(bar.center(), Vec2::new(bar.width(), 6.0));
            ui.painter().rect_filled(track, CornerRadius::same(3), p.track);
            let span = g_total.max(1.0) as f32;
            let shown = st.drag.unwrap_or((g_pos as f32 / span).clamp(0.0, 1.0));
            // Weggeschnittene Bereiche abdunkeln, Marken zeigen
            let (m_in, m_out) = if st.window.is_some() || prog_mode { (None, None) } else { st.cut_marks };
            if m_in.is_some() || m_out.is_some() {
                let a = m_in.unwrap_or(0.0).clamp(0.0, 1.0);
                let b = m_out.unwrap_or(1.0).clamp(0.0, 1.0);
                let x = |f: f32| track.left() + track.width() * f;
                let keep = Rect::from_min_max(egui::pos2(x(a), track.top() - 3.0), egui::pos2(x(b), track.bottom() + 3.0));
                ui.painter().rect_stroke(keep, CornerRadius::same(4), egui::Stroke::new(1.5, p.good), egui::StrokeKind::Outside);
            }
            let fill = Rect::from_min_size(track.min, Vec2::new(track.width() * shown, track.height()));
            ui.painter().rect_filled(fill, CornerRadius::same(3), p.accent);
            if prog_mode {
                // Grenzen zwischen den Abschnitten; mit "An Schnitten Kapitelmarken" als
                // Kapitelmarke (Farbe der Kapitel, etwas höher)
                let (offs, _) = st.prog_offsets();
                let (color, reach) = if st.chapter_cuts { (p.subs, 7.0) } else { (p.good, 5.0) };
                for o in offs.iter().skip(1) {
                    let x = track.left() + track.width() * (*o as f32 / span).min(1.0);
                    ui.painter().line_segment(
                        [egui::pos2(x, track.top() - reach), egui::pos2(x, track.bottom() + reach)],
                        egui::Stroke::new(2.5, color),
                    );
                }
            } else {
                // Kapitelmarken
                for (ms, _) in st.chapters.iter().filter(|c| (c.0 as f64) > lo + 1.0 && (c.0 as f64) < hi) {
                    let x = track.left() + track.width() * ((*ms as f64 - lo) as f32 / span).min(1.0);
                    ui.painter().line_segment(
                        [egui::pos2(x, track.top() - 4.0), egui::pos2(x, track.bottom() + 4.0)],
                        egui::Stroke::new(2.0, p.subs),
                    );
                }
            }
            ui.painter().circle_filled(egui::pos2(track.left() + track.width() * shown, track.center().y), 7.0, p.accent);
            if let Some(hover) = resp.hover_pos() {
                let f = ((hover.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
                if prog_mode {
                    let (offs, _) = st.prog_offsets();
                    let g = f64::from(f * span);
                    let k = offs.iter().rposition(|o| *o <= g).unwrap_or(0);
                    if st.chapter_cuts {
                        resp.clone().on_hover_text(trf!("Eintrag {} · {} · beginnt ein Kapitel", "Entry {} · {} · starts a chapter", k + 1, fmt_time(g as u64)));
                    } else {
                        resp.clone().on_hover_text(trf!("Eintrag {} · {}", "Entry {} · {}", k + 1, fmt_time(g as u64)));
                    }
                } else {
                    // Kapitel unter dem Mauszeiger nennen
                    let at = (lo + f64::from(f * span)) as u64;
                    if let Some((_, name)) = st.chapters.iter().rev().find(|(ms, _)| *ms <= at) {
                        resp.clone().on_hover_text(format!("{name} · {}", fmt_time(at)));
                    }
                }
            }
            if resp.is_pointer_button_down_on() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    let f = ((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0);
                    st.drag = Some(f);
                    // Beim Ziehen laufend mitspringen (am Schlüsselbild, das geht schnell).
                    // Ein neuer Sprung erst, wenn das vorige Bild da ist oder es zu lange dauert.
                    let (k, t) = if prog_mode {
                        let (offs, _) = st.prog_offsets();
                        let g = f64::from(f) * g_total;
                        let k = offs.iter().rposition(|o| *o <= g).unwrap_or(0);
                        (k, st.prog.get(k).map(|c| c.1 + (g - offs[k])).unwrap_or(0.0))
                    } else {
                        (st.prog_cur, lo + f64::from(f) * (hi - lo))
                    };
                    let ready = !st.want_frame || st.drag_live.is_none_or(|(_, at)| at.elapsed().as_millis() > 300);
                    let moved = st.drag_live.is_none_or(|(last, _)| (last - t).abs() > 40.0);
                    // Bei "Alle Schnitte" nur innerhalb des laufenden Eintrags (ein Wechsel lädt neu)
                    if ready && moved && (!prog_mode || k == st.prog_cur) {
                        st.seek_time(t, false);
                        st.drag_live = Some((t, Instant::now()));
                    }
                    ui.ctx().request_repaint_after(Duration::from_millis(16));
                }
            }
            // Rechtsklick: an dieser Stelle Schnittanfang oder -ende des passenden Eintrags setzen
            if resp.secondary_clicked() {
                if let Some(pos) = resp.interact_pointer_pos() {
                    st.ctx_frac = Some(((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0));
                }
            }
            let ctx_target: Option<(usize, u64)> = st.ctx_frac.and_then(|f| {
                if prog_mode {
                    let (offs, _) = st.prog_offsets();
                    let g = f64::from(f) * g_total;
                    let k = offs.iter().rposition(|o| *o <= g).unwrap_or(0);
                    st.prog.get(k).map(|c| (c.0, (c.1 + (g - offs[k])).min(c.2) as u64))
                } else {
                    sel_entry.map(|i| (i, (lo + f64::from(f) * (hi - lo)) as u64))
                }
            });
            if let Some((entry, at)) = ctx_target {
                resp.context_menu(|ui| {
                    ui.label(
                        RichText::new(trf!("Eintrag {} · {}", "Entry {} · {}", entry + 1, super::views::fmt_time_precise(at)))
                            .size(12.5)
                            .color(p.muted),
                    );
                    if ui.button(trf!("{}  Schnittanfang hier setzen", "{}  Set cut start here", ic::BRACKETS_SQUARE)).clicked() {
                        cut_action = Some((entry, true, at));
                        ui.close();
                    }
                    if ui.button(trf!("{}  Schnittende hier setzen", "{}  Set cut end here", ic::BRACKETS_SQUARE)).clicked() {
                        cut_action = Some((entry, false, at));
                        ui.close();
                    }
                });
            }
            if resp.drag_stopped() || resp.clicked() {
                st.drag_live = None;
                if let Some(f) = st.drag.take() {
                    if prog_mode {
                        st.prog_seek_global(f64::from(f) * g_total);
                    } else {
                        let exact = st.step_exact;
                        st.seek_time(lo + f64::from(f) * (hi - lo), exact);
                    }
                }
            }
        }

        let jump_secs = &mut self.prefs.jump_secs;
        let timeline_open = &mut self.prefs.timeline_open;        // Erste Zeile: Navigation, Sprünge, Zeit, Kapitel
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            let enabled = !st.segments.is_empty();
            let exact = st.step_exact;
            let cur = st.time_ms.unwrap_or(0);
            let prev_tip = if prog_mode { tr!("Zum vorigen Eintrag", "Previous entry") } else { tr!("Zum vorigen Kapitel", "Previous chapter") };
            if super::style::icon_button(ui, &p, ic::SKIP_BACK, prev_tip).clicked() && enabled {
                if prog_mode {
                    let k = st.prog_cur;
                    let start = st.prog[k].1;
                    if cur as f64 > start + 500.0 || k == 0 {
                        st.prog_goto(k, start);
                    } else {
                        let s = st.prog[k - 1].1;
                        st.prog_goto(k - 1, s);
                    }
                } else {
                    let target = st.chapters.iter().rev().map(|c| c.0 as f64).find(|ms| *ms >= lo && *ms + 500.0 < cur as f64).unwrap_or(lo);
                    st.seek_time(target, exact);
                }
            }
            let (back_tip, fwd_tip) = if exact {
                (tr!("Ein Bild zurück (gedrückt halten: fortlaufend)", "One frame back (hold: continuous)"), tr!("Ein Bild weiter (gedrückt halten: fortlaufend)", "One frame forward (hold: continuous)"))
            } else {
                (tr!("Ein Schlüsselbild zurück (gedrückt halten: fortlaufend)", "One keyframe back (hold: continuous)"), tr!("Ein Schlüsselbild weiter (gedrückt halten: fortlaufend)", "One keyframe forward (hold: continuous)"))
            };
            let r = super::style::icon_button(ui, &p, ic::CARET_LEFT, back_tip);
            st.hold_step(&r, -1, enabled);
            let icon = if st.playing { ic::PAUSE } else { ic::PLAY };
            let b = egui::Button::new(egui::RichText::new(icon).size(18.0).color(p.on_accent))
                .fill(p.accent)
                .stroke(egui::Stroke::NONE)
                .corner_radius(CornerRadius::same(18))
                .min_size(Vec2::splat(36.0));
            if ui.add_enabled(enabled, b).on_hover_text(if st.playing { tr!("Anhalten", "Pause") } else { tr!("Abspielen", "Play") }).clicked() {
                let on = !st.playing;
                st.set_playing(on);
                st.next_at = None;
                // Am Ende von vorn beginnen
                if st.playing && st.current_f() + st.frame_len() * 1.5 >= hi {
                    if prog_mode && st.prog_cur + 1 >= st.prog.len() {
                        let s = st.prog[0].1;
                        st.prog_goto(0, s);
                    } else if !prog_mode {
                        st.seek_time(lo, exact);
                    }
                }
            }
            let r = super::style::icon_button(ui, &p, ic::CARET_RIGHT, fwd_tip);
            st.hold_step(&r, 1, enabled);
            let next_tip = if prog_mode { tr!("Zum nächsten Eintrag", "Next entry") } else { tr!("Zum nächsten Kapitel", "Next chapter") };
            if super::style::icon_button(ui, &p, ic::SKIP_FORWARD, next_tip).clicked() && enabled {
                if prog_mode && st.prog_cur + 1 < st.prog.len() {
                    let k = st.prog_cur + 1;
                    let s = st.prog[k].1;
                    st.prog_goto(k, s);
                } else {
                    // Nächstes Kapitel, sonst das Ende des Titels (bzw. des Schnitts)
                    let next = if prog_mode {
                        None
                    } else {
                        st.chapters.iter().map(|c| c.0 as f64).find(|ms| *ms > cur as f64 + 500.0 && *ms < hi)
                    };
                    match next {
                        Some(t) => st.seek_time(t, exact),
                        None => {
                            st.set_playing(false);
                            let end = if exact { hi - st.frame_len() } else { hi - 1.0 };
                            st.seek_time(end.max(lo), exact);
                        }
                    }
                }
            }

            // Um eine wählbare Zahl von Sekunden springen
            ui.add_space(6.0);
            let secs = (*jump_secs).max(1);
            if ui
                .add_enabled(enabled, egui::Button::new(RichText::new(format!("{}  {secs} s", ic::CARET_DOUBLE_LEFT)).size(13.0)))
                .on_hover_text(trf!("{secs} Sekunden zurück", "{secs} seconds back"))
                .clicked()
            {
                st.jump(-f64::from(secs) * 1000.0);
            }
            ui.add(egui::DragValue::new(jump_secs).range(1..=600).suffix(" s").speed(0.5))
                .on_hover_text(tr!("Sprungweite in Sekunden", "Jump size in seconds"));
            if ui
                .add_enabled(enabled, egui::Button::new(RichText::new(format!("{secs} s  {}", ic::CARET_DOUBLE_RIGHT)).size(13.0)))
                .on_hover_text(trf!("{secs} Sekunden vor", "{secs} seconds forward"))
                .clicked()
            {
                st.jump(f64::from(secs) * 1000.0);
            }

            ui.add_space(8.0);
            let at = match (st.drag, st.strip.drag) {
                (Some(f), _) => f64::from(f) * g_total,
                (None, Some(t)) if !prog_mode => t - lo,
                _ => g_pos,
            };
            let mut text = format!(
                "{} / {}",
                super::views::fmt_time_precise(at.max(0.0) as u64),
                fmt_time(g_total as u64)
            );
            if prog_mode {
                text.push_str(&trf!("  ·  Eintrag {} von {}", "  ·  Entry {} of {}", st.prog_cur + 1, st.prog.len()));
            } else if st.window.is_none() {
                if let Some(len) = st.cut_len {
                    text.push_str(&trf!("  ·  Schnitt {}", "  ·  Cut {}", fmt_time(len)));
                }
            }
            ui.label(RichText::new(text).size(13.0).color(p.muted));
            if st.chapters.len() > 1 && !prog_mode {
                let current = st.chapters.iter().rev().find(|c| c.0 <= cur).map(|c| c.1.clone()).unwrap_or_default();
                let mut jump = None;
                egui::ComboBox::from_id_salt("kapitelwahl").width(100.0).selected_text(current).show_ui(ui, |ui| {
                    for (ms, name) in &st.chapters {
                        if ui.selectable_label(false, format!("{name}  ·  {}", fmt_time(*ms))).clicked() {
                            jump = Some(*ms);
                        }
                    }
                });
                if let Some(ms) = jump {
                    st.seek_time(ms as f64, exact);
                }
            }
        });

        // Zweite Zeile: Zeitleiste ein/aus, Ansicht, Ton, Kompression
        ui.horizontal(|ui| {
            let tip = if *timeline_open {
                tr!("Wieder die schmale Leiste zeigen", "Show the slim bar again")
            } else {
                tr!("Zoombare Zeitleiste mit Standbildern zeigen, zum genauen Setzen von Schnitten", "Show the zoomable timeline with stills, for precise cuts")
            };
            if ui
                .selectable_label(*timeline_open, RichText::new(trf!("{}  Zeitleiste", "{}  Timeline", ic::FILM_STRIP)).size(13.0))
                .on_hover_text(tip)
                .clicked()
            {
                *timeline_open = !*timeline_open;
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                super::style::check(ui, &p, &mut st.compressed, tr!("Mit gewählter Kompression", "With chosen compression"));
                if can_all || st.cut_len.is_some() {
                    let mut view: u8 = if st.program { 2 } else if st.cut_view { 1 } else { 0 };
                    let before = view;
                    if can_all {
                        super::style::segmented(ui, &p, &mut view, &[(0, tr!("Ganzer Titel", "Whole title")), (1, tr!("Nur Schnitt", "Cut only")), (2, tr!("Alle Schnitte", "All cuts"))], 300.0);
                    } else {
                        super::style::segmented(ui, &p, &mut view, &[(0, tr!("Ganzer Titel", "Whole title")), (1, tr!("Nur Schnitt", "Cut only"))], 200.0);
                    }
                    if view != before {
                        st.cut_view = view == 1;
                        let to_all = view == 2;
                        if to_all && !st.program {
                            // Beim gewählten Eintrag beginnen
                            let k = sel_entry.filter(|i| *i < st.prog.len()).unwrap_or(0);
                            st.program = true;
                            st.prog_cur = k;
                            st.prog_seek = st.prog.get(k).map(|c| c.1);
                            st.prog_play = Some(false);
                        } else if !to_all {
                            st.program = false;
                        }
                    }
                }
                if let Some(bus) = st.audio.clone() {
                    let muted = bus.muted();
                    let (icon, tip) = if muted { (ic::SPEAKER_SLASH, tr!("Ton einschalten", "Unmute")) } else { (ic::SPEAKER_HIGH, tr!("Ton ausschalten", "Mute")) };
                    if super::style::icon_button(ui, &p, icon, tip).clicked() {
                        bus.set_muted(!muted);
                    }
                }
            });
        });

        // Schnittmarke aus dem Kontextmenü der Zeitleiste übernehmen
        if let Some((entry, start, at)) = cut_action {
            if let (Some(disc), Some(e)) = (self.disc.clone(), self.plan.reauthor.get_mut(entry)) {
                let total = discslim::engine::plan::entry_duration_ms(&disc, e);
                let at = at.min(total);
                if start {
                    e.cut.start_ms = Some(at);
                    if e.cut.end() <= at {
                        e.cut.end_ms = Some(total);
                    }
                } else {
                    e.cut.end_ms = Some(at.max(e.cut.start() + 40));
                }
            }
        }
    }
}
