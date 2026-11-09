//! Ton für die Vorschau: Spur aus den VOB-Paketen lösen, dekodieren (LPCM selbst,
//! Dolby Digital und MPEG-Ton über die in Windows enthaltenen Decoder) und über die
//! Windows-Tonausgabe abspielen. Die Ausgabe liefert zugleich die Uhr, nach der die
//! Vorschau ihre Bilder zeigt; so bleiben Bild und Ton beisammen.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use crate::dvd::ifo::AudioAttr;
use crate::dvd::vob::{pack_header_len, read_ts};

/// Ausgabeformat: 48 kHz, Stereo, 16 Bit.
pub const RATE: f64 = 48_000.0;

#[derive(Default)]
struct BusState {
    seek_id: u64,
    chunks: VecDeque<(f64, Vec<i16>)>,
    playing: bool,
    /// Neu anfangen ab dieser Zeit (wird vom Abspiel-Thread übernommen)
    restart_at: Option<f64>,
    muted: bool,
    clock: Option<f64>,
    quit: bool,
    device_ok: bool,
}

/// Verbindung zwischen Vorschau (liefert Ton), Oberfläche (steuert) und Ausgabe.
#[derive(Clone, Default)]
pub struct AudioBus {
    inner: Arc<Mutex<BusState>>,
}

impl AudioBus {
    /// Legt den Bus an und startet die Ausgabe in einem eigenen Thread.
    pub fn start() -> Self {
        let bus = Self::default();
        let b2 = bus.clone();
        std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| player::run(b2)));
        });
        bus
    }

    fn with<R>(&self, f: impl FnOnce(&mut BusState) -> R) -> R {
        match self.inner.lock() {
            Ok(mut g) => f(&mut g),
            Err(p) => f(&mut p.into_inner()),
        }
    }

    /// Neuer Abschnitt nach einem Sprung: alter Ton entfällt.
    pub fn reset(&self, seek_id: u64) {
        self.with(|s| {
            s.seek_id = seek_id;
            s.chunks.clear();
        });
    }

    pub fn push(&self, seek_id: u64, start_ms: f64, samples: Vec<i16>) {
        self.with(|s| {
            if s.seek_id == seek_id && !samples.is_empty() {
                s.chunks.push_back((start_ms, samples));
                // Nicht unbegrenzt sammeln (etwa 20 Sekunden)
                while s.chunks.len() > 800 {
                    s.chunks.pop_front();
                }
            }
        });
    }

    pub fn play(&self, from_ms: f64) {
        self.with(|s| {
            s.playing = true;
            s.restart_at = Some(from_ms);
        });
    }

    pub fn pause(&self) {
        self.with(|s| {
            s.playing = false;
            s.clock = None;
        });
    }

    pub fn set_muted(&self, muted: bool) {
        self.with(|s| s.muted = muted);
    }

    pub fn muted(&self) -> bool {
        self.with(|s| s.muted)
    }

    /// Zeit des gerade hörbaren Tons, solange abgespielt wird.
    pub fn clock_ms(&self) -> Option<f64> {
        self.with(|s| if s.playing && s.device_ok { s.clock } else { None })
    }

    pub fn shutdown(&self) {
        self.with(|s| s.quit = true);
    }
}

/// Füllt `frames` Stereo-Abtastwerte ab der Zeit `cursor` aus den gesammelten Stücken.
fn fill(chunks: &mut VecDeque<(f64, Vec<i16>)>, cursor: f64, frames: usize, out: &mut Vec<i16>) {
    out.clear();
    while out.len() < frames * 2 {
        let t = cursor + (out.len() / 2) as f64 * 1000.0 / RATE;
        while let Some((start, data)) = chunks.front() {
            let end = start + (data.len() / 2) as f64 * 1000.0 / RATE;
            if end <= t {
                chunks.pop_front();
            } else {
                break;
            }
        }
        let missing = frames * 2 - out.len();
        match chunks.front() {
            Some((start, data)) => {
                if *start > t + 0.5 {
                    // Lücke bis zum nächsten Stück mit Stille füllen
                    let gap = (((start - t) * RATE / 1000.0) as usize * 2).min(missing).max(2);
                    out.resize(out.len() + gap.min(missing), 0);
                } else {
                    let idx = (((t - start) * RATE / 1000.0).round() as usize * 2).min(data.len());
                    let take = (data.len() - idx).min(missing);
                    if take == 0 {
                        chunks.pop_front();
                        continue;
                    }
                    out.extend_from_slice(&data[idx..idx + take]);
                }
            }
            None => out.resize(frames * 2, 0),
        }
    }
    out.truncate(frames * 2);
}

#[cfg(windows)]
mod player {
    use super::{fill, AudioBus, RATE};
    use windows::core::PSTR;
    use windows::Win32::Media::Audio::*;
    use windows::Win32::Media::{MMTIME, TIME_SAMPLES};

    const BLOCK: usize = 1920; // 40 ms
    const BUFFERS: usize = 4;

    struct Buf {
        hdr: WAVEHDR,
        data: Vec<i16>,
        queued: bool,
    }

    pub fn run(bus: AudioBus) {
        let fmt = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: RATE as u32,
            nAvgBytesPerSec: RATE as u32 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let mut hwo = HWAVEOUT::default();
        let ok = unsafe { waveOutOpen(Some(&mut hwo), WAVE_MAPPER, &fmt, None, None, CALLBACK_NULL) } == 0;
        bus.with(|s| s.device_ok = ok);
        if !ok {
            return;
        }
        let mut bufs: Vec<Box<Buf>> = (0..BUFFERS)
            .map(|_| Box::new(Buf { hdr: WAVEHDR::default(), data: vec![0; BLOCK * 2], queued: false }))
            .collect();
        let mut running = false;
        let mut cursor = 0.0f64;
        let mut base = 0.0f64;
        let mut scratch = Vec::with_capacity(BLOCK * 2);
        let hdr_size = std::mem::size_of::<WAVEHDR>() as u32;

        let release_all = |hwo: HWAVEOUT, bufs: &mut [Box<Buf>]| unsafe {
            let _ = waveOutReset(hwo);
            for b in bufs.iter_mut() {
                if b.queued {
                    let _ = waveOutUnprepareHeader(hwo, &mut b.hdr, hdr_size);
                    b.queued = false;
                }
            }
        };

        loop {
            std::thread::sleep(std::time::Duration::from_millis(5));
            let (quit, playing, restart, muted) =
                bus.with(|s| (s.quit, s.playing, s.restart_at.take(), s.muted));
            if quit {
                release_all(hwo, &mut bufs);
                unsafe {
                    let _ = waveOutClose(hwo);
                }
                return;
            }
            if let Some(t) = restart {
                release_all(hwo, &mut bufs);
                cursor = t;
                base = t;
                running = true;
            }
            if !playing {
                if running {
                    release_all(hwo, &mut bufs);
                    running = false;
                }
                continue;
            }
            // Fertige Puffer neu füllen
            for b in bufs.iter_mut() {
                let done = !b.queued || b.hdr.dwFlags & WHDR_DONE != 0;
                if !done {
                    continue;
                }
                unsafe {
                    if b.queued {
                        let _ = waveOutUnprepareHeader(hwo, &mut b.hdr, hdr_size);
                    }
                }
                bus.with(|s| fill(&mut s.chunks, cursor, BLOCK, &mut scratch));
                if muted {
                    scratch.iter_mut().for_each(|v| *v = 0);
                }
                b.data.copy_from_slice(&scratch);
                cursor += BLOCK as f64 * 1000.0 / RATE;
                b.hdr = WAVEHDR {
                    lpData: PSTR(b.data.as_mut_ptr() as *mut u8),
                    dwBufferLength: (BLOCK * 4) as u32,
                    ..Default::default()
                };
                unsafe {
                    let _ = waveOutPrepareHeader(hwo, &mut b.hdr, hdr_size);
                    let _ = waveOutWrite(hwo, &mut b.hdr, hdr_size);
                }
                b.queued = true;
            }
            // Uhr: tatsächlich ausgegebene Abtastwerte
            let mut mmt = MMTIME { wType: TIME_SAMPLES, ..Default::default() };
            let pos_ok = unsafe { waveOutGetPosition(hwo, &mut mmt, std::mem::size_of::<MMTIME>() as u32) } == 0;
            if pos_ok && mmt.wType == TIME_SAMPLES {
                let played = unsafe { mmt.u.sample } as f64;
                bus.with(|s| s.clock = Some(base + played * 1000.0 / RATE));
            }
        }
    }
}

#[cfg(not(windows))]
mod player {
    pub fn run(_bus: super::AudioBus) {}
}

// ---------------------------------------------------------------------------
// Spur lesen und dekodieren
// ---------------------------------------------------------------------------

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Codec {
    Ac3,
    Lpcm,
    Mpeg,
}

impl Codec {
    pub fn of(attr: &AudioAttr) -> Option<Codec> {
        match attr.coding {
            0 => Some(Codec::Ac3),
            4 => Some(Codec::Lpcm),
            2 | 3 => Some(Codec::Mpeg),
            _ => None,
        }
    }
}

/// Welche Spur die Vorschau hören lässt.
#[derive(Clone, Copy, Debug)]
pub struct TrackChoice {
    pub phys: u8,
    pub codec: Codec,
    pub channels: u8,
}

/// Inhalt eines Tonpakets der gewählten Spur.
pub struct AudioPes<'a> {
    pub pts: Option<u64>,
    pub data: &'a [u8],
    /// Bei LPCM: (Bits, Abtastrate, Kanäle)
    pub lpcm: Option<(u8, u32, u8)>,
}

/// Prüft, ob ein Paket zur Spur gehört, und liefert dann dessen Nutzdaten.
pub fn audio_pes<'a>(s: &'a [u8], track: &TrackChoice) -> Option<AudioPes<'a>> {
    let h = pack_header_len(s);
    if h + 9 > s.len() || s[h] != 0 || s[h + 1] != 0 || s[h + 2] != 1 {
        return None;
    }
    let id = s[h + 3];
    let len = usize::from(u16::from_be_bytes([s[h + 4], s[h + 5]]));
    let end = (h + 6 + len).min(s.len());
    let hdr = usize::from(s[h + 8]);
    let pts = if s[h + 7] & 0x80 != 0 && h + 14 <= s.len() { Some(read_ts(&s[h + 9..])) } else { None };
    let mut p = h + 9 + hdr;
    match (id, track.codec) {
        (0xC0..=0xC7, Codec::Mpeg) if id & 7 == track.phys => Some(AudioPes { pts, data: s.get(p..end)?, lpcm: None }),
        (0xBD, Codec::Ac3) => {
            let sub = *s.get(p)?;
            if sub != 0x80 | track.phys {
                return None;
            }
            p += 4;
            Some(AudioPes { pts, data: s.get(p..end)?, lpcm: None })
        }
        (0xBD, Codec::Lpcm) => {
            let sub = *s.get(p)?;
            if sub != 0xA0 | track.phys {
                return None;
            }
            let info = *s.get(p + 5)?;
            let bits = match info >> 6 {
                0 => 16,
                1 => 20,
                _ => 24,
            };
            let rate = if (info >> 4) & 3 == 1 { 96_000 } else { 48_000 };
            let ch = (info & 7) + 1;
            p += 7;
            Some(AudioPes { pts, data: s.get(p..end)?, lpcm: Some((bits, rate, ch)) })
        }
        _ => None,
    }
}

/// Dekodiert eine Spur zu 48 kHz Stereo. Zeiten in Millisekunden der Vorschau.
pub struct TrackDecoder {
    codec: Codec,
    /// Zusammenhängender Strom (AC-3/MPEG) samt Zeitmarken (Byte-Position, Zeit)
    stream: Vec<u8>,
    marks: VecDeque<(usize, f64)>,
    next_time: Option<f64>,
    /// Zeitmarken der Rahmen, die noch im Decoder stecken
    frame_times: VecDeque<Option<f64>>,
    #[cfg(windows)]
    mf: Option<mf::MfDecoder>,
}

impl TrackDecoder {
    pub fn new(track: &TrackChoice) -> Option<Self> {
        #[cfg(windows)]
        let mf = match track.codec {
            Codec::Lpcm => None,
            c => Some(mf::MfDecoder::new(c, track.channels)?),
        };
        #[cfg(not(windows))]
        if track.codec != Codec::Lpcm {
            return None;
        }
        Some(Self {
            codec: track.codec,
            stream: Vec::new(),
            marks: VecDeque::new(),
            next_time: None,
            frame_times: VecDeque::new(),
            #[cfg(windows)]
            mf,
        })
    }

    /// Nach einem Sprung: angefangene Daten verwerfen.
    pub fn flush(&mut self) {
        self.stream.clear();
        self.marks.clear();
        self.frame_times.clear();
        self.next_time = None;
        #[cfg(windows)]
        if let Some(m) = self.mf.as_mut() {
            m.flush();
        }
    }

    /// Nimmt die Daten eines Pakets auf; `time` ist die Zeit des ersten darin beginnenden Rahmens.
    pub fn feed(&mut self, pes: &AudioPes, time: Option<f64>, out: &mut Vec<(f64, Vec<i16>)>) {
        if let Some((bits, rate, ch)) = pes.lpcm {
            let t = time.or(self.next_time).unwrap_or(0.0);
            let samples = lpcm_to_stereo(pes.data, bits, rate, ch);
            let dur = (samples.len() / 2) as f64 * 1000.0 / RATE;
            self.next_time = Some(t + dur);
            if !samples.is_empty() {
                out.push((t, samples));
            }
            return;
        }
        if let Some(t) = time {
            self.marks.push_back((self.stream.len(), t));
        }
        self.stream.extend_from_slice(pes.data);
        self.decode_frames(out);
    }

    fn decode_frames(&mut self, out: &mut Vec<(f64, Vec<i16>)>) {
        #[cfg(windows)]
        {
            let Some(mf) = self.mf.as_mut() else { return };
            let mut pos = 0usize;
            loop {
                // Nächsten Rahmenanfang suchen
                let Some(start) = find_sync(&self.stream, pos, self.codec) else {
                    pos = self.stream.len().saturating_sub(3);
                    break;
                };
                let Some(len) = frame_len(&self.stream[start..], self.codec) else {
                    pos = start + 1;
                    continue;
                };
                if start + len > self.stream.len() {
                    pos = start;
                    break;
                }
                // Ein Zeitstempel gilt für den ersten Rahmen, der an oder nach ihm beginnt.
                let mut t = None;
                while let Some(&(at, mt)) = self.marks.front() {
                    if at <= start {
                        t = Some(mt);
                        self.marks.pop_front();
                    } else {
                        break;
                    }
                }
                // Jeder Rahmen ergibt ein Stück; die Zeiten laufen in derselben Reihenfolge.
                self.frame_times.push_back(t);
                let time = t.or(self.next_time);
                for (_, pcm) in mf.decode(&self.stream[start..start + len], time) {
                    let dur = (pcm.len() / 2) as f64 * 1000.0 / RATE;
                    let at = self.frame_times.pop_front().flatten().or(self.next_time).unwrap_or(0.0);
                    self.next_time = Some(at + dur);
                    out.push((at, pcm));
                }
                pos = start + len;
            }
            self.stream.drain(..pos.min(self.stream.len()));
            for m in self.marks.iter_mut() {
                m.0 = m.0.saturating_sub(pos);
            }
        }
        #[cfg(not(windows))]
        {
            let _ = out;
        }
    }
}

/// LPCM (Big-Endian) zu 48 kHz Stereo, 16 Bit. 20/24 Bit werden auf 16 Bit gekürzt.
fn lpcm_to_stereo(data: &[u8], bits: u8, rate: u32, ch: u8) -> Vec<i16> {
    let ch = usize::from(ch.max(1));
    let step = if rate == 96_000 { 2 } else { 1 };
    let mut out = Vec::with_capacity(data.len() / 2);
    match bits {
        16 => {
            let frame = 2 * ch;
            for (n, f) in data.chunks_exact(frame).enumerate() {
                if n % step != 0 {
                    continue;
                }
                let l = i16::from_be_bytes([f[0], f[1]]);
                let r = if ch > 1 { i16::from_be_bytes([f[2], f[3]]) } else { l };
                out.push(l);
                out.push(r);
            }
        }
        _ => {
            // 20/24 Bit: Je zwei Abtastwerte pro Kanal stehen zuerst die oberen 16 Bit,
            // danach die unteren Bits. Für die Vorschau genügen die oberen.
            let low = if bits == 24 { 2 } else { 1 };
            let group = 2 * ch * 2 + ch * low;
            for g in data.chunks_exact(group) {
                for k in 0..2 {
                    let base = k * ch * 2;
                    let l = i16::from_be_bytes([g[base], g[base + 1]]);
                    let r = if ch > 1 { i16::from_be_bytes([g[base + 2], g[base + 3]]) } else { l };
                    out.push(l);
                    out.push(r);
                }
            }
            if step == 2 {
                out = out.chunks_exact(4).flat_map(|c| [c[0], c[1]]).collect();
            }
        }
    }
    out
}

fn find_sync(d: &[u8], from: usize, codec: Codec) -> Option<usize> {
    let mut i = from;
    while i + 1 < d.len() {
        let ok = match codec {
            Codec::Ac3 => d[i] == 0x0B && d[i + 1] == 0x77,
            Codec::Mpeg => d[i] == 0xFF && d[i + 1] & 0xE0 == 0xE0,
            Codec::Lpcm => true,
        };
        if ok {
            return Some(i);
        }
        i += 1;
    }
    None
}

/// Länge eines Rahmens in Bytes.
fn frame_len(d: &[u8], codec: Codec) -> Option<usize> {
    match codec {
        Codec::Ac3 => {
            if d.len() < 5 {
                return None;
            }
            const RATES: [usize; 19] =
                [32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 576, 640];
            let fscod = d[4] >> 6;
            let code = usize::from(d[4] & 0x3F);
            let kbps = *RATES.get(code >> 1)?;
            match fscod {
                0 => Some(kbps * 4),
                1 => Some((kbps * 2 * 48 / 44 + (code & 1)) * 2),
                2 => Some(kbps * 6),
                _ => None,
            }
        }
        Codec::Mpeg => {
            if d.len() < 4 {
                return None;
            }
            let version = (d[1] >> 3) & 3; // 3 = MPEG-1, 2 = MPEG-2
            let layer = (d[1] >> 1) & 3; // 2 = Layer II, 1 = Layer III
            let bitrate_idx = usize::from(d[2] >> 4);
            let rate_idx = usize::from((d[2] >> 2) & 3);
            let padding = usize::from((d[2] >> 1) & 1);
            const L2: [usize; 16] = [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 0];
            const L2_V2: [usize; 16] = [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160, 0];
            let rates = if version == 3 { [44_100, 48_000, 32_000] } else { [22_050, 24_000, 16_000] };
            let rate = *rates.get(rate_idx)?;
            if layer != 2 && layer != 1 {
                return None;
            }
            let kbps = if version == 3 { L2[bitrate_idx] } else { L2_V2[bitrate_idx] };
            if kbps == 0 {
                return None;
            }
            let factor = if version != 3 && layer == 1 { 72 } else { 144 };
            Some(factor * kbps * 1000 / rate + padding)
        }
        Codec::Lpcm => None,
    }
}

#[cfg(windows)]
mod mf {
    use super::{Codec, RATE};
    use windows::core::GUID;
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

    pub struct MfDecoder {
        mft: IMFTransform,
        channels: u32,
        float: bool,
        bits: u32,
        provides: bool,
        out_size: u32,
    }

    fn choose_output(mft: &IMFTransform) -> Option<(u32, bool, u32)> {
        unsafe {
            let mut best: Option<(IMFMediaType, u32, bool, u32, i32)> = None;
            for i in 0..64 {
                let Ok(t) = mft.GetOutputAvailableType(0, i) else { break };
                let sub = t.GetGUID(&MF_MT_SUBTYPE).unwrap_or(GUID::zeroed());
                let ch = t.GetUINT32(&MF_MT_AUDIO_NUM_CHANNELS).unwrap_or(0);
                let bits = t.GetUINT32(&MF_MT_AUDIO_BITS_PER_SAMPLE).unwrap_or(0);
                let float = sub == MFAudioFormat_Float;
                let pcm16 = sub == MFAudioFormat_PCM && bits == 16;
                if !(float || pcm16) {
                    continue;
                }
                // Stereo bevorzugt, dann Ganzzahl
                let score = i32::from(ch == 2) * 4 + i32::from(pcm16) * 2 + i32::from(ch > 0);
                if best.as_ref().is_none_or(|b| score > b.4) {
                    best = Some((t, ch, float, bits, score));
                }
            }
            let (t, ch, float, bits, _) = best?;
            mft.SetOutputType(0, &t, 0).ok()?;
            Some((ch.max(1), float, bits))
        }
    }

    impl MfDecoder {
        pub fn new(codec: Codec, channels: u8) -> Option<Self> {
            unsafe {
                let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
                MFStartup(MF_VERSION, MFSTARTUP_LITE).ok()?;
                let subtype = match codec {
                    Codec::Ac3 => MFAudioFormat_Dolby_AC3,
                    Codec::Mpeg => MFAudioFormat_MPEG,
                    Codec::Lpcm => return None,
                };
                let info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Audio, guidSubtype: subtype };
                let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
                let mut count = 0u32;
                MFTEnumEx(
                    MFT_CATEGORY_AUDIO_DECODER,
                    MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER,
                    Some(&info),
                    None,
                    &mut list,
                    &mut count,
                )
                .ok()?;
                if list.is_null() || count == 0 {
                    return None;
                }
                let items = std::slice::from_raw_parts_mut(list, count as usize);
                let activate = items[0].take();
                for it in items.iter_mut() {
                    drop(it.take());
                }
                CoTaskMemFree(Some(list as *const _));
                let mft: IMFTransform = activate?.ActivateObject().ok()?;

                let input = MFCreateMediaType().ok()?;
                input.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Audio).ok()?;
                input.SetGUID(&MF_MT_SUBTYPE, &subtype).ok()?;
                input.SetUINT32(&MF_MT_AUDIO_SAMPLES_PER_SECOND, RATE as u32).ok()?;
                input.SetUINT32(&MF_MT_AUDIO_NUM_CHANNELS, u32::from(channels.max(1))).ok()?;
                mft.SetInputType(0, &input, 0).ok()?;
                let (ch, float, bits) = choose_output(&mft)?;
                let info = mft.GetOutputStreamInfo(0).ok()?;
                let provides = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
                let _ = mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0);
                Some(Self { mft, channels: ch, float, bits, provides, out_size: info.cbSize.max(1536 * 8 * 4) })
            }
        }

        pub fn flush(&mut self) {
            unsafe {
                let _ = self.mft.ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            }
        }

        /// Ein Rahmen hinein, fertige Stücke (Zeit, Stereo-Abtastwerte) heraus.
        pub fn decode(&mut self, frame: &[u8], time_ms: Option<f64>) -> Vec<(Option<f64>, Vec<i16>)> {
            let mut out = Vec::new();
            unsafe {
                let Ok(buf) = MFCreateMemoryBuffer(frame.len() as u32) else { return out };
                let mut ptr: *mut u8 = std::ptr::null_mut();
                if buf.Lock(&mut ptr, None, None).is_err() {
                    return out;
                }
                std::ptr::copy_nonoverlapping(frame.as_ptr(), ptr, frame.len());
                let _ = buf.Unlock();
                let _ = buf.SetCurrentLength(frame.len() as u32);
                let Ok(sample) = MFCreateSample() else { return out };
                let _ = sample.AddBuffer(&buf);
                if let Some(t) = time_ms {
                    let _ = sample.SetSampleTime((t * 10_000.0) as i64);
                }
                if self.mft.ProcessInput(0, &sample, 0).is_err() {
                    // Voll: erst abholen, dann noch einmal
                    self.drain(&mut out);
                    let _ = self.mft.ProcessInput(0, &sample, 0);
                }
                self.drain(&mut out);
            }
            out
        }

        unsafe fn drain(&mut self, out: &mut Vec<(Option<f64>, Vec<i16>)>) {
            for _ in 0..16 {
                let own = if self.provides {
                    None
                } else {
                    let Ok(s) = MFCreateSample() else { return };
                    let Ok(b) = MFCreateMemoryBuffer(self.out_size) else { return };
                    let _ = s.AddBuffer(&b);
                    Some(s)
                };
                let mut db = [MFT_OUTPUT_DATA_BUFFER {
                    dwStreamID: 0,
                    pSample: std::mem::ManuallyDrop::new(own),
                    dwStatus: 0,
                    pEvents: std::mem::ManuallyDrop::new(None),
                }];
                let mut status = 0u32;
                let res = self.mft.ProcessOutput(0, &mut db, &mut status);
                let sample = std::mem::ManuallyDrop::into_inner(std::mem::replace(
                    &mut db[0].pSample,
                    std::mem::ManuallyDrop::new(None),
                ));
                drop(std::mem::ManuallyDrop::into_inner(std::mem::replace(
                    &mut db[0].pEvents,
                    std::mem::ManuallyDrop::new(None),
                )));
                match res {
                    Ok(()) => {
                        if let Some(s) = sample {
                            if let Some(chunk) = self.read_sample(&s) {
                                out.push(chunk);
                            }
                        }
                    }
                    Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                        if let Some((ch, float, bits)) = choose_output(&self.mft) {
                            self.channels = ch;
                            self.float = float;
                            self.bits = bits;
                        }
                    }
                    Err(_) => return,
                }
            }
        }

        unsafe fn read_sample(&self, s: &IMFSample) -> Option<(Option<f64>, Vec<i16>)> {
            let time = s.GetSampleTime().ok().map(|t| t as f64 / 10_000.0);
            let buf = s.ConvertToContiguousBuffer().ok()?;
            let mut ptr: *mut u8 = std::ptr::null_mut();
            let mut len = 0u32;
            buf.Lock(&mut ptr, None, Some(&mut len)).ok()?;
            let bytes = std::slice::from_raw_parts(ptr, len as usize);
            let ch = self.channels as usize;
            let mut out = Vec::new();
            let get = |frame: &[u8], k: usize| -> f32 {
                if self.float {
                    f32::from_le_bytes([frame[k * 4], frame[k * 4 + 1], frame[k * 4 + 2], frame[k * 4 + 3]])
                } else {
                    f32::from(i16::from_le_bytes([frame[k * 2], frame[k * 2 + 1]])) / 32768.0
                }
            };
            let width = if self.float { 4 } else { (self.bits / 8).max(2) as usize };
            for frame in bytes.chunks_exact(width * ch) {
                let (l, r) = match ch {
                    1 => (get(frame, 0), get(frame, 0)),
                    2 => (get(frame, 0), get(frame, 1)),
                    _ => {
                        // Mehrkanal (FL FR C LFE SL SR) auf Stereo
                        let c = if ch > 2 { get(frame, 2) } else { 0.0 };
                        let sl = if ch > 4 { get(frame, 4) } else { 0.0 };
                        let sr = if ch > 5 { get(frame, 5) } else { 0.0 };
                        let l = (get(frame, 0) + 0.707 * c + 0.707 * sl) / 2.0;
                        let r = (get(frame, 1) + 0.707 * c + 0.707 * sr) / 2.0;
                        (l, r)
                    }
                };
                out.push((l.clamp(-1.0, 1.0) * 32767.0) as i16);
                out.push((r.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
            let _ = buf.Unlock();
            Some((time, out))
        }
    }
}
