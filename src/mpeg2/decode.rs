//! MPEG-2-Decoder für die Vorschau (Vollbilder, Halbbilder, Dual Prime, 4:2:0).

use std::collections::VecDeque;

use super::bits::{next_start_code, BitReader};
use super::headers::{PicInfo, SeqInfo, PIC_B, PIC_I, PIC_P, STRUCT_BOTTOM, STRUCT_FRAME};
use super::tables::*;

#[derive(Clone, Default)]
pub struct Frame {
    pub stride: usize,
    pub rows: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    interlaced: bool,
    rff: bool,
    tag: u64,
    gop: u32,
    tr: u16,
}

impl Frame {
    fn alloc(&mut self, stride: usize, rows: usize) {
        self.stride = stride;
        self.rows = rows;
        self.y = vec![16; stride * rows];
        self.u = vec![128; stride / 2 * rows / 2];
        self.v = vec![128; stride / 2 * rows / 2];
    }
}

/// Fertiges Bild in Anzeigereihenfolge.
#[derive(Clone)]
pub struct DecodedFrame {
    pub width: usize,
    pub height: usize,
    pub stride: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub aspect: f32,
    /// Dauer in Sekunden
    pub duration: f64,
    pub interlaced: bool,
    /// Kennung des Datenstücks, in dem das Bild begann (siehe push_tagged).
    pub tag: u64,
    /// Laufende Nummer der Bildgruppe innerhalb dieses Stücks.
    pub gop: u32,
    /// Anzeigeposition innerhalb der Bildgruppe.
    pub tr: u16,
    /// Dauer eines Bildes ohne Wiederholung, in Millisekunden.
    pub frame_ms: f64,
}

impl DecodedFrame {
    /// Rechnet nach RGBA um; Zeilensprung-Material wird für die Anzeige geglättet.
    pub fn to_rgba(&self) -> Vec<u8> {
        let (w, h) = (self.width, self.height);
        let mut out = vec![255u8; w * h * 4];
        let cs = self.stride / 2;
        for row in 0..h {
            let line = &self.y[row * self.stride..row * self.stride + w];
            let next_row = if row + 1 < h { row + 1 } else { row };
            let next = &self.y[next_row * self.stride..next_row * self.stride + w];
            let cu = &self.u[(row / 2) * cs..];
            let cv = &self.v[(row / 2) * cs..];
            let dst = &mut out[row * w * 4..(row + 1) * w * 4];
            for x in 0..w {
                let luma = if self.interlaced {
                    (i32::from(line[x]) + i32::from(next[x]) + 1) >> 1
                } else {
                    i32::from(line[x])
                };
                let c = (luma - 16) * 298;
                let d = i32::from(cu[x / 2]) - 128;
                let e = i32::from(cv[x / 2]) - 128;
                let px = &mut dst[x * 4..x * 4 + 4];
                px[0] = ((c + 409 * e + 128) >> 8).clamp(0, 255) as u8;
                px[1] = ((c - 100 * d - 208 * e + 128) >> 8).clamp(0, 255) as u8;
                px[2] = ((c + 516 * d + 128) >> 8).clamp(0, 255) as u8;
            }
        }
        out
    }
}

pub struct Decoder {
    pub seq: SeqInfo,
    pic: PicInfo,
    frames: [Frame; 3],
    fwd: usize,
    bwd: usize,
    scratch: usize,
    cur: usize,
    refs: u8,
    second_field: bool,
    in_field_pair: bool,
    first_field_struct: u8,
    in_picture: bool,
    skip_picture: bool,
    allocated: bool,
    tail: Vec<u8>,
    out: VecDeque<DecodedFrame>,
    pred_y: [u8; 256],
    pred_u: [u8; 64],
    pred_v: [u8; 64],
    pmv: [[[i32; 2]; 2]; 2],
    mv: [[[i32; 2]; 2]; 2],
    field_sel: [[u8; 2]; 2],
    dmv: [i32; 2],
    dc_pred: [i32; 3],
    last_flags: u8,
    q_code: u8,
    cur_tag: u64,
    gop_in_tag: i32,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

// Fehler in einer Slice: Der Rest der Slice entfällt.
struct BadSlice;
type R<T> = Result<T, BadSlice>;

impl Decoder {
    pub fn new() -> Self {
        Self {
            seq: SeqInfo::default(),
            pic: PicInfo::default(),
            frames: Default::default(),
            fwd: 0,
            bwd: 1,
            scratch: 2,
            cur: 1,
            refs: 0,
            second_field: false,
            in_field_pair: false,
            first_field_struct: 0,
            in_picture: false,
            skip_picture: false,
            allocated: false,
            tail: Vec::new(),
            out: VecDeque::new(),
            pred_y: [0; 256],
            pred_u: [0; 64],
            pred_v: [0; 64],
            pmv: Default::default(),
            mv: Default::default(),
            field_sel: Default::default(),
            dmv: [0; 2],
            dc_pred: [128; 3],
            last_flags: 0,
            q_code: 1,
            cur_tag: 0,
            gop_in_tag: -1,
        }
    }

    /// Vergisst Referenzbilder, z. B. nach einem Sprung im Film.
    pub fn reset(&mut self) {
        self.refs = 0;
        self.second_field = false;
        self.in_field_pair = false;
        self.first_field_struct = 0;
        self.in_picture = false;
        self.tail.clear();
        self.out.clear();
    }

    pub fn pop_frame(&mut self) -> Option<DecodedFrame> {
        self.out.pop_front()
    }

    pub fn queued(&self) -> usize {
        self.out.len()
    }

    /// Nimmt weitere Daten des Elementarstroms auf.
    /// Wie push, merkt sich aber für jedes beginnende Bild die Kennung 	ag.
    pub fn push_tagged(&mut self, es: &[u8], tag: u64) {
        if tag != self.cur_tag {
            self.cur_tag = tag;
            self.gop_in_tag = -1;
        }
        self.push(es);
    }

    pub fn push(&mut self, es: &[u8]) {
        let owned;
        let data: &[u8] = if self.tail.is_empty() {
            es
        } else {
            let mut t = std::mem::take(&mut self.tail);
            t.extend_from_slice(es);
            owned = t;
            &owned
        };
        let mut i = match next_start_code(data, 0) {
            Some(p) => p,
            None => return,
        };
        loop {
            let j = match next_start_code(data, i + 4) {
                Some(j) => j,
                None => {
                    self.tail.clear();
                    self.tail.extend_from_slice(&data[i..]);
                    return;
                }
            };
            self.unit(data[i + 3], &data[i + 4..j]);
            i = j;
        }
    }

    /// Gibt am Ende die zurückgehaltenen Bilder aus.
    pub fn flush(&mut self) {
        if !self.tail.is_empty() {
            let t = std::mem::take(&mut self.tail);
            if t.len() >= 4 {
                self.unit(t[3], &t[4..]);
            }
        }
        self.finish_picture();
        if self.refs > 0 {
            self.emit(self.bwd);
            self.refs = 0;
        }
    }

    fn unit(&mut self, code: u8, body: &[u8]) {
        match code {
            0xB3 => {
                self.finish_picture();
                self.seq.parse_seq_header(body);
            }
            0xB5 => match body.first().map(|b| b >> 4) {
                Some(1) => {
                    self.seq.parse_seq_ext(body);
                    self.ensure_alloc();
                }
                Some(3) => self.seq.parse_quant_ext(body),
                Some(8) => {
                    self.pic.parse_ext(body);
                    self.start_picture();
                }
                _ => {}
            },
            0xB7 => self.finish_picture(),
            0xB8 => {
                self.finish_picture();
                self.gop_in_tag += 1;
            }
            0x00 => {
                self.finish_picture();
                self.pic.parse_header(body);
            }
            0x01..=0xAF => {
                if self.in_picture && !self.skip_picture && self.allocated {
                    let _ = self.slice(code, body);
                }
            }
            _ => {}
        }
    }

    fn ensure_alloc(&mut self) {
        let stride = self.seq.mb_width() * 16;
        let rows = self.seq.mb_height() * 16;
        if !self.seq.mpeg2 || self.seq.chroma_format != 1 {
            self.allocated = false;
            return;
        }
        if self.allocated && self.frames[0].stride == stride && self.frames[0].rows == rows {
            return;
        }
        for f in &mut self.frames {
            f.alloc(stride, rows);
        }
        self.allocated = true;
        self.refs = 0;
    }

    fn start_picture(&mut self) {
        if !self.allocated {
            return;
        }
        let ct = self.pic.coding_type;
        if !(PIC_I..=PIC_B).contains(&ct) {
            self.skip_picture = true;
            return;
        }
        let field = self.pic.structure != STRUCT_FRAME;
        if field && self.second_field_pending() {
            self.second_field = true;
        } else {
            self.second_field = false;
            self.skip_picture = match ct {
                PIC_B => self.refs < 2,
                PIC_P => self.refs < 1,
                _ => false,
            };
            if self.skip_picture {
                self.in_picture = true;
                return;
            }
            if ct == PIC_B {
                self.cur = self.scratch;
            } else {
                if self.refs > 0 {
                    let (old_fwd, old_bwd) = (self.fwd, self.bwd);
                    self.emit(old_bwd);
                    self.fwd = old_bwd;
                    self.bwd = old_fwd;
                }
                self.cur = self.bwd;
                self.refs = (self.refs + 1).min(2);
            }
            let (interlaced, rff) = (
                !self.pic.progressive_frame && !self.seq.progressive,
                self.pic.repeat_first_field,
            );
            let f = &mut self.frames[self.cur];
            f.interlaced = interlaced;
            f.tag = self.cur_tag;
            f.gop = self.gop_in_tag.max(0) as u32;
            f.tr = self.pic.temporal_reference;
            f.rff = rff;
            self.first_field_struct = if field { self.pic.structure } else { 0 };
        }
        self.in_picture = true;
    }

    fn second_field_pending(&self) -> bool {
        self.in_field_pair
            && self.first_field_struct != 0
            && self.first_field_struct != self.pic.structure
    }

    fn finish_picture(&mut self) {
        if !self.in_picture {
            return;
        }
        self.in_picture = false;
        let field = self.pic.structure != STRUCT_FRAME;
        if field && !self.second_field {
            // Erstes Halbbild fertig, das zweite gehört zum selben Bild.
            self.in_field_pair = !self.skip_picture;
            if !self.skip_picture {
                return;
            }
        }
        self.in_field_pair = false;
        self.second_field = false;
        self.first_field_struct = 0;
        if self.skip_picture {
            self.skip_picture = false;
            return;
        }
        if self.pic.coding_type == PIC_B {
            self.emit(self.scratch);
        }
    }

    fn emit(&mut self, idx: usize) {
        let f = &self.frames[idx];
        let base = 1.0 / self.seq.fps();
        let duration = if f.rff && !self.seq.progressive { base * 1.5 } else { base };
        self.out.push_back(DecodedFrame {
            width: (self.seq.width as usize).min(f.stride),
            height: (self.seq.height as usize).min(f.rows),
            stride: f.stride,
            y: f.y.clone(),
            u: f.u.clone(),
            v: f.v.clone(),
            aspect: self.seq.display_aspect(),
            duration,
            interlaced: f.interlaced,
            tag: f.tag,
            gop: f.gop,
            tr: f.tr,
            frame_ms: 1000.0 / self.seq.fps(),
        });
    }

    fn reset_dc(&mut self) {
        let v = 128 << self.pic.intra_dc_precision;
        self.dc_pred = [v; 3];
    }

    fn slice(&mut self, code: u8, data: &[u8]) -> R<()> {
        let t = tables();
        let mut r = BitReader::new(data);
        let row = usize::from(code) - 1;
        let mbw = self.seq.mb_width();
        let field_pic = self.pic.structure != STRUCT_FRAME;
        let mbh = if field_pic { self.seq.mb_height() / 2 } else { self.seq.mb_height() };
        if row >= mbh {
            return Err(BadSlice);
        }
        self.q_code = r.get(5) as u8;
        if self.q_code == 0 {
            return Err(BadSlice);
        }
        while r.peek(1) == 1 {
            r.skip(9);
            if r.overrun() {
                return Err(BadSlice);
            }
        }
        r.skip(1);
        self.reset_dc();
        self.pmv = Default::default();

        let ct = self.pic.coding_type;
        let ct_idx = (ct - 1) as usize;
        let mut addr: isize = -1;
        let mut first = true;
        loop {
            // Adress-Inkrement
            let mut inc = 0usize;
            loop {
                let (v, len) = t.addr.lookup(r.peek(11));
                if len == 0 {
                    return Err(BadSlice);
                }
                r.skip(u32::from(len));
                if v == ADDR_ESCAPE {
                    inc += 33;
                } else {
                    inc += v as usize;
                    break;
                }
            }
            if first {
                addr = inc as isize - 1;
                first = false;
            } else {
                // Übersprungene Makroblöcke
                for _ in 1..inc {
                    addr += 1;
                    let x = addr as usize;
                    if x >= mbw {
                        return Err(BadSlice);
                    }
                    self.skipped_mb(x, row);
                }
                addr += 1;
            }
            let mb_x = addr as usize;
            if mb_x >= mbw {
                return Err(BadSlice);
            }

            let (flags, len) = t.mb_type[ct_idx][r.peek(6) as usize];
            if len == 0 {
                return Err(BadSlice);
            }
            r.skip(u32::from(len));
            let intra = flags & MB_INTRA != 0;
            let pattern = flags & MB_PATTERN != 0;

            let frame = !field_pic;
            let mut motion_type = if frame { 2u8 } else { 1u8 };
            if flags & (MB_FWD | MB_BWD) != 0 && (field_pic || !self.pic.frame_pred_frame_dct) {
                motion_type = r.get(2) as u8;
                if motion_type == 0 {
                    return Err(BadSlice);
                }
            }
            let mut dct_field = false;
            if frame && !self.pic.frame_pred_frame_dct && (intra || pattern) {
                dct_field = r.get(1) == 1;
            }
            if flags & MB_QUANT != 0 {
                self.q_code = r.get(5) as u8;
                if self.q_code == 0 {
                    return Err(BadSlice);
                }
            }

            if flags & MB_FWD != 0 || (intra && self.pic.concealment_mv) {
                self.read_motion(&mut r, 0, motion_type)?;
            }
            if flags & MB_BWD != 0 {
                self.read_motion(&mut r, 1, motion_type)?;
            }
            if intra && self.pic.concealment_mv {
                r.skip(1);
            }

            let mut cbp = 0u8;
            if intra {
                cbp = 0x3F;
            } else if pattern {
                let (c, len) = t.cbp.lookup(r.peek(9));
                if len == 0 {
                    return Err(BadSlice);
                }
                r.skip(u32::from(len));
                cbp = c as u8;
            }

            if intra {
                if !self.pic.concealment_mv {
                    self.pmv = Default::default();
                }
            } else {
                self.reset_dc();
                if ct == PIC_P && flags & MB_FWD == 0 {
                    self.pmv = Default::default();
                }
                self.predict(mb_x, row, flags, motion_type);
                self.last_flags = flags;
            }

            for b in 0..6usize {
                if cbp & (1 << (5 - b)) != 0 {
                    let mut coef = [0i32; 64];
                    self.read_block(&mut r, b, intra, &mut coef)?;
                    idct(&mut coef);
                    self.place_block(b, &coef, intra, dct_field);
                } else if intra {
                    return Err(BadSlice);
                }
            }
            self.store_mb(mb_x, row);

            if r.overrun() {
                return Err(BadSlice);
            }
            if r.at_slice_end() {
                break;
            }
        }
        Ok(())
    }

    fn skipped_mb(&mut self, x: usize, row: usize) {
        self.reset_dc();
        let ct = self.pic.coding_type;
        if ct == PIC_P {
            self.pmv = Default::default();
            self.mv = Default::default();
            let frame = self.pic.structure == STRUCT_FRAME;
            let parity = u8::from(self.pic.structure == STRUCT_BOTTOM);
            self.field_sel = [[parity; 2]; 2];
            self.predict(x, row, MB_FWD, if frame { 2 } else { 1 });
        } else {
            // Regel für übersprungene Blöcke in B-Bildern: Richtung wie zuvor, aber immer
            // ein einzelner Vektor aus den Prädiktoren und gleiche Parität.
            let frame = self.pic.structure == STRUCT_FRAME;
            let parity = u8::from(self.pic.structure == STRUCT_BOTTOM);
            for s in 0..2 {
                self.mv[0][s] = self.pmv[0][s];
                self.field_sel[0][s] = parity;
            }
            let flags = self.last_flags;
            self.predict(x, row, flags & (MB_FWD | MB_BWD), if frame { 2 } else { 1 });
        }
        self.store_mb(x, row);
    }

    fn read_mv_comp(r: &mut BitReader, f_code: u8, pred: i32) -> R<i32> {
        let t = tables();
        let (code, len) = t.motion.lookup(r.peek(11));
        if len == 0 {
            return Err(BadSlice);
        }
        r.skip(u32::from(len));
        let r_size = u32::from(f_code.max(1)) - 1;
        let f = 1i32 << r_size;
        let code = i32::from(code);
        let delta = if f == 1 || code == 0 {
            code
        } else {
            let resid = r.get(r_size) as i32;
            let abs = (code.abs() - 1) * f + resid + 1;
            if code < 0 {
                -abs
            } else {
                abs
            }
        };
        let (low, high, range) = (-16 * f, 16 * f - 1, 32 * f);
        let mut v = pred + delta;
        if v < low {
            v += range;
        } else if v > high {
            v -= range;
        }
        Ok(v)
    }

    fn read_dmv(r: &mut BitReader) -> i32 {
        if r.get(1) == 0 {
            0
        } else if r.get(1) == 0 {
            1
        } else {
            -1
        }
    }

    fn read_motion(&mut self, r: &mut BitReader, s: usize, motion_type: u8) -> R<()> {
        let fc = self.pic.f_code[s];
        let frame = self.pic.structure == STRUCT_FRAME;
        match (frame, motion_type) {
            (true, 2) => {
                let h = Self::read_mv_comp(r, fc[0], self.pmv[0][s][0])?;
                let v = Self::read_mv_comp(r, fc[1], self.pmv[0][s][1])?;
                self.mv[0][s] = [h, v];
                self.pmv[0][s] = [h, v];
                self.pmv[1][s] = [h, v];
            }
            (true, 1) => {
                for n in 0..2 {
                    self.field_sel[n][s] = r.get(1) as u8;
                    let h = Self::read_mv_comp(r, fc[0], self.pmv[n][s][0])?;
                    let v = Self::read_mv_comp(r, fc[1], self.pmv[n][s][1] >> 1)?;
                    self.mv[n][s] = [h, v];
                    self.pmv[n][s] = [h, v * 2];
                }
            }
            (true, 3) => {
                let h = Self::read_mv_comp(r, fc[0], self.pmv[0][s][0])?;
                self.dmv[0] = Self::read_dmv(r);
                let v = Self::read_mv_comp(r, fc[1], self.pmv[0][s][1] >> 1)?;
                self.dmv[1] = Self::read_dmv(r);
                self.mv[0][s] = [h, v];
                self.pmv[0][s] = [h, v * 2];
                self.pmv[1][s] = [h, v * 2];
            }
            (false, 1) => {
                self.field_sel[0][s] = r.get(1) as u8;
                let h = Self::read_mv_comp(r, fc[0], self.pmv[0][s][0])?;
                let v = Self::read_mv_comp(r, fc[1], self.pmv[0][s][1])?;
                self.mv[0][s] = [h, v];
                self.pmv[0][s] = [h, v];
                self.pmv[1][s] = [h, v];
            }
            (false, 2) => {
                for n in 0..2 {
                    self.field_sel[n][s] = r.get(1) as u8;
                    let h = Self::read_mv_comp(r, fc[0], self.pmv[n][s][0])?;
                    let v = Self::read_mv_comp(r, fc[1], self.pmv[n][s][1])?;
                    self.mv[n][s] = [h, v];
                    self.pmv[n][s] = [h, v];
                }
            }
            (false, 3) => {
                let h = Self::read_mv_comp(r, fc[0], self.pmv[0][s][0])?;
                self.dmv[0] = Self::read_dmv(r);
                let v = Self::read_mv_comp(r, fc[1], self.pmv[0][s][1])?;
                self.dmv[1] = Self::read_dmv(r);
                self.mv[0][s] = [h, v];
                self.pmv[0][s] = [h, v];
                self.pmv[1][s] = [h, v];
            }
            _ => return Err(BadSlice),
        }
        Ok(())
    }

    /// Eine Vorhersage aus einem Referenzbild in den Zwischenspeicher.
    /// `parity`: None = ganzes Bild, Some(p) = ein Halbbild daraus.
    #[allow(clippy::too_many_arguments)]
    fn mc(
        &mut self,
        ref_idx: usize,
        parity: Option<u8>,
        x0: usize,
        y0: usize,
        h: usize,
        dst_line: usize,
        dst_step: usize,
        mv: [i32; 2],
        avg: bool,
    ) {
        let f = &self.frames[ref_idx];
        let (mul, off) = match parity {
            Some(p) => (2usize, usize::from(p)),
            None => (1usize, 0usize),
        };
        let stride = f.stride;
        let rows = f.rows / mul;
        pred_plane(
            &f.y[off * stride..],
            stride * mul,
            stride,
            rows,
            x0 as i32 * 2 + mv[0],
            y0 as i32 * 2 + mv[1],
            &mut self.pred_y,
            dst_line * 16,
            16 * dst_step,
            16,
            h,
            avg,
        );
        let cs = stride / 2;
        let crows = rows / 2;
        let cmv = [mv[0] / 2, mv[1] / 2];
        let cdst = if dst_step == 2 { dst_line * 8 } else { dst_line / 2 * 8 };
        for plane in 0..2 {
            let (src, dst) = if plane == 0 {
                (&f.u, &mut self.pred_u)
            } else {
                (&f.v, &mut self.pred_v)
            };
            pred_plane(
                &src[off * cs..],
                cs * mul,
                cs,
                crows,
                (x0 / 2) as i32 * 2 + cmv[0],
                (y0 / 2) as i32 * 2 + cmv[1],
                dst,
                cdst,
                8 * dst_step,
                8,
                h / 2,
                avg,
            );
        }
    }

    fn predict(&mut self, x: usize, row: usize, flags: u8, motion_type: u8) {
        let px = x * 16;
        let mut did = false;
        if flags & MB_FWD != 0 {
            self.predict_dir(px, row, 0, motion_type, false);
            did = true;
        } else if self.pic.coding_type == PIC_P {
            // Ohne Bewegung: Nullvektor auf die eigene Parität
            self.mv = Default::default();
            let parity = u8::from(self.pic.structure == STRUCT_BOTTOM);
            self.field_sel = [[parity; 2]; 2];
            let m = if self.pic.structure == STRUCT_FRAME { 2 } else { 1 };
            self.predict_dir(px, row, 0, m, false);
            did = true;
        }
        if flags & MB_BWD != 0 {
            self.predict_dir(px, row, 1, motion_type, did);
        }
    }

    /// Dual-Prime: abgeleiteter Vektor für das Halbbild anderer Parität.
    fn dmv_vector(mv: [i32; 2], dmv: [i32; 2], m: i32, e: i32) -> [i32; 2] {
        let h = ((mv[0] * m + i32::from(mv[0] > 0)) >> 1) + dmv[0];
        let v = ((mv[1] * m + i32::from(mv[1] > 0)) >> 1) + dmv[1] + e;
        [h, v]
    }

    fn predict_dir(&mut self, px: usize, row: usize, s: usize, motion_type: u8, avg: bool) {
        let frame = self.pic.structure == STRUCT_FRAME;
        let reference = if s == 0 { self.fwd } else { self.bwd };
        if frame {
            let y = row * 16;
            match motion_type {
                2 => self.mc(reference, None, px, y, 16, 0, 1, self.mv[0][s], avg),
                1 => {
                    for f in 0..2usize {
                        let sel = self.field_sel[f][s];
                        self.mc(reference, Some(sel), px, row * 8, 8, f, 2, self.mv[f][s], avg);
                    }
                }
                3 => {
                    let base = self.mv[0][s];
                    let m_top = if self.pic.top_field_first { 1 } else { 3 };
                    let cross_t = Self::dmv_vector(base, self.dmv, m_top, -1);
                    let cross_b = Self::dmv_vector(base, self.dmv, 4 - m_top, 1);
                    self.mc(reference, Some(0), px, row * 8, 8, 0, 2, base, avg);
                    self.mc(reference, Some(1), px, row * 8, 8, 0, 2, cross_t, true);
                    self.mc(reference, Some(1), px, row * 8, 8, 1, 2, base, avg);
                    self.mc(reference, Some(0), px, row * 8, 8, 1, 2, cross_b, true);
                }
                _ => {}
            }
        } else {
            let parity = u8::from(self.pic.structure == STRUCT_BOTTOM);
            let y = row * 16;
            // Das zweite Halbbild eines P-Bildes darf das erste desselben Bildes nutzen.
            let opposite_ref = |sel: u8, this: &Self| {
                if this.second_field && this.pic.coding_type == PIC_P && s == 0 && sel != parity {
                    this.cur
                } else {
                    reference
                }
            };
            match motion_type {
                1 => {
                    let sel = self.field_sel[0][s];
                    let r = opposite_ref(sel, self);
                    self.mc(r, Some(sel), px, y, 16, 0, 1, self.mv[0][s], avg);
                }
                2 => {
                    for n in 0..2usize {
                        let sel = self.field_sel[n][s];
                        let r = opposite_ref(sel, self);
                        self.mc(r, Some(sel), px, y + n * 8, 8, n * 8, 1, self.mv[n][s], avg);
                    }
                }
                3 => {
                    let base = self.mv[0][s];
                    let e = if parity == 0 { -1 } else { 1 };
                    let cross = Self::dmv_vector(base, self.dmv, 1, e);
                    self.mc(reference, Some(parity), px, y, 16, 0, 1, base, avg);
                    let other = 1 - parity;
                    let r = opposite_ref(other, self);
                    self.mc(r, Some(other), px, y, 16, 0, 1, cross, true);
                }
                _ => {}
            }
        }
    }

    fn read_block(&mut self, r: &mut BitReader, b: usize, intra: bool, coef: &mut [i32; 64]) -> R<()> {
        let t = tables();
        let scan: &[u8; 64] = if self.pic.alternate_scan { &ALT_SCAN } else { &ZIGZAG };
        let qs = qscale(self.q_code, self.pic.q_scale_type) as i32;
        let table = usize::from(intra && self.pic.intra_vlc_format);
        let dct = &t.dct[table];
        let matrix = if intra { &self.seq.intra_matrix } else { &self.seq.nonintra_matrix };
        let mut idx: i32 = -1;
        let mut sum = 0i32;
        if intra {
            let (size, len) = if b < 4 {
                t.dc_luma.lookup(r.peek(9))
            } else {
                t.dc_chroma.lookup(r.peek(10))
            };
            if len == 0 {
                return Err(BadSlice);
            }
            r.skip(u32::from(len));
            let cc = if b < 4 { 0 } else { b - 3 };
            let mut diff = 0i32;
            if size > 0 {
                let bits = r.get(size as u32) as i32;
                let half = 1 << (size - 1);
                diff = if bits >= half { bits } else { bits - (1 << size) + 1 };
            }
            self.dc_pred[cc] = (self.dc_pred[cc] + diff).clamp(-4096, 4095);
            let dc = (self.dc_pred[cc] << (3 - self.pic.intra_dc_precision)).clamp(-2048, 2047);
            coef[0] = dc;
            sum = dc;
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
                    return Err(BadSlice);
                }
                r.skip(u32::from(e.len));
                if e.run == TOK_EOB {
                    break;
                }
                if e.run == TOK_ESC {
                    run = r.get(6) as i32;
                    level = r.get_signed(12);
                } else {
                    run = i32::from(e.run);
                    level = if r.get(1) == 1 { -i32::from(e.level) } else { i32::from(e.level) };
                }
            }
            first = false;
            idx += run + 1;
            if idx > 63 {
                return Err(BadSlice);
            }
            let pos = scan[idx as usize] as usize;
            let w = i32::from(matrix[pos]);
            let val = if intra {
                (2 * level * w * qs) / 32
            } else {
                ((2 * level + level.signum()) * w * qs) / 32
            };
            let val = val.clamp(-2048, 2047);
            coef[pos] = val;
            sum += val;
        }
        if sum & 1 == 0 {
            coef[63] ^= 1;
        }
        Ok(())
    }

    fn place_block(&mut self, b: usize, res: &[i32; 64], intra: bool, dct_field: bool) {
        let (plane, stride, x0, y0, step): (&mut [u8], usize, usize, usize, usize) = if b < 4 {
            let x0 = (b & 1) * 8;
            let (y0, step) = if dct_field { (b >> 1, 2) } else { ((b >> 1) * 8, 1) };
            (&mut self.pred_y, 16, x0, y0, step)
        } else if b == 4 {
            (&mut self.pred_u, 8, 0, 0, 1)
        } else {
            (&mut self.pred_v, 8, 0, 0, 1)
        };
        for row in 0..8 {
            let line = &mut plane[(y0 + row * step) * stride + x0..];
            for col in 0..8 {
                let r = res[row * 8 + col];
                let v = if intra { r } else { i32::from(line[col]) + r };
                line[col] = v.clamp(0, 255) as u8;
            }
        }
    }

    fn store_mb(&mut self, x: usize, row: usize) {
        let field = self.pic.structure != STRUCT_FRAME;
        let parity = usize::from(self.pic.structure == STRUCT_BOTTOM);
        let f = &mut self.frames[self.cur];
        let stride = f.stride;
        let cs = stride / 2;
        for r in 0..16 {
            let frow = if field { (row * 16 + r) * 2 + parity } else { row * 16 + r };
            let at = frow * stride + x * 16;
            if at + 16 > f.y.len() {
                return;
            }
            f.y[at..at + 16].copy_from_slice(&self.pred_y[r * 16..r * 16 + 16]);
        }
        for r in 0..8 {
            let frow = if field { (row * 8 + r) * 2 + parity } else { row * 8 + r };
            let at = frow * cs + x * 8;
            if at + 8 > f.u.len() {
                return;
            }
            f.u[at..at + 8].copy_from_slice(&self.pred_u[r * 8..r * 8 + 8]);
            f.v[at..at + 8].copy_from_slice(&self.pred_v[r * 8..r * 8 + 8]);
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pred_plane(
    src: &[u8],
    src_stride: usize,
    plane_w: usize,
    plane_rows: usize,
    pos_x: i32,
    pos_y: i32,
    dst: &mut [u8],
    dst_start: usize,
    dst_stride: usize,
    w: usize,
    h: usize,
    avg: bool,
) {
    let hx = (pos_x & 1) as usize;
    let hy = (pos_y & 1) as usize;
    let max_x = plane_w as i32 - w as i32 - hx as i32;
    let max_y = plane_rows as i32 - h as i32 - hy as i32;
    let (ox, oy) = (pos_x >> 1, pos_y >> 1);
    if ox < 0 || oy < 0 || ox > max_x || oy > max_y {
        // Vektor zeigt über den Bildrand: Randpixel wiederholen.
        let sample = |x: i32, y: i32| -> u32 {
            let xx = x.clamp(0, plane_w as i32 - 1) as usize;
            let yy = y.clamp(0, plane_rows as i32 - 1) as usize;
            u32::from(*src.get(yy * src_stride + xx).unwrap_or(&128))
        };
        for row in 0..h as i32 {
            for col in 0..w as i32 {
                let (x, y) = (ox + col, oy + row);
                let a = sample(x, y);
                let p = match (hx, hy) {
                    (0, 0) => a,
                    (1, 0) => (a + sample(x + 1, y) + 1) >> 1,
                    (0, _) => (a + sample(x, y + 1) + 1) >> 1,
                    _ => (a + sample(x + 1, y) + sample(x, y + 1) + sample(x + 1, y + 1) + 2) >> 2,
                };
                let d = &mut dst[dst_start + row as usize * dst_stride + col as usize];
                *d = if avg { ((u32::from(*d) + p + 1) >> 1) as u8 } else { p as u8 };
            }
        }
        return;
    }
    let (ix, iy) = (ox as usize, oy as usize);
    for row in 0..h {
        let d = &mut dst[dst_start + row * dst_stride..dst_start + row * dst_stride + w];
        let at = (iy + row) * src_stride + ix;
        if at + w + hx > src.len() {
            return;
        }
        let a = &src[at..at + w + hx];
        match (hx, hy) {
            (0, 0) => {
                if avg {
                    for x in 0..w {
                        d[x] = ((u32::from(d[x]) + u32::from(a[x]) + 1) >> 1) as u8;
                    }
                } else {
                    d.copy_from_slice(&a[..w]);
                }
            }
            (1, 0) => {
                for x in 0..w {
                    let p = (u32::from(a[x]) + u32::from(a[x + 1]) + 1) >> 1;
                    d[x] = if avg { ((u32::from(d[x]) + p + 1) >> 1) as u8 } else { p as u8 };
                }
            }
            _ => {
                let at2 = at + src_stride;
                if at2 + w + hx > src.len() {
                    return;
                }
                let b = &src[at2..at2 + w + hx];
                if hx == 0 {
                    for x in 0..w {
                        let p = (u32::from(a[x]) + u32::from(b[x]) + 1) >> 1;
                        d[x] = if avg { ((u32::from(d[x]) + p + 1) >> 1) as u8 } else { p as u8 };
                    }
                } else {
                    for x in 0..w {
                        let p = (u32::from(a[x]) + u32::from(a[x + 1]) + u32::from(b[x]) + u32::from(b[x + 1]) + 2) >> 2;
                        d[x] = if avg { ((u32::from(d[x]) + p + 1) >> 1) as u8 } else { p as u8 };
                    }
                }
            }
        }
    }
}

/// Rechnet für dekodierte Bilder die Anzeigezeit relativ zum Beginn ihres Datenstücks aus.
/// Grundlage sind Bildgruppe und Anzeigeposition; fehlende führende Bilder (nach einem
/// Sprung) verschieben die Zeit deshalb nicht.
#[derive(Default)]
pub struct FrameClock {
    tag: Option<u64>,
    gop: u32,
    gop_base: f64,
    gop_max_tr: i32,
}

impl FrameClock {
    pub fn offset_ms(&mut self, f: &DecodedFrame) -> f64 {
        if self.tag != Some(f.tag) {
            self.tag = Some(f.tag);
            self.gop = f.gop;
            self.gop_base = 0.0;
            self.gop_max_tr = -1;
        } else if f.gop != self.gop {
            self.gop_base += f64::from(self.gop_max_tr + 1) * f.frame_ms;
            self.gop = f.gop;
            self.gop_max_tr = -1;
        }
        self.gop_max_tr = self.gop_max_tr.max(i32::from(f.tr));
        self.gop_base + f64::from(f.tr) * f.frame_ms
    }
}
const W1: i32 = 2841;
const W2: i32 = 2676;
const W3: i32 = 2408;
const W5: i32 = 1609;
const W6: i32 = 1108;
const W7: i32 = 565;

/// Inverse 8x8-DCT (Festkomma, nach dem Referenz-Verfahren aus Zeilen und Spalten).
pub fn idct(b: &mut [i32; 64]) {
    for i in 0..8 {
        idct_row(&mut b[i * 8..i * 8 + 8]);
    }
    for i in 0..8 {
        idct_col(b, i);
    }
}

#[inline]
fn idct_row(blk: &mut [i32]) {
    let mut x1 = blk[4] << 11;
    let mut x2 = blk[6];
    let mut x3 = blk[2];
    let mut x4 = blk[1];
    let mut x5 = blk[7];
    let mut x6 = blk[5];
    let mut x7 = blk[3];
    if x1 | x2 | x3 | x4 | x5 | x6 | x7 == 0 {
        let v = blk[0] << 3;
        blk[..8].fill(v);
        return;
    }
    let mut x0 = (blk[0] << 11) + 128;
    let mut x8 = W7 * (x4 + x5);
    x4 = x8 + (W1 - W7) * x4;
    x5 = x8 - (W1 + W7) * x5;
    x8 = W3 * (x6 + x7);
    x6 = x8 - (W3 - W5) * x6;
    x7 = x8 - (W3 + W5) * x7;
    x8 = x0 + x1;
    x0 -= x1;
    x1 = W6 * (x3 + x2);
    x2 = x1 - (W2 + W6) * x2;
    x3 = x1 + (W2 - W6) * x3;
    x1 = x4 + x6;
    x4 -= x6;
    x6 = x5 + x7;
    x5 -= x7;
    x7 = x8 + x3;
    x8 -= x3;
    x3 = x0 + x2;
    x0 -= x2;
    x2 = ((181i64 * i64::from(x4 + x5) + 128) >> 8) as i32;
    x4 = ((181i64 * i64::from(x4 - x5) + 128) >> 8) as i32;
    // Begrenzen schützt nur vor beschädigten Daten, gültige Werte bleiben darin.
    let lim = |v: i32| (v >> 8).clamp(-32768, 32767);
    blk[0] = lim(x7 + x1);
    blk[1] = lim(x3 + x2);
    blk[2] = lim(x0 + x4);
    blk[3] = lim(x8 + x6);
    blk[4] = lim(x8 - x6);
    blk[5] = lim(x0 - x4);
    blk[6] = lim(x3 - x2);
    blk[7] = lim(x7 - x1);
}

#[inline]
fn idct_col(b: &mut [i32; 64], c: usize) {
    let mut x1 = b[c + 8 * 4] << 8;
    let mut x2 = b[c + 8 * 6];
    let mut x3 = b[c + 8 * 2];
    let mut x4 = b[c + 8];
    let mut x5 = b[c + 8 * 7];
    let mut x6 = b[c + 8 * 5];
    let mut x7 = b[c + 8 * 3];
    if x1 | x2 | x3 | x4 | x5 | x6 | x7 == 0 {
        let v = (b[c] + 32) >> 6;
        for i in 0..8 {
            b[c + 8 * i] = v;
        }
        return;
    }
    let mut x0 = (b[c] << 8) + 8192;
    let mut x8 = W7 * (x4 + x5) + 4;
    x4 = (x8 + (W1 - W7) * x4) >> 3;
    x5 = (x8 - (W1 + W7) * x5) >> 3;
    x8 = W3 * (x6 + x7) + 4;
    x6 = (x8 - (W3 - W5) * x6) >> 3;
    x7 = (x8 - (W3 + W5) * x7) >> 3;
    x8 = x0 + x1;
    x0 -= x1;
    x1 = W6 * (x3 + x2) + 4;
    x2 = (x1 - (W2 + W6) * x2) >> 3;
    x3 = (x1 + (W2 - W6) * x3) >> 3;
    x1 = x4 + x6;
    x4 -= x6;
    x6 = x5 + x7;
    x5 -= x7;
    x7 = x8 + x3;
    x8 -= x3;
    x3 = x0 + x2;
    x0 -= x2;
    x2 = ((181i64 * i64::from(x4 + x5) + 128) >> 8) as i32;
    x4 = ((181i64 * i64::from(x4 - x5) + 128) >> 8) as i32;
    b[c] = (x7 + x1) >> 14;
    b[c + 8] = (x3 + x2) >> 14;
    b[c + 8 * 2] = (x0 + x4) >> 14;
    b[c + 8 * 3] = (x8 + x6) >> 14;
    b[c + 8 * 4] = (x8 - x6) >> 14;
    b[c + 8 * 5] = (x0 - x4) >> 14;
    b[c + 8 * 6] = (x3 - x2) >> 14;
    b[c + 8 * 7] = (x7 - x1) >> 14;
}
