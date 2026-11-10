//! Vorschaufläche: holt Einzelbilder aus dem Hintergrund und spielt sie zeitrichtig ab.

use std::time::{Duration, Instant};

use eframe::egui::{self, Align2, Color32, CornerRadius, FontId, Rect, RichText, Sense, TextureOptions, Vec2};
use egui_phosphor::regular as ic;

use discslim::dvd::model::{fmt_time, Disc};
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
}

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
        self.preview.stop();
        self.preview.loaded = Some(want);
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
        if self.preview.handle.is_some() && self.preview.total_ms > 0 {
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
        let controls_h = 124.0;
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
        } else {
            let text = if st.segments.is_empty() { tr!("Für diese Auswahl gibt es keine Vorschau", "No preview is available for this selection") } else { tr!("Vorschau wird geladen …", "Loading preview …") };
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

        ui.add_space(8.0);
        // Zeitleiste
        let width = ui.available_width();
        let (bar, resp) = ui.allocate_exact_size(Vec2::new(width, 18.0), Sense::click_and_drag());
        let track = Rect::from_center_size(bar.center(), Vec2::new(bar.width(), 6.0));
        ui.painter().rect_filled(track, CornerRadius::same(3), p.track);
        let prog_mode = st.program && !st.prog.is_empty();
        let (lo, hi) = st.range();
        // Bei "Alle Schnitte" gilt eine gemeinsame Zeitleiste über alle Abschnitte.
        let (g_pos, g_total) = if prog_mode {
            let (_, total) = st.prog_offsets();
            (st.prog_position(), total)
        } else {
            (st.current_f() - lo, hi - lo)
        };
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
            // Grenzen zwischen den Abschnitten
            let (offs, _) = st.prog_offsets();
            for o in offs.iter().skip(1) {
                let x = track.left() + track.width() * (*o as f32 / span).min(1.0);
                ui.painter().line_segment(
                    [egui::pos2(x, track.top() - 5.0), egui::pos2(x, track.bottom() + 5.0)],
                    egui::Stroke::new(2.5, p.good),
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
                resp.clone().on_hover_text(trf!("Eintrag {} · {}", "Entry {} · {}", k + 1, fmt_time(g as u64)));
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
                st.drag = Some(((pos.x - track.left()) / track.width().max(1.0)).clamp(0.0, 1.0));
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
            if let Some(f) = st.drag.take() {
                if prog_mode {
                    st.prog_seek_global(f64::from(f) * g_total);
                } else {
                    let exact = st.step_exact;
                    st.seek_time(lo + f64::from(f) * (hi - lo), exact);
                }
            }
        }

        let jump_secs = &mut self.prefs.jump_secs;
        // Erste Zeile: Navigation, Sprünge, Zeit, Kapitel
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
            let at = match st.drag {
                Some(f) => f64::from(f) * g_total,
                None => g_pos,
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

        // Zweite Zeile: Ansicht, Ton, Kompression
        ui.horizontal(|ui| {
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
