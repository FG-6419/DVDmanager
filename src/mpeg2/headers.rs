//! Sequenz- und Bildköpfe, von Verkleinerer und Decoder gemeinsam genutzt.

use super::bits::BitReader;
use super::tables::{DEFAULT_INTRA, ZIGZAG};

#[derive(Clone)]
pub struct SeqInfo {
    pub width: u32,
    pub height: u32,
    pub aspect: u8,
    pub frame_rate_code: u8,
    pub mpeg2: bool,
    pub progressive: bool,
    pub chroma_format: u8,
    pub intra_matrix: [u8; 64],
    pub nonintra_matrix: [u8; 64],
}

impl Default for SeqInfo {
    fn default() -> Self {
        Self {
            width: 0,
            height: 0,
            aspect: 2,
            frame_rate_code: 3,
            mpeg2: false,
            progressive: false,
            chroma_format: 1,
            intra_matrix: DEFAULT_INTRA,
            nonintra_matrix: [16; 64],
        }
    }
}

impl SeqInfo {
    /// Liest einen Sequenzkopf; `data` beginnt direkt hinter dem Startcode.
    pub fn parse_seq_header(&mut self, data: &[u8]) {
        let mut r = BitReader::new(data);
        self.width = r.get(12);
        self.height = r.get(12);
        self.aspect = r.get(4) as u8;
        self.frame_rate_code = r.get(4) as u8;
        r.skip(18 + 1 + 10 + 1); // Bitrate, Marker, VBV, constrained
        if r.get(1) == 1 {
            for i in 0..64 {
                self.intra_matrix[ZIGZAG[i] as usize] = r.get(8) as u8;
            }
        } else {
            self.intra_matrix = DEFAULT_INTRA;
        }
        if r.get(1) == 1 {
            for i in 0..64 {
                self.nonintra_matrix[ZIGZAG[i] as usize] = r.get(8) as u8;
            }
        } else {
            self.nonintra_matrix = [16; 64];
        }
        self.mpeg2 = false;
    }

    pub fn parse_seq_ext(&mut self, data: &[u8]) {
        let mut r = BitReader::new(data);
        r.skip(4 + 8);
        self.progressive = r.get(1) == 1;
        self.chroma_format = r.get(2) as u8;
        self.width |= r.get(2) << 12;
        self.height |= r.get(2) << 12;
        self.mpeg2 = true;
    }

    pub fn parse_quant_ext(&mut self, data: &[u8]) {
        let mut r = BitReader::new(data);
        r.skip(4);
        if r.get(1) == 1 {
            for i in 0..64 {
                self.intra_matrix[ZIGZAG[i] as usize] = r.get(8) as u8;
            }
        }
        if r.get(1) == 1 {
            for i in 0..64 {
                self.nonintra_matrix[ZIGZAG[i] as usize] = r.get(8) as u8;
            }
        }
    }

    pub fn fps(&self) -> f64 {
        match self.frame_rate_code {
            1 => 24000.0 / 1001.0,
            2 => 24.0,
            3 => 25.0,
            4 => 30000.0 / 1001.0,
            5 => 30.0,
            6 => 50.0,
            7 => 60000.0 / 1001.0,
            8 => 60.0,
            _ => 25.0,
        }
    }

    pub fn display_aspect(&self) -> f32 {
        match self.aspect {
            3 => 16.0 / 9.0,
            4 => 2.21,
            2 => 4.0 / 3.0,
            _ => {
                if self.height > 0 {
                    self.width as f32 / self.height as f32
                } else {
                    4.0 / 3.0
                }
            }
        }
    }

    pub fn mb_width(&self) -> usize {
        ((self.width as usize + 15) / 16).max(1)
    }

    pub fn mb_height(&self) -> usize {
        if self.progressive {
            ((self.height as usize + 15) / 16).max(1)
        } else {
            2 * ((self.height as usize + 31) / 32).max(1)
        }
    }
}

pub const PIC_I: u8 = 1;
pub const PIC_P: u8 = 2;
pub const PIC_B: u8 = 3;
pub const STRUCT_TOP: u8 = 1;
pub const STRUCT_BOTTOM: u8 = 2;
pub const STRUCT_FRAME: u8 = 3;

#[derive(Clone, Default)]
pub struct PicInfo {
    pub temporal_reference: u16,
    pub coding_type: u8,
    pub f_code: [[u8; 2]; 2],
    pub intra_dc_precision: u8,
    pub structure: u8,
    pub top_field_first: bool,
    pub frame_pred_frame_dct: bool,
    pub concealment_mv: bool,
    pub q_scale_type: bool,
    pub intra_vlc_format: bool,
    pub alternate_scan: bool,
    pub repeat_first_field: bool,
    pub progressive_frame: bool,
    pub has_ext: bool,
}

impl PicInfo {
    pub fn parse_header(&mut self, data: &[u8]) {
        let mut r = BitReader::new(data);
        self.temporal_reference = r.get(10) as u16;
        self.coding_type = r.get(3) as u8;
        self.has_ext = false;
    }

    pub fn parse_ext(&mut self, data: &[u8]) {
        let mut r = BitReader::new(data);
        r.skip(4);
        self.f_code[0][0] = r.get(4) as u8;
        self.f_code[0][1] = r.get(4) as u8;
        self.f_code[1][0] = r.get(4) as u8;
        self.f_code[1][1] = r.get(4) as u8;
        self.intra_dc_precision = r.get(2) as u8;
        self.structure = r.get(2) as u8;
        self.top_field_first = r.get(1) == 1;
        self.frame_pred_frame_dct = r.get(1) == 1;
        self.concealment_mv = r.get(1) == 1;
        self.q_scale_type = r.get(1) == 1;
        self.intra_vlc_format = r.get(1) == 1;
        self.alternate_scan = r.get(1) == 1;
        self.repeat_first_field = r.get(1) == 1;
        r.skip(1);
        self.progressive_frame = r.get(1) == 1;
        self.has_ext = true;
    }
}
