//! Saubere Tonübergänge an Schnittstellen.
//!
//! Beim Schneiden werden ganze Tonpakete übernommen oder weggelassen. Ein Tonrahmen
//! (AC-3 oder MPEG-Ton) liegt aber oft über zwei Paketen. Am Anfang eines Eintrags
//! beginnt der Ton deshalb mitten in einem Rahmen, am Ende hört er mitten in einem auf,
//! und bildgenaue Schnitte können auch im Eintrag solche Bruchstücke hinterlassen.
//! Manche Player (z. B. Kodi) dekodieren die Bruchstücke und erzeugen ein Knacken.
//!
//! Hier wird der Tonstrom jeder Spur Rahmen für Rahmen verfolgt. Bytes, die zu keinem
//! vollständigen Rahmen gehören, werden aus den Paketen entfernt; die Pakete werden
//! dafür gekürzt und mit Füllbytes aufgefüllt, jeder Sektor bleibt 2048 Bytes groß.
//! Eine VOBU wird erst weitergegeben, wenn die nächste da ist, denn ob ihr letzter
//! Rahmen vollständig ist, zeigt sich erst dort.

use crate::dvd::vob::{get_scr, pack_header_len, put_scr, read_ts, write_ts, DSI_OFF, PCI_OFF};
use crate::dvd::{be32, SECTOR};
use crate::engine::audio::{find_sync, frame_len, Codec};

/// Stand einer Spur am Anfang der nächsten VOBU.
#[derive(Clone, Copy, Debug, PartialEq)]
enum State {
    /// Die ersten n Bytes gehören noch zu einem geprüften Rahmen.
    Cont(usize),
    /// Nächsten vollständigen Rahmen suchen, alles davor entfällt.
    Resync,
}

/// Lage der Ton-Nutzdaten in einer VOBU.
#[derive(Clone, Copy, Debug)]
struct Piece {
    /// Anfang des PES-Pakets (Startcode) in den VOBU-Daten
    pes: usize,
    /// Nutzdaten in den VOBU-Daten
    start: usize,
    end: usize,
    /// Zeitstempel des ersten Rahmens, der in diesem Paket beginnt
    pts: Option<i64>,
}

/// Alle Ton-Nutzdaten einer VOBU je Spur: (Kennung, Codec, Stücke).
fn audio_pieces(data: &[u8]) -> Vec<(u8, Codec, Vec<Piece>)> {
    let mut out: Vec<(u8, Codec, Vec<Piece>)> = Vec::new();
    for (si, s) in data.chunks_exact(SECTOR).enumerate() {
        if s[..4] != [0, 0, 1, 0xBA] {
            continue;
        }
        let base = si * SECTOR;
        let mut p = pack_header_len(s);
        while p + 9 <= SECTOR && s[p] == 0 && s[p + 1] == 0 && s[p + 2] == 1 {
            let id = s[p + 3];
            let end = p + 6 + usize::from(u16::from_be_bytes([s[p + 4], s[p + 5]]));
            if end > SECTOR {
                break;
            }
            let payload = p + 9 + usize::from(s[p + 8]);
            let found = match id {
                0xC0..=0xC7 => Some((id, Codec::Mpeg, payload)),
                0xBD if payload + 4 <= end && (0x80..=0x87).contains(&s[payload]) => Some((s[payload], Codec::Ac3, payload + 4)),
                _ => None,
            };
            if let Some((key, codec, start)) = found.filter(|f| f.2 <= end) {
                let pts = (s[p + 7] & 0x80 != 0 && p + 14 <= end).then(|| read_ts(&s[p + 9..]) as i64);
                let piece = Piece { pes: base + p, start: base + start, end: base + end, pts };
                match out.iter_mut().find(|o| o.0 == key) {
                    Some(o) => o.2.push(piece),
                    None => out.push((key, codec, vec![piece])),
                }
            }
            p = end;
        }
    }
    out
}

fn gather(data: &[u8], pieces: &[Piece]) -> Vec<u8> {
    let mut es = Vec::new();
    for p in pieces {
        es.extend_from_slice(&data[p.start..p.end]);
    }
    es
}

/// Hält je Eintrag eine VOBU zurück und bereinigt den Ton an ihren Rändern.
#[derive(Default)]
pub struct AudioSplicer {
    held: Option<Vec<u8>>,
    state: Vec<(u8, State)>,
    /// Bildanfang des Eintrags (aus der ersten VOBU)
    start_ptm: Option<i64>,
    /// Spuren, die schon Ton ab dem Bildanfang geliefert haben. Liegt der Ton im Datenstrom
    /// weit hinter dem Bild, enthält die erste VOBU noch keinen; dann gilt der Anfang weiter.
    started: Vec<u8>,
}

/// Anfangs- und Endzeit des Bildes einer VOBU laut Navigationspaket (90 kHz).
fn vobu_ptm(data: &[u8]) -> (Option<i64>, Option<i64>) {
    // Navigationspaket: Packkopf, dann Systemkopf
    if data.len() < SECTOR || data[..4] != [0, 0, 1, 0xBA] || data[14..18] != [0, 0, 1, 0xBB] {
        return (None, None);
    }
    let (s, e) = (be32(data, PCI_OFF + 12), be32(data, PCI_OFF + 16));
    if s == 0 && e == 0 {
        return (None, None);
    }
    (Some(i64::from(s)), Some(i64::from(e)))
}

/// Ton-Kennung und Zeitstempel eines Tonpakets.
fn audio_pack(s: &[u8]) -> Option<(u8, Option<i64>)> {
    if s.len() < SECTOR || s[..4] != [0, 0, 1, 0xBA] {
        return None;
    }
    let p = pack_header_len(s);
    if p + 14 > SECTOR || s[p..p + 3] != [0, 0, 1] {
        return None;
    }
    let id = s[p + 3];
    let payload = p + 9 + usize::from(s[p + 8]);
    let key = match id {
        0xC0..=0xC7 => id,
        0xBD if payload < SECTOR && matches!(s[payload], 0x80..=0x87 | 0xA0..=0xA7) => s[payload],
        _ => return None,
    };
    let pts = (s[p + 7] & 0x80 != 0).then(|| read_ts(&s[p + 9..]) as i64);
    Some((key, pts))
}

/// Hängt an die letzte VOBU eines Eintrags die Tonpakete der folgenden VOBU an, die noch
/// vor dem Bildende beginnen. Viele DVDs multiplexen den Ton etwas nach dem Bild; ohne
/// diese Pakete fehlte am Ende jedes Eintrags ein Stück Ton. Was über das Bildende
/// hinausragt, entfernt danach der `AudioSplicer`.
pub fn append_tail_audio(last: &mut Vec<u8>, next: &[u8]) {
    let (_, Some(end)) = vobu_ptm(last) else { return };
    let Some(mut scr) = last.chunks_exact(SECTOR).rev().find(|s| s[..4] == [0, 0, 1, 0xBA]).map(get_scr) else { return };
    let mut keep: Vec<(u8, bool)> = Vec::new();
    let mut added = 0;
    for s in next.chunks_exact(SECTOR) {
        let Some((key, pts)) = audio_pack(s) else { continue };
        let k = match keep.iter_mut().find(|k| k.0 == key) {
            Some(k) => k,
            None => {
                keep.push((key, false));
                keep.last_mut().unwrap()
            }
        };
        if let Some(p) = pts {
            k.1 = ts_diff(p, end) < 0 && ts_diff(p, end) > -5 * 90_000;
        }
        if !k.1 {
            continue;
        }
        scr += SCR_TICK;
        let at = last.len();
        last.extend_from_slice(s);
        put_scr(&mut last[at..at + SECTOR], scr);
        added += 1;
        if added >= 64 {
            break;
        }
    }
    if added > 0 {
        // Endadresse der VOBU im Navigationspaket
        let count = (last.len() / SECTOR) as u32;
        last[DSI_OFF + 8..DSI_OFF + 12].copy_from_slice(&(count - 1).to_be_bytes());
    }
}

/// Systemuhr-Schritt je Sektor bei 10,08 MBit/s (27 MHz)
const SCR_TICK: u64 = 43_886;

impl AudioSplicer {
    /// Ton aus der VOBU hinter dem Eintrag an die zurückgehaltene letzte VOBU hängen.
    pub fn extend_last(&mut self, next: &[u8]) {
        if let Some(last) = self.held.as_mut() {
            append_tail_audio(last, next);
        }
    }

    /// Nimmt die nächste VOBU an und gibt die vorige bereinigt zurück.
    pub fn push(&mut self, data: Vec<u8>) -> Option<Vec<u8>> {
        let Some(mut prev) = self.held.replace(data) else {
            self.start_ptm = self.held.as_deref().and_then(|d| vobu_ptm(d).0);
            return None;
        };
        let next = self.held.take();
        self.clean(&mut prev, next.as_deref(), None);
        self.held = next;
        Some(prev)
    }

    /// Ende des Eintrags: Die letzte VOBU verliert einen angefangenen Rahmen.
    pub fn finish(&mut self) -> Option<Vec<u8>> {
        let mut last = self.held.take()?;
        let e = vobu_ptm(&last).1;
        self.clean(&mut last, None, e);
        self.state.clear();
        self.started.clear();
        self.start_ptm = None;
        Some(last)
    }

    fn state_of(&self, key: u8) -> State {
        self.state.iter().find(|s| s.0 == key).map(|s| s.1).unwrap_or(State::Resync)
    }

    fn set_state(&mut self, key: u8, st: State) {
        match self.state.iter_mut().find(|s| s.0 == key) {
            Some(s) => s.1 = st,
            None => self.state.push((key, st)),
        }
    }

    /// `end`: Bildende des Eintrags, falls es in dieser VOBU liegt.
    fn clean(&mut self, cur: &mut [u8], next: Option<&[u8]>, end: Option<i64>) {
        let next_pieces = next.map(audio_pieces).unwrap_or_default();
        for (key, codec, pieces) in audio_pieces(cur) {
            let start = if self.started.contains(&key) { None } else { self.start_ptm };
            let es_cur = gather(cur, &pieces);
            let b = es_cur.len();
            let mut all = es_cur;
            if let (Some(n), Some((_, _, np))) = (next, next_pieces.iter().find(|o| o.0 == key)) {
                all.extend_from_slice(&gather(n, np));
            }
            let (mut remove, frames, st) = scan(&all, b, codec, self.state_of(key), next.is_some());
            self.set_state(key, st);
            if start.is_some() || end.is_some() {
                trim_to_picture(&all, &pieces, codec, &frames, start, end, &mut remove);
                // Ragt ein entfernter Rahmen in die nächste VOBU, fällt sein Rest dort weg.
                if remove.iter().any(|r| r.1 > b) {
                    self.set_state(key, State::Resync);
                }
            }
            // Ein Rahmen ist geblieben: Ab jetzt läuft der Ton dieser Spur ab dem Bildanfang.
            if frames.iter().any(|f| !remove.iter().any(|r| r.0 <= *f && *f < r.1)) {
                self.started.push(key);
            }
            if !remove.is_empty() {
                let dur = frames.first().and_then(|f| frame_ticks(&all[*f..], codec));
                rewrite(cur, &pieces, codec, &remove, &frames, dur);
            }
        }
    }
}

/// Unterschied zweier Zeitstempel der 33-Bit-Uhr.
fn ts_diff(a: i64, b: i64) -> i64 {
    const WRAP: i64 = 1 << 33;
    let d = (a - b).rem_euclid(WRAP);
    if d > WRAP / 2 {
        d - WRAP
    } else {
        d
    }
}

/// Entfernt Rahmen, die überwiegend vor dem ersten oder nach dem letzten Bild liegen.
/// So treffen Ton und Bild an der Schnittstelle zusammen, ohne Überlappung oder Lücke.
fn trim_to_picture(
    all: &[u8],
    pieces: &[Piece],
    codec: Codec,
    frames: &[usize],
    start: Option<i64>,
    end: Option<i64>,
    remove: &mut Vec<(usize, usize)>,
) {
    let Some(&f0) = frames.first() else { return };
    let Some(dur) = frame_ticks(&all[f0..], codec) else { return };
    // Zeit je Rahmen: Der Zeitstempel eines Pakets gilt für den ersten Rahmen, der darin beginnt.
    let mut anchor: Option<(usize, i64)> = None;
    let mut off = 0usize;
    for p in pieces {
        let len = p.end - p.start;
        if let Some(t) = p.pts {
            if let Some(i) = frames.iter().position(|f| *f >= off && *f < off + len) {
                anchor = Some((i, t));
                break;
            }
        }
        off += len;
    }
    let Some((ai, at)) = anchor else { return };
    let time = |i: usize| at + (i as i64 - ai as i64) * dur;
    // Liegt die Tonzeit weit weg vom Bild, stimmt der Bezug nicht: lieber nichts tun.
    let near = |t: i64, r: i64| ts_diff(t, r).abs() < 5 * 90_000;
    for (i, &f) in frames.iter().enumerate() {
        let mid = time(i) + dur / 2;
        let before = start.is_some_and(|s| near(mid, s) && ts_diff(mid, s) < 0);
        let after = end.is_some_and(|e| near(mid, e) && ts_diff(mid, e) > 0);
        if before || after {
            let l = frame_len(&all[f..], codec).unwrap_or(0);
            remove.push((f, f + l));
        }
    }
    remove.sort_unstable();
}

/// Verfolgt die Rahmen in `all[..b]` (dahinter folgt der Ton der nächsten VOBU).
/// Liefert die zu entfernenden Bereiche, die Rahmenanfänge und den Stand danach.
fn scan(all: &[u8], b: usize, codec: Codec, state: State, more: bool) -> (Vec<(usize, usize)>, Vec<usize>, State) {
    let mut remove = Vec::new();
    let mut frames = Vec::new();
    // Ein Rahmen zählt, wenn direkt danach der nächste mit demselben Kopf beginnt oder der
    // Strom dort endet. Der gleiche Kopf schließt zufällige Sync-Muster in den Daten aus.
    let chained = |p: usize| -> Option<usize> {
        let l = frame_len(all.get(p..)?, codec).filter(|l| *l > 4)?;
        let e = p + l;
        if e == all.len() {
            return Some(l);
        }
        if e > all.len() {
            // Läuft in die nächste VOBU, deren Ton zu kurz ist, um es zu prüfen.
            return (more && e > b).then_some(l);
        }
        let same = all.len() >= e + 6 && find_sync(&all[..e + 2], e, codec) == Some(e) && header_key(&all[p..], codec) == header_key(&all[e..], codec);
        // Zu wenig Daten hinter dem Rahmen, um den Kopf zu vergleichen
        let short = all.len() < e + 6 && more;
        (same || short).then_some(l)
    };
    let mut pos = match state {
        State::Cont(n) if n >= b => return (remove, frames, State::Cont(n - b)),
        State::Cont(n) => n,
        State::Resync => 0,
    };
    while pos < b {
        if let Some(l) = chained(pos) {
            frames.push(pos);
            pos += l;
            continue;
        }
        let mut q = pos + 1;
        while q < b && chained(q).is_none() {
            q += 1;
        }
        remove.push((pos, q.min(b)));
        if q >= b {
            return (remove, frames, State::Resync);
        }
        pos = q;
    }
    (remove, frames, State::Cont(pos - b))
}

/// Was bei allen Rahmen einer DVD-Tonspur gleich ist: AC-3 Abtastrate, Datenrate und
/// Version; MPEG-Ton Version, Schicht, Datenrate, Abtastrate und Kanalmodus.
pub(crate) fn header_key(f: &[u8], codec: Codec) -> [u8; 3] {
    match codec {
        Codec::Ac3 => [f[4], f[5], 0],
        _ => [f[1], f[2] & 0xFD, f[3] & 0xC0],
    }
}

/// Dauer eines Rahmens (90 kHz) laut Kopf.
pub(crate) fn frame_ticks(f: &[u8], codec: Codec) -> Option<i64> {
    let (samples, rate): (i64, i64) = match codec {
        Codec::Ac3 => (1536, *[48_000, 44_100, 32_000].get(usize::from(*f.get(4)? >> 6))?),
        _ => {
            let r = *[44_100, 48_000, 32_000].get(usize::from((*f.get(2)? >> 2) & 3))?;
            let r = if (f[1] >> 3) & 3 == 3 { r } else { r / 2 };
            (if (f[1] >> 1) & 3 == 3 { 384 } else { 1152 }, r)
        }
    };
    Some(samples * 90_000 / rate)
}

/// Entfernt Bereiche des Tonstroms aus den Paketen.
fn rewrite(data: &mut [u8], pieces: &[Piece], codec: Codec, remove: &[(usize, usize)], frames: &[usize], dur: Option<i64>) {
    let mut es_at = 0usize;
    for p in pieces {
        let len = p.end - p.start;
        let (a, z) = (es_at, es_at + len);
        es_at = z;
        if !remove.iter().any(|r| r.0 < z && r.1 > a) {
            continue;
        }
        // Fällt der erste Rahmen des Pakets weg, gilt sein Zeitstempel für den nächsten.
        let removed = |i: usize| remove.iter().any(|r| r.0 <= i && i < r.1);
        let starts_here: Vec<usize> = frames.iter().copied().filter(|f| *f >= a && *f < z).collect();
        let new_pts = match (p.pts, dur, starts_here.iter().position(|f| !removed(*f))) {
            (Some(t), Some(d), Some(n)) if n > 0 => Some((t + n as i64 * d).rem_euclid(1 << 33)),
            _ => None,
        };
        // Verbleibende Bytes und Rahmenanfänge darin (relativ zu den neuen Nutzdaten)
        let mut kept = Vec::with_capacity(len);
        let mut starts = Vec::new();
        for i in a..z {
            if remove.iter().any(|r| r.0 <= i && i < r.1) {
                continue;
            }
            if frames.binary_search(&i).is_ok() {
                starts.push(kept.len());
            }
            kept.push(data[p.start + i - a]);
        }
        rewrite_pes(data, p, codec, &kept, &starts);
        if let Some(t) = new_pts {
            let h = p.pes;
            if data[h + 3] != 0xBE && data[h + 7] & 0xC0 == 0x80 {
                let prefix = data[h + 9] >> 4;
                write_ts(&mut data[h + 9..], prefix, t as u64);
            }
        }
    }
}

fn rewrite_pes(data: &mut [u8], p: &Piece, codec: Codec, kept: &[u8], starts: &[usize]) {
    let h = p.pes;
    if kept.is_empty() {
        // Ganzes Paket wird zu Füllung
        data[h + 3] = 0xBE;
        data[h + 6..p.end].fill(0xFF);
        return;
    }
    let k = (p.end - p.start) - kept.len();
    let hdr = usize::from(data[h + 8]);
    // Ohne Rahmenanfang im Paket gehört auch kein Zeitstempel hinein.
    if starts.is_empty() && data[h + 7] == 0x80 && hdr >= 5 {
        data[h + 7] = 0;
        data[h + 9..h + 14].fill(0xFF);
    }
    let private = p.start - (h + 9 + hdr); // AC-3: 4 Bytes Kopf vor den Nutzdaten
    let start = if k >= 6 {
        // Paket kürzen, dahinter ein Füllpaket
        let new_end = p.start + kept.len();
        data[p.start..new_end].copy_from_slice(kept);
        let pes_len = new_end - (h + 6);
        data[h + 4..h + 6].copy_from_slice(&(pes_len as u16).to_be_bytes());
        data[new_end..new_end + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
        data[new_end + 4..new_end + 6].copy_from_slice(&((k - 6) as u16).to_be_bytes());
        data[new_end + 6..p.end].fill(0xFF);
        p.start
    } else {
        // Weniger als ein Füllpaket: als Stopfbytes in den PES-Kopf
        let mid: Vec<u8> = data[h + 9 + hdr..p.start].to_vec();
        let mut at = h + 9 + hdr;
        data[at..at + k].fill(0xFF);
        at += k;
        data[at..at + mid.len()].copy_from_slice(&mid);
        at += mid.len();
        data[at..at + kept.len()].copy_from_slice(kept);
        data[h + 8] = (hdr + k) as u8;
        at
    };
    if codec == Codec::Ac3 && private == 4 {
        let ph = start - 4;
        data[ph + 1] = starts.len().min(255) as u8;
        let first = starts.first().map(|s| (s + 1) as u16).unwrap_or(0);
        data[ph + 2..ph + 4].copy_from_slice(&first.to_be_bytes());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// MPEG-1 Layer II, 48 kHz, 192 kBit/s: 576 Bytes je Rahmen
    fn mpa_frame(fill: u8) -> Vec<u8> {
        let mut f = vec![fill; 576];
        f[..4].copy_from_slice(&[0xFF, 0xFD, 0xA4, 0x00]);
        f
    }

    /// Eine VOBU aus Tonpaketen mit den angegebenen Nutzdaten.
    fn vobu(payloads: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for pl in payloads {
            let mut s = vec![0u8; SECTOR];
            s[..4].copy_from_slice(&[0, 0, 1, 0xBA]);
            s[4] = 0x44;
            s[13] = 0xF8;
            let h = 14;
            s[h..h + 4].copy_from_slice(&[0, 0, 1, 0xC0]);
            s[h + 6] = 0x81;
            s[h + 7] = 0x80;
            s[h + 8] = 5;
            s[h + 9..h + 14].copy_from_slice(&[0x21, 0, 1, 0, 1]);
            let start = h + 14;
            s[start..start + pl.len()].copy_from_slice(pl);
            let end = start + pl.len();
            s[h + 4..h + 6].copy_from_slice(&((end - h - 6) as u16).to_be_bytes());
            if SECTOR - end >= 6 {
                s[end..end + 4].copy_from_slice(&[0, 0, 1, 0xBE]);
                s[end + 4..end + 6].copy_from_slice(&((SECTOR - end - 6) as u16).to_be_bytes());
                s[end + 6..].fill(0xFF);
            }
            out.extend_from_slice(&s);
        }
        out
    }

    fn es_of(data: &[u8]) -> Vec<u8> {
        audio_pieces(data).into_iter().flat_map(|(_, _, p)| gather(data, &p)).collect()
    }

    #[test]
    fn cut_frames_are_removed() {
        let frames: Vec<u8> = (1..=8u8).flat_map(mpa_frame).collect();
        // Eintrag beginnt 100 Bytes in Rahmen 1 und endet 200 Bytes in Rahmen 8.
        let es = &frames[100..576 * 7 + 200];
        let (a, b) = es.split_at(1500);
        let (b, c) = b.split_at(1700);
        let mut sp = AudioSplicer::default();
        assert!(sp.push(vobu(&[a])).is_none());
        let mut out = sp.push(vobu(&[b, c])).unwrap();
        out.extend(sp.finish().unwrap());
        let got = es_of(&out);
        // Übrig bleiben genau die vollständigen Rahmen 2 bis 7.
        assert_eq!(got, frames[576..576 * 7].to_vec());
        // Jeder Sektor bleibt ein gültiges Paket
        for s in out.chunks_exact(SECTOR) {
            assert_eq!(&s[..4], &[0, 0, 1, 0xBA]);
        }
    }

    #[test]
    fn join_of_two_cuts_is_clean() {
        let frames: Vec<u8> = (1..=6u8).flat_map(mpa_frame).collect();
        let mut sp = AudioSplicer::default();
        // Erster Eintrag: endet mitten in Rahmen 3
        let mut out = Vec::new();
        assert!(sp.push(vobu(&[&frames[..576 * 2 + 300]])).is_none());
        out.extend(sp.finish().unwrap());
        // Zweiter Eintrag: beginnt mitten in Rahmen 4, nur drei Bytes zu viel
        assert!(sp.push(vobu(&[&frames[576 * 4 - 3..]])).is_none());
        out.extend(sp.finish().unwrap());
        let got = es_of(&out);
        let mut want = frames[..576 * 2].to_vec();
        want.extend_from_slice(&frames[576 * 4..]);
        assert_eq!(got, want);
    }

    #[test]
    fn intact_stream_is_untouched() {
        let frames: Vec<u8> = (1..=6u8).flat_map(mpa_frame).collect();
        let (a, b) = frames.split_at(2000);
        let (v1, v2) = (vobu(&[a]), vobu(&[b]));
        let mut sp = AudioSplicer::default();
        assert!(sp.push(v1.clone()).is_none());
        assert_eq!(sp.push(v2.clone()).unwrap(), v1);
        assert_eq!(sp.finish().unwrap(), v2);
    }
}
