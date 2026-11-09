//! Bitweises Lesen und Schreiben im MSB-first-Format von MPEG.

#[derive(Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> BitReader<'a> {
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    #[inline]
    pub fn pos(&self) -> usize {
        self.pos
    }

    #[inline]
    pub fn bits_total(&self) -> usize {
        self.data.len() * 8
    }

    /// Wahr, wenn über das Datenende hinaus gelesen wurde.
    #[inline]
    pub fn overrun(&self) -> bool {
        self.pos > self.bits_total()
    }

    /// Bis zu 32 Bits ansehen, ohne weiterzugehen. Hinter dem Ende stehen Nullen.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        debug_assert!(n > 0 && n <= 32);
        let byte = self.pos >> 3;
        let v = if byte + 8 <= self.data.len() {
            let mut a = [0u8; 8];
            a.copy_from_slice(&self.data[byte..byte + 8]);
            u64::from_be_bytes(a)
        } else {
            let mut v = 0u64;
            for i in 0..8 {
                v = (v << 8) | u64::from(*self.data.get(byte + i).unwrap_or(&0));
            }
            v
        };
        ((v << (self.pos & 7)) >> (64 - n)) as u32
    }

    #[inline]
    pub fn skip(&mut self, n: u32) {
        self.pos += n as usize;
    }

    #[inline]
    pub fn get(&mut self, n: u32) -> u32 {
        let v = self.peek(n);
        self.pos += n as usize;
        v
    }

    #[inline]
    pub fn get_signed(&mut self, n: u32) -> i32 {
        let v = self.get(n) as i32;
        let shift = 32 - n;
        (v << shift) >> shift
    }

    /// Stehen in den nächsten 23 Bits nur Nullen, folgt ein Startcode oder das Ende.
    #[inline]
    pub fn at_slice_end(&self) -> bool {
        self.peek(23) == 0
    }
}

#[derive(Default)]
pub struct BitWriter {
    pub out: Vec<u8>,
    acc: u64,
    n: u32,
}

impl BitWriter {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn put(&mut self, val: u32, bits: u32) {
        if bits == 0 {
            return;
        }
        let mask = if bits >= 32 { u32::MAX } else { (1u32 << bits) - 1 };
        self.acc = (self.acc << bits) | u64::from(val & mask);
        self.n += bits;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.acc >> self.n) as u8);
        }
    }

    /// Kopiert den Bitbereich `[from, to)` aus `src`.
    pub fn copy_bits(&mut self, src: &[u8], from: usize, to: usize) {
        let mut r = BitReader::new(src);
        r.skip(from as u32);
        let mut left = to.saturating_sub(from);
        while left > 0 {
            let chunk = left.min(24) as u32;
            let v = r.get(chunk);
            self.put(v, chunk);
            left -= chunk as usize;
        }
    }

    /// Füllt mit Nullbits bis zur nächsten Bytegrenze auf.
    pub fn align(&mut self) {
        let rem = self.n % 8;
        if rem != 0 {
            self.put(0, 8 - rem);
        }
    }

    pub fn start_code(&mut self, code: u8) {
        self.align();
        self.out.extend_from_slice(&[0, 0, 1, code]);
    }

    pub fn bytes(&mut self, b: &[u8]) {
        self.align();
        self.out.extend_from_slice(b);
    }

    pub fn len_bytes(&self) -> usize {
        self.out.len()
    }
}

/// Sucht ab `from` den nächsten Startcode (`00 00 01`) und liefert dessen Position.
pub fn next_start_code(data: &[u8], from: usize) -> Option<usize> {
    let mut i = from;
    while i + 3 <= data.len() {
        // Schneller Sprung: Das dritte Byte entscheidet, wie weit es weitergehen darf.
        match data[i + 2] {
            0 => i += 1,
            1 if data[i] == 0 && data[i + 1] == 0 => return Some(i),
            _ => i += 3,
        }
    }
    None
}
