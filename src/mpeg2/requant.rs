//! Verkleinert MPEG-2-Video ohne vollständiges Neukodieren: Die DCT-Koeffizienten
//! werden mit einem gröberen Quantisierer neu geschrieben, Bewegungsvektoren und
//! Struktur bleiben unverändert. Was sich nicht sauber lesen lässt, wird unverändert
//! durchgereicht, damit nie ein kaputter Datenstrom entsteht.

use super::bits::{next_start_code, BitReader, BitWriter};
use super::headers::{PicInfo, SeqInfo, PIC_B, PIC_I, PIC_P, STRUCT_FRAME};
use super::tables::*;

/// Einstellungen für einen Videoabschnitt.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoOpts {
    /// Ziel: Ausgabegröße / Eingabegröße (1.0 = unverändert).
    pub ratio: f32,
    /// Koeffizienten ab dieser Zickzack-Position entfallen (64 = keine Glättung).
    pub smooth_cutoff: u8,
    /// Nur ein Standbild behalten, Bildanzahl und Zeitablauf bleiben erhalten.
    pub still: bool,
}

impl Default for VideoOpts {
    fn default() -> Self {
        Self { ratio: 1.0, smooth_cutoff: 64, still: false }
    }
}

impl VideoOpts {
    pub fn is_passthrough(&self) -> bool {
        self.ratio >= 0.999 && self.smooth_cutoff >= 64 && !self.still
    }
}

/// Lage eines Bildes im Ein- und Ausgabestrom.
#[derive(Clone, Copy, Debug)]
pub struct PicRec {
    pub in_start: usize,
    pub in_end: usize,
    pub out_start: usize,
    pub out_end: usize,
    pub coding_type: u8,
    pub structure: u8,
}

#[derive(Default)]
pub struct EsOut {
    pub es: Vec<u8>,
    /// Monoton steigende Zuordnung (Eingabe-Offset, Ausgabe-Offset).
    pub pos_map: Vec<(u32, u32)>,
    pub pics: Vec<PicRec>,
}

impl EsOut {
    /// Ausgabeposition zu einer Eingabeposition.
    pub fn map_pos(&self, input: usize) -> usize {
        let input = input as u32;
        let idx = self.pos_map.partition_point(|p| p.0 <= input);
        if idx == 0 {
            0
        } else {
            self.pos_map[idx - 1].1 as usize
        }
    }
}

const BLOCKS: usize = 6;

pub struct Requant {
    pub seq: SeqInfo,
    pub pic: PicInfo,
    opts: VideoOpts,
    factor: f64,
    in_win: f64,
    out_win: f64,
    qmap: [u8; 32],
    qmap_nl: bool,
    qmap_factor: f64,
    // Standbild-Zustand
    still_slices: Option<Vec<u8>>,
    still_key: (u8, bool, bool, bool, bool, bool),
    still_replay: bool,
    still_capture: bool,
    still_blocked: bool,
    // Wiederverwendete Puffer
    tokens: [Vec<(u8, i16)>; BLOCKS],
    dc_range: [(usize, usize); BLOCKS],
    /// Statistik über alles, was bisher durchlief.
    pub total_in: u64,
    pub total_out: u64,
    pub slices_fallback: u64,
    pub slices_total: u64,
}

impl Default for Requant {
    fn default() -> Self {
        Self::new()
    }
}

impl Requant {
    pub fn new() -> Self {
        Self {
            seq: SeqInfo::default(),
            pic: PicInfo::default(),
            opts: VideoOpts::default(),
            factor: 1.0,
            in_win: 0.0,
            out_win: 0.0,
            qmap: [0; 32],
            qmap_nl: false,
            qmap_factor: -1.0,
            still_slices: None,
            still_key: (0, false, false, false, false, false),
            still_replay: false,
            still_capture: false,
            still_blocked: false,
            tokens: Default::default(),
            dc_range: [(0, 0); BLOCKS],
            total_in: 0,
            total_out: 0,
            slices_fallback: 0,
            slices_total: 0,
        }
    }

    /// Setzt die Vorgabe für den folgenden Abschnitt. Der Regelkreis startet nur neu,
    /// wenn sich das Ziel spürbar ändert.
    pub fn set_opts(&mut self, opts: VideoOpts) {
        let changed = (opts.ratio - self.opts.ratio).abs() > 0.15
            || (opts.ratio >= 0.999) != (self.opts.ratio >= 0.999)
            || opts.still != self.opts.still
            || opts.smooth_cutoff != self.opts.smooth_cutoff;
        self.opts = opts;
        if changed {
            let r = f64::from(opts.ratio).clamp(0.02, 1.0);
            self.factor = (1.0 / r).powf(1.25);
            self.in_win = 0.0;
            self.out_win = 0.0;
            self.still_slices = None;
            self.still_blocked = false;
        }
    }

    /// Eine neue Zelle beginnt: Ein Standbild gilt immer nur für seine Zelle.
    pub fn new_cell(&mut self) {
        self.still_slices = None;
        self.still_blocked = false;
    }

    /// Aktueller Regelfaktor (für die Anzeige der Qualität).
    pub fn current_factor(&self) -> f64 {
        self.factor
    }

    fn rate_update(&mut self) {
        if self.opts.ratio >= 0.999 || self.in_win < 16_000.0 {
            return;
        }
        let target = self.in_win * f64::from(self.opts.ratio);
        let err = self.out_win / target.max(1.0);
        let step = err.powf(0.4).clamp(0.85, 1.2);
        self.factor = (self.factor * step).clamp(1.0, 24.0);
        self.in_win *= 0.97;
        self.out_win *= 0.97;
    }

    fn picture_factor(&self) -> f64 {
        if self.still_blocked {
            return self.factor.max(4.0);
        }
        let weight = match self.pic.coding_type {
            PIC_I => 0.85,
            PIC_B => 1.2,
            _ => 1.0,
        };
        (self.factor * weight).max(1.0)
    }

    fn prepare_qmap(&mut self, factor: f64) {
        let nl = self.pic.q_scale_type;
        if self.qmap_nl == nl && (self.qmap_factor - factor).abs() < 1e-9 {
            return;
        }
        self.qmap_nl = nl;
        self.qmap_factor = factor;
        self.qmap[0] = 0;
        for code in 1..32u8 {
            let want = f64::from(qscale(code, nl)) * factor;
            let mut best = code;
            let mut best_d = f64::MAX;
            for c in code..32 {
                let d = (f64::from(qscale(c, nl)) - want).abs();
                if d < best_d {
                    best_d = d;
                    best = c;
                }
            }
            self.qmap[code as usize] = best;
        }
    }

    /// Verarbeitet einen Abschnitt des Elementarstroms (in der Regel eine VOBU).
    pub fn process(&mut self, es: &[u8]) -> EsOut {
        let mut out = EsOut {
            es: Vec::with_capacity(es.len()),
            pos_map: Vec::with_capacity(64),
            pics: Vec::with_capacity(16),
        };
        out.pos_map.push((0, 0));

        let mut i = match next_start_code(es, 0) {
            Some(p) => p,
            None => {
                out.es.extend_from_slice(es);
                out.pos_map.push((es.len() as u32, es.len() as u32));
                self.total_in += es.len() as u64;
                self.total_out += es.len() as u64;
                return out;
            }
        };
        out.es.extend_from_slice(&es[..i]);
        out.pos_map.push((i as u32, i as u32));

        let mut open_pic: Option<usize> = None;
        let mut stripped_slices = false;

        while i + 4 <= es.len() {
            let code = es[i + 3];
            let j = next_start_code(es, i + 4).unwrap_or(es.len());
            let unit = &es[i..j];
            let body = &es[i + 4..j];
            let out_before = out.es.len();

            match code {
                0xB3 => {
                    self.seq.parse_seq_header(body);
                    self.still_slices = None;
                    Self::close_pic(&mut out, &mut open_pic, i);
                    out.es.extend_from_slice(unit);
                }
                0xB5 => {
                    match body.first().map(|b| b >> 4) {
                        Some(1) => self.seq.parse_seq_ext(body),
                        Some(3) => self.seq.parse_quant_ext(body),
                        Some(8) => self.pic.parse_ext(body),
                        _ => {}
                    }
                    out.es.extend_from_slice(unit);
                }
                0x00 => {
                    Self::close_pic(&mut out, &mut open_pic, i);
                    self.pic.parse_header(body);
                    self.rate_update();
                    out.pics.push(PicRec {
                        in_start: i,
                        in_end: j,
                        out_start: out.es.len(),
                        out_end: out.es.len(),
                        coding_type: self.pic.coding_type,
                        structure: STRUCT_FRAME,
                    });
                    open_pic = Some(out.pics.len() - 1);
                    stripped_slices = false;
                    self.still_replay = false;
                    self.still_capture = false;
                    out.es.extend_from_slice(unit);
                }
                0x01..=0xAF => {
                    if let Some(p) = open_pic {
                        out.pics[p].structure = self.pic.structure;
                    }
                    self.handle_slice(code, unit, body, &mut out.es, &mut stripped_slices);
                    if let Some(p) = open_pic {
                        out.pics[p].in_end = j;
                        out.pics[p].out_end = out.es.len();
                    }
                }
                0xB7 | 0xB8 => {
                    Self::close_pic(&mut out, &mut open_pic, i);
                    out.es.extend_from_slice(unit);
                }
                _ => out.es.extend_from_slice(unit),
            }

            let in_len = unit.len() as u64;
            let out_len = (out.es.len() - out_before) as u64;
            self.total_in += in_len;
            self.total_out += out_len;
            self.in_win += in_len as f64;
            self.out_win += out_len as f64;
            out.pos_map.push((j as u32, out.es.len() as u32));
            i = j;
        }
        // Ein unvollständiger Rest ohne Startcode bleibt erhalten.
        if i < es.len() {
            out.es.extend_from_slice(&es[i..]);
            self.total_in += (es.len() - i) as u64;
            self.total_out += (es.len() - i) as u64;
            out.pos_map.push((es.len() as u32, out.es.len() as u32));
        }
        out
    }

    fn close_pic(out: &mut EsOut, open: &mut Option<usize>, _at: usize) {
        *open = None;
        let _ = out;
    }

    fn can_requant(&self) -> bool {
        self.seq.mpeg2
            && self.seq.chroma_format == 1
            && self.pic.has_ext
            && (PIC_I..=PIC_B).contains(&self.pic.coding_type)
            && self.pic.structure != 0
    }

    fn still_params_key(&self) -> (u8, bool, bool, bool, bool, bool) {
        (
            self.pic.intra_dc_precision,
            self.pic.intra_vlc_format,
            self.pic.q_scale_type,
            self.pic.concealment_mv,
            self.pic.alternate_scan,
            self.pic.frame_pred_frame_dct,
        )
    }

    fn handle_slice(
        &mut self,
        code: u8,
        unit: &[u8],
        body: &[u8],
        out: &mut Vec<u8>,
        stripped: &mut bool,
    ) {
        self.slices_total += 1;
        if !self.can_requant() {
            self.slices_fallback += 1;
            out.extend_from_slice(unit);
            return;
        }

        if self.opts.still && !self.still_blocked {
            if self.pic.structure != STRUCT_FRAME {
                // Halbbild-Material: Standbilder gehen nur mit Vollbildern; stark verkleinern.
                self.still_blocked = true;
            } else {
                if !*stripped {
                    *stripped = true;
                    match self.pic.coding_type {
                        PIC_I => {
                            let key = self.still_params_key();
                            if self.still_slices.is_some() && self.still_key == key {
                                out.extend_from_slice(self.still_slices.as_ref().unwrap());
                                self.still_replay = true;
                                self.still_capture = false;
                            } else {
                                self.still_key = key;
                                self.still_slices = Some(Vec::new());
                                self.still_capture = true;
                                self.still_replay = false;
                            }
                        }
                        _ => {
                            self.write_skip_picture(out);
                            self.still_replay = true;
                            self.still_capture = false;
                        }
                    }
                }
                if self.still_replay {
                    return;
                }
                if self.still_capture {
                    // Das behaltene I-Bild wird etwas gröber, es wiederholt sich oft.
                    let start = out.len();
                    self.requant_into(code, unit, body, out, self.factor.max(1.6));
                    let captured = out[start..].to_vec();
                    if let Some(s) = self.still_slices.as_mut() {
                        s.extend_from_slice(&captured);
                    }
                    return;
                }
            }
        }

        if self.opts.is_passthrough() {
            out.extend_from_slice(unit);
            return;
        }
        let f = self.picture_factor();
        self.requant_into(code, unit, body, out, f);
    }

    fn requant_into(&mut self, code: u8, unit: &[u8], body: &[u8], out: &mut Vec<u8>, factor: f64) {
        self.prepare_qmap(factor);
        let mut w = BitWriter::new();
        w.out.reserve(unit.len());
        w.out.extend_from_slice(&[0, 0, 1, code]);
        if self.requant_slice(body, &mut w).is_some() {
            w.align();
            if w.out.len() < unit.len() {
                out.extend_from_slice(&w.out);
                return;
            }
        } else {
            self.slices_fallback += 1;
        }
        out.extend_from_slice(unit);
    }

    /// Schreibt alle Zeilen eines Bildes als "keine Änderung gegenüber der Referenz".
    fn write_skip_picture(&self, out: &mut Vec<u8>) {
        let t = tables();
        let ct = self.pic.coding_type;
        let flags = if ct == PIC_B && self.pic.f_code[1][0] != 15 { MB_BWD } else { MB_FWD };
        let (ty, ty_len) = t.mb_enc[(ct - 1) as usize][flags as usize];
        let mbw = self.seq.mb_width();
        let mbh = self.seq.mb_height();
        let mut w = BitWriter::new();
        let write_mb = |w: &mut BitWriter| {
            w.put(u32::from(ty), u32::from(ty_len));
            if !self.pic.frame_pred_frame_dct {
                w.put(2, 2); // Bewegung im Vollbild
            }
            w.put(1, 1); // waagerecht 0
            w.put(1, 1); // senkrecht 0
        };
        for row in 0..mbh.min(175) {
            w.start_code((row + 1) as u8);
            w.put(1, 5);
            w.put(0, 1);
            w.put(1, 1);
            write_mb(&mut w);
            if mbw > 1 {
                let mut inc = mbw - 1;
                while inc > 33 {
                    w.put(0b00000001000, 11);
                    inc -= 33;
                }
                let (c, n) = t.addr_enc[inc];
                w.put(u32::from(c), u32::from(n));
                write_mb(&mut w);
            }
        }
        w.align();
        out.extend_from_slice(&w.out);
    }

    fn skip_mv(r: &mut BitReader, f_code: [u8; 2], dmv: bool) -> Option<()> {
        let t = tables();
        for f in f_code {
            let (c, len) = t.motion.lookup(r.peek(11));
            if len == 0 {
                return None;
            }
            r.skip(u32::from(len));
            if f > 1 && c != 0 {
                r.skip(u32::from(f) - 1);
            }
            if dmv {
                if r.peek(1) == 0 {
                    r.skip(1);
                } else {
                    r.skip(2);
                }
            }
        }
        Some(())
    }

    fn skip_motion(&self, r: &mut BitReader, s: usize, motion_type: u8) -> Option<()> {
        let frame = self.pic.structure == STRUCT_FRAME;
        let fc = self.pic.f_code[s];
        match (frame, motion_type) {
            // Vollbild-Vektor
            (true, 2) => Self::skip_mv(r, fc, false),
            // Zwei Halbbild-Vektoren: im Vollbild oder als 16x8 im Halbbild
            (true, 1) | (false, 2) => {
                r.skip(1);
                Self::skip_mv(r, fc, false)?;
                r.skip(1);
                Self::skip_mv(r, fc, false)
            }
            // Dual Prime hat keine Halbbildauswahl
            (_, 3) => Self::skip_mv(r, fc, true),
            // Ein Halbbild-Vektor im Halbbild
            (false, 1) => {
                r.skip(1);
                Self::skip_mv(r, fc, false)
            }
            _ => None,
        }
    }

    /// Liest eine Slice und schreibt sie mit dem neuen Quantisierer.
    fn requant_slice(&mut self, data: &[u8], w: &mut BitWriter) -> Option<()> {
        let t = tables();
        let mut r = BitReader::new(data);
        let nl = self.pic.q_scale_type;
        let ct = self.pic.coding_type;
        let ct_idx = (ct - 1) as usize;
        let frame = self.pic.structure == STRUCT_FRAME;
        let cutoff = self.opts.smooth_cutoff;

        let mut q_in = r.get(5) as u8;
        if q_in == 0 {
            return None;
        }
        let mut q_state = self.qmap[q_in as usize];
        w.put(u32::from(q_state), 5);

        let extra_from = r.pos();
        while r.peek(1) == 1 {
            r.skip(9);
            if r.overrun() {
                return None;
            }
        }
        r.skip(1);
        w.copy_bits(data, extra_from, r.pos());

        let mut mb_count = 0usize;
        loop {
            // Adress-Inkrement unverändert übernehmen
            let a0 = r.pos();
            loop {
                let (v, len) = t.addr.lookup(r.peek(11));
                if len == 0 {
                    return None;
                }
                r.skip(u32::from(len));
                if v != ADDR_ESCAPE {
                    break;
                }
            }
            let a1 = r.pos();

            let (flags, len) = t.mb_type[ct_idx][r.peek(6) as usize];
            if len == 0 {
                return None;
            }
            r.skip(u32::from(len));
            let intra = flags & MB_INTRA != 0;
            let pattern = flags & MB_PATTERN != 0;

            let m0 = r.pos();
            let mut motion_type = if frame { 2u8 } else { 1u8 };
            if flags & (MB_FWD | MB_BWD) != 0 && (!frame || !self.pic.frame_pred_frame_dct) {
                motion_type = r.get(2) as u8;
                if motion_type == 0 {
                    return None;
                }
            }
            let m1 = r.pos();
            let mut dct_bit = None;
            if frame && !self.pic.frame_pred_frame_dct && (intra || pattern) {
                dct_bit = Some(r.get(1));
            }
            if flags & MB_QUANT != 0 {
                q_in = r.get(5) as u8;
                if q_in == 0 {
                    return None;
                }
            }
            let v0 = r.pos();
            if flags & MB_FWD != 0 || (intra && self.pic.concealment_mv) {
                self.skip_motion(&mut r, 0, motion_type)?;
            }
            if flags & MB_BWD != 0 {
                self.skip_motion(&mut r, 1, motion_type)?;
            }
            if intra && self.pic.concealment_mv {
                r.skip(1);
            }
            let v1 = r.pos();

            let mut cbp = 0u8;
            if intra {
                cbp = 0x3F;
            } else if pattern {
                let (c, len) = t.cbp.lookup(r.peek(9));
                if len == 0 {
                    return None;
                }
                r.skip(u32::from(len));
                cbp = c as u8;
            }

            // Blöcke lesen und mit neuem Quantisierer umrechnen
            let q_target = self.qmap[q_in as usize];
            let q_in_val = qscale(q_in, nl);
            let q_out_val = qscale(q_target, nl);
            let mut first_token: Option<(usize, u8, bool)> = None;
            let mut cbp_out = 0u8;
            let table = usize::from(intra && self.pic.intra_vlc_format);

            for b in 0..BLOCKS {
                self.tokens[b].clear();
                if cbp & (1 << (5 - b)) == 0 {
                    continue;
                }
                let dct = &t.dct[table];
                let mut idx: i32 = -1;
                if intra {
                    let d0 = r.pos();
                    let size = if b < 4 {
                        let (s, len) = t.dc_luma.lookup(r.peek(9));
                        if len == 0 {
                            return None;
                        }
                        r.skip(u32::from(len));
                        s
                    } else {
                        let (s, len) = t.dc_chroma.lookup(r.peek(10));
                        if len == 0 {
                            return None;
                        }
                        r.skip(u32::from(len));
                        s
                    };
                    r.skip(size as u32);
                    self.dc_range[b] = (d0, r.pos());
                    idx = 0;
                }
                let mut first = !intra;
                loop {
                    let run: i32;
                    let level: i32;
                    if first && r.peek(1) == 1 {
                        r.skip(1);
                        run = 0;
                        level = if r.get(1) == 1 { -1 } else { 1 };
                    } else {
                        let e = dct.dec[r.peek(16) as usize];
                        if e.len == 0 {
                            return None;
                        }
                        r.skip(u32::from(e.len));
                        if e.run == TOK_EOB {
                            break;
                        }
                        if e.run == TOK_ESC {
                            run = r.get(6) as i32;
                            level = r.get_signed(12);
                            if level == 0 {
                                return None;
                            }
                        } else {
                            run = i32::from(e.run);
                            level = if r.get(1) == 1 { -i32::from(e.level) } else { i32::from(e.level) };
                        }
                    }
                    first = false;
                    idx += run + 1;
                    if idx > 63 {
                        return None;
                    }
                    let abs = level.unsigned_abs();
                    if first_token.is_none() && !intra {
                        first_token = Some((b, idx as u8, level < 0));
                    }
                    if (idx as u8) >= cutoff {
                        continue;
                    }
                    let new_abs = if q_in_val == q_out_val {
                        abs
                    } else if intra {
                        (abs * q_in_val + q_out_val / 2) / q_out_val
                    } else {
                        ((2 * abs + 1) * q_in_val) / (2 * q_out_val)
                    };
                    if new_abs > 0 {
                        let v = new_abs.min(2047) as i16;
                        self.tokens[b].push((idx as u8, if level < 0 { -v } else { v }));
                    }
                }
                if !self.tokens[b].is_empty() {
                    cbp_out |= 1 << (5 - b);
                }
            }
            if r.overrun() {
                return None;
            }

            let mut flags_out = flags & !MB_QUANT;
            if pattern && cbp_out == 0 {
                if flags & (MB_FWD | MB_BWD) != 0 {
                    // Mit Bewegung und ohne Rest: Typ "nicht kodiert" genügt.
                    flags_out &= !MB_PATTERN;
                } else if let Some((b, idx, neg)) = first_token {
                    // Ohne Bewegung muss ein Koeffizient stehen bleiben.
                    self.tokens[b].push((idx, if neg { -1 } else { 1 }));
                    cbp_out |= 1 << (5 - b);
                } else {
                    return None;
                }
            }
            let uses_q = intra || flags_out & MB_PATTERN != 0;
            let need_q = uses_q && q_target != q_state;
            if need_q {
                flags_out |= MB_QUANT;
                q_state = q_target;
            }

            // Makroblock schreiben
            w.copy_bits(data, a0, a1);
            let (c, n) = t.mb_enc[ct_idx][flags_out as usize];
            if n == 0 {
                return None;
            }
            w.put(u32::from(c), u32::from(n));
            w.copy_bits(data, m0, m1);
            if uses_q {
                if let Some(bit) = dct_bit {
                    w.put(bit, 1);
                }
            }
            if need_q {
                w.put(u32::from(q_state), 5);
            }
            w.copy_bits(data, v0, v1);
            if flags_out & MB_PATTERN != 0 {
                let (c, n) = t.cbp_enc[cbp_out as usize];
                w.put(u32::from(c), u32::from(n));
            }
            for b in 0..BLOCKS {
                let coded = if intra { true } else { cbp_out & (1 << (5 - b)) != 0 };
                if !coded {
                    continue;
                }
                if intra {
                    let (d0, d1) = self.dc_range[b];
                    w.copy_bits(data, d0, d1);
                }
                write_block(w, &self.tokens[b], intra, table);
            }

            mb_count += 1;
            if mb_count > 16384 {
                return None;
            }
            if r.at_slice_end() {
                break;
            }
        }
        Some(())
    }
}

/// Schreibt eine Koeffizientenliste samt Blockende.
#[inline]
pub fn write_block(w: &mut BitWriter, tokens: &[(u8, i16)], intra: bool, table: usize) {
    let t = tables();
    let dct = &t.dct[table];
    let mut last: i32 = if intra { 0 } else { -1 };
    let mut first = !intra;
    for &(idx, lvl) in tokens {
        let run = (i32::from(idx) - last - 1) as usize;
        last = i32::from(idx);
        let abs = lvl.unsigned_abs() as usize;
        let sign = u32::from(lvl < 0);
        if first && run == 0 && abs == 1 {
            w.put(1, 1);
            w.put(sign, 1);
        } else if run < 32 && abs <= 40 && dct.enc[run][abs].1 > 0 {
            let (c, n) = dct.enc[run][abs];
            w.put(u32::from(c), u32::from(n));
            w.put(sign, 1);
        } else {
            w.put(1, 6);
            w.put(run as u32, 6);
            w.put((i32::from(lvl) & 0xFFF) as u32, 12);
        }
        first = false;
    }
    w.put(u32::from(dct.eob.0), u32::from(dct.eob.1));
}

#[allow(dead_code)]
const _: u8 = PIC_P;
