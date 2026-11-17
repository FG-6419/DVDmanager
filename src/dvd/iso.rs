//! ISO-Abbilder: Lesen (UDF, ersatzweise ISO 9660) und Schreiben (UDF 1.02 mit
//! ISO-9660-Brücke, wie es Video-DVDs brauchen).

use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

use super::{Res, SECTOR};

type Entries = Vec<(String, u64, u64)>;

fn le16(d: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([d[o], d[o + 1]])
}
fn le32(d: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]])
}
fn le64(d: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&d[o..o + 8]);
    u64::from_le_bytes(a)
}

struct Img {
    f: File,
}

impl Img {
    fn sector(&mut self, n: u64) -> Res<Vec<u8>> {
        self.sectors(n, 1)
    }
    fn sectors(&mut self, n: u64, count: usize) -> Res<Vec<u8>> {
        let mut b = vec![0u8; SECTOR * count];
        self.f.seek(SeekFrom::Start(n * SECTOR as u64)).map_err(|e| e.to_string())?;
        self.f.read_exact(&mut b).map_err(|e| trf!("Das ISO-Abbild ist zu kurz: {e}", "The ISO image is too short: {e}"))?;
        Ok(b)
    }
}

/// Liefert Datenträgername und alle Dateien in VIDEO_TS (Name, Byte-Offset, Länge).
pub fn read_video_ts(path: &Path) -> Res<(String, Entries)> {
    let f = File::open(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut img = Img { f };
    if let Ok(r) = read_udf(&mut img) {
        if !r.1.is_empty() {
            return Ok(r);
        }
    }
    read_iso9660(&mut img)
}

fn tag_id(d: &[u8]) -> u16 {
    le16(d, 0)
}

fn dstring(d: &[u8]) -> String {
    if d.is_empty() {
        return String::new();
    }
    let len = usize::from(*d.last().unwrap_or(&0)).min(d.len() - 1);
    if len == 0 {
        return String::new();
    }
    decode_cs0(&d[..len])
}

fn decode_cs0(d: &[u8]) -> String {
    match d.first() {
        Some(8) => d[1..].iter().map(|&c| c as char).collect(),
        Some(16) => d[1..]
            .chunks_exact(2)
            .map(|c| char::from_u32(u32::from(u16::from_be_bytes([c[0], c[1]]))).unwrap_or('?'))
            .collect(),
        _ => String::new(),
    }
}

fn read_udf(img: &mut Img) -> Res<(String, Entries)> {
    let avdp = img.sector(256)?;
    if tag_id(&avdp) != 2 {
        return Err(tr!("kein UDF", "no UDF").into());
    }
    let vds_len = le32(&avdp, 16) as usize;
    let vds_loc = u64::from(le32(&avdp, 20));
    let mut part_start = None;
    let mut fsd_lb = None;
    let mut label = String::new();
    for i in 0..(vds_len / SECTOR).clamp(1, 32) {
        let s = img.sector(vds_loc + i as u64)?;
        match tag_id(&s) {
            5 => part_start = Some(u64::from(le32(&s, 188))),
            6 => {
                fsd_lb = Some(u64::from(le32(&s, 252)));
                label = dstring(&s[84..84 + 128]);
            }
            8 => break,
            _ => {}
        }
    }
    let part = part_start.ok_or(tr!("UDF ohne Partition", "UDF without partition"))?;
    let fsd = img.sector(part + fsd_lb.unwrap_or(0))?;
    if tag_id(&fsd) != 256 {
        return Err(tr!("UDF ohne Dateisatz", "UDF without file set").into());
    }
    let root_lb = u64::from(le32(&fsd, 404));

    let root = read_dir_udf(img, part, root_lb)?;
    let vts = root
        .iter()
        .find(|(n, _, dir)| *dir && n.eq_ignore_ascii_case("VIDEO_TS"))
        .ok_or(tr!("VIDEO_TS fehlt", "VIDEO_TS is missing"))?;
    let listing = read_dir_udf(img, part, vts.1)?;
    let mut out = Vec::new();
    for (name, lb, dir) in listing {
        if dir {
            continue;
        }
        let (exts, size) = file_extents(img, part, lb)?;
        if let Some((start, _)) = exts.first() {
            out.push((name, *start * SECTOR as u64, size));
        } else if size == 0 {
            out.push((name, 0, 0));
        }
    }
    Ok((label, out))
}

fn file_extents(img: &mut Img, part: u64, lb: u64) -> Res<(Vec<(u64, u64)>, u64)> {
    let fe = img.sector(part + lb)?;
    let tag = tag_id(&fe);
    let (l_ea_o, l_ad_o, ad_o) = match tag {
        261 => (168, 172, 176),
        266 => (208, 212, 216),
        _ => return Err(tr!("UDF-Dateieintrag ungültig", "invalid UDF file entry").into()),
    };
    let flags = le16(&fe, 34) & 7;
    let size = le64(&fe, 56);
    let l_ea = le32(&fe, l_ea_o) as usize;
    let l_ad = le32(&fe, l_ad_o) as usize;
    let start = ad_o + l_ea;
    let mut out = Vec::new();
    if start + l_ad > SECTOR {
        return Err(tr!("UDF-Zuordnung zu lang", "UDF allocation too long").into());
    }
    let step = match flags {
        0 => 8,
        1 => 16,
        _ => return Ok((out, size)),
    };
    let mut have = 0u64;
    for c in fe[start..start + l_ad].chunks_exact(step) {
        let len = ad_len(le32(c, 0), size.saturating_sub(have));
        if len > 0 {
            out.push((part + u64::from(le32(c, 4)), len));
            have += len;
        }
    }
    Ok((out, size))
}

/// Länge aus einem UDF-Zuordnungseintrag. Die oberen zwei Bits sind eigentlich die Art
/// des Bereichs; manche Brennprogramme schreiben dort bei Dateien von genau 1 GiB
/// (VTS_xx_n.VOB) aber die Länge 0x4000_0000 hinein. Dann ist die Länge ohne Maske gemeint.
fn ad_len(raw: u32, remaining: u64) -> u64 {
    let len = u64::from(raw & 0x3FFF_FFFF);
    if len == 0 && raw != 0 && u64::from(raw) <= remaining {
        u64::from(raw)
    } else {
        len
    }
}

#[cfg(test)]
mod ad_tests {
    use super::ad_len;

    #[test]
    fn one_gib_extent_is_not_lost() {
        assert_eq!(ad_len(0x4000_0000, 0x4000_0000), 0x4000_0000);
        assert_eq!(ad_len(1_073_530_880, 1_073_530_880), 1_073_530_880);
        // Echte Art-Bits bei einem kurzen Rest bleiben maskiert.
        assert_eq!(ad_len(0x4000_0800, 0x800), 0x800);
        assert_eq!(ad_len(0x4000_0000, 2048), 0);
    }
}

fn read_dir_udf(img: &mut Img, part: u64, lb: u64) -> Res<Vec<(String, u64, bool)>> {
    let (exts, size) = file_extents(img, part, lb)?;
    let mut data = Vec::new();
    for (start, len) in exts {
        let count = (len as usize).div_ceil(SECTOR);
        data.extend_from_slice(&img.sectors(start, count)?);
    }
    data.truncate((size as usize).min(data.len()));
    let mut out = Vec::new();
    let mut p = 0usize;
    while p + 38 <= data.len() {
        if le16(&data, p) != 257 {
            break;
        }
        let chars = data[p + 18];
        let l_fi = usize::from(data[p + 19]);
        let icb_lb = u64::from(le32(&data, p + 24));
        let l_iu = usize::from(le16(&data, p + 36));
        let name_at = p + 38 + l_iu;
        if name_at + l_fi > data.len() {
            break;
        }
        if chars & 0x0C == 0 && l_fi > 0 {
            let name = decode_cs0(&data[name_at..name_at + l_fi]);
            out.push((name, icb_lb, chars & 2 != 0));
        }
        p += (38 + l_iu + l_fi + 3) & !3;
    }
    Ok(out)
}

fn read_iso9660(img: &mut Img) -> Res<(String, Entries)> {
    let pvd = img.sector(16)?;
    if &pvd[1..6] != b"CD001" {
        return Err(tr!("Die Datei ist kein lesbares DVD-Abbild.", "The file is not a readable DVD image.").into());
    }
    let label = String::from_utf8_lossy(&pvd[40..72]).trim().to_string();
    let root = &pvd[156..190];
    let root_lba = u64::from(le32(root, 2));
    let root_size = le32(root, 10) as usize;
    let root_items = iso9660_dir(img, root_lba, root_size)?;
    let vts = root_items
        .iter()
        .find(|e| e.3 && e.0.eq_ignore_ascii_case("VIDEO_TS"))
        .ok_or(tr!("Im Abbild gibt es keinen Ordner VIDEO_TS.", "The image has no VIDEO_TS folder."))?;
    let items = iso9660_dir(img, vts.1, vts.2 as usize)?;
    // Sehr große Dateien stehen in mehreren Einträgen gleichen Namens; sie liegen am Stück.
    let mut out: Entries = Vec::new();
    for e in items.into_iter().filter(|e| !e.3) {
        let name = e.0.split(';').next().unwrap_or("").to_string();
        match out.last_mut() {
            Some(last) if last.0 == name => last.2 += e.2,
            _ => out.push((name, e.1 * SECTOR as u64, e.2)),
        }
    }
    Ok((label, out))
}

fn iso9660_dir(img: &mut Img, lba: u64, size: usize) -> Res<Vec<(String, u64, u64, bool)>> {
    let data = img.sectors(lba, size.div_ceil(SECTOR).max(1))?;
    let mut out = Vec::new();
    let mut p = 0;
    while p < size {
        let len = usize::from(data[p]);
        if len == 0 {
            p = (p / SECTOR + 1) * SECTOR;
            continue;
        }
        if p + 33 > data.len() || p + len > data.len() {
            break;
        }
        let nlen = usize::from(data[p + 32]);
        let name = String::from_utf8_lossy(&data[p + 33..p + 33 + nlen]).to_string();
        let dir = data[p + 25] & 2 != 0;
        if !(nlen == 1 && (data[p + 33] == 0 || data[p + 33] == 1)) {
            out.push((name, u64::from(le32(&data, p + 2)), u64::from(le32(&data, p + 10)), dir));
        }
        p += len;
    }
    Ok(out)
}

// ---------------------------------------------------------------------------
// Schreiben
// ---------------------------------------------------------------------------

fn crc_ccitt(data: &[u8]) -> u16 {
    let mut crc: u16 = 0;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 { (crc << 1) ^ 0x1021 } else { crc << 1 };
        }
    }
    crc
}

fn set16(d: &mut [u8], o: usize, v: u16) {
    d[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
fn set32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn set64(d: &mut [u8], o: usize, v: u64) {
    d[o..o + 8].copy_from_slice(&v.to_le_bytes());
}

fn finish_tag(d: &mut [u8], id: u16, location: u32, body_len: usize) {
    set16(d, 0, id);
    set16(d, 2, 2);
    set16(d, 6, 1);
    let crc = crc_ccitt(&d[16..16 + body_len]);
    set16(d, 8, crc);
    set16(d, 10, body_len as u16);
    set32(d, 12, location);
    let mut sum = 0u8;
    for (i, b) in d[..16].iter().enumerate() {
        if i != 4 {
            sum = sum.wrapping_add(*b);
        }
    }
    d[4] = sum;
}

fn set_dstring(d: &mut [u8], o: usize, field: usize, text: &str) {
    let bytes: Vec<u8> = text.bytes().take(field - 2).collect();
    d[o] = 8;
    d[o + 1..o + 1 + bytes.len()].copy_from_slice(&bytes);
    d[o + field - 1] = (bytes.len() + 1) as u8;
}

fn set_charspec(d: &mut [u8], o: usize) {
    d[o] = 0;
    let s = b"OSTA Compressed Unicode";
    d[o + 1..o + 1 + s.len()].copy_from_slice(s);
}

fn set_regid(d: &mut [u8], o: usize, name: &[u8], suffix: &[u8]) {
    d[o + 1..o + 1 + name.len()].copy_from_slice(name);
    d[o + 24..o + 24 + suffix.len()].copy_from_slice(suffix);
}

const DOMAIN_SUFFIX: &[u8] = &[0x02, 0x01, 0x03];
const IMPL: &[u8] = b"*DVDmanager";

fn set_timestamp(d: &mut [u8], o: usize, t: &Ymd) {
    // Typ 1 (lokale Zeit), Zeitzone unbekannt
    set16(d, o, 0x1801);
    set16(d, o + 2, t.year);
    d[o + 4] = t.month;
    d[o + 5] = t.day;
    d[o + 6] = t.hour;
    d[o + 7] = t.minute;
    d[o + 8] = t.second;
}

struct Ymd {
    year: u16,
    month: u8,
    day: u8,
    hour: u8,
    minute: u8,
    second: u8,
}

fn now() -> Ymd {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(1_577_836_800);
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    // Kalenderumrechnung nach dem Verfahren von Howard Hinnant
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u8;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u8;
    let year = (yoe + era * 400 + i64::from(month <= 2)) as u16;
    Ymd { year, month, day, hour: (rem / 3600) as u8, minute: ((rem / 60) % 60) as u8, second: (rem % 60) as u8 }
}

/// Lage des ISO-9660-Verzeichnisses VIDEO_TS (frei bis zum Anker bei Sektor 256).
const ISO_VTS_DIR: u32 = 66;
const UDF_MAX_EXTENT: u64 = 0x3FFF_F800;
const ISO_MAX_EXTENT: u64 = 0xFFFF_F800;

struct IsoFile {
    name: String,
    path: PathBuf,
    size: u64,
    start: u32,
    fe_lb: u32,
}

fn sectors_of(size: u64) -> u32 {
    size.div_ceil(SECTOR as u64) as u32
}

fn clean_label(label: &str) -> String {
    let mut s: String = label
        .chars()
        .map(|c| c.to_ascii_uppercase())
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' { c } else { '_' })
        .take(30)
        .collect();
    if s.is_empty() {
        s = "DVD_VIDEO".into();
    }
    s
}

/// Schreibt `video_ts_dir` (fertige, geordnete Dateiliste) als ISO-Abbild.
pub fn write_iso(
    out_path: &Path,
    label: &str,
    files: &[PathBuf],
    mut progress: impl FnMut(u64, u64) -> bool,
) -> Res<()> {
    let label = clean_label(label);
    let t = now();

    let mut list: Vec<IsoFile> = files
        .iter()
        .map(|p| {
            let meta = std::fs::metadata(p).map_err(|e| format!("{}: {e}", p.display()))?;
            Ok(IsoFile {
                name: p.file_name().unwrap_or_default().to_string_lossy().to_ascii_uppercase(),
                path: p.clone(),
                size: meta.len(),
                start: 0,
                fe_lb: 0,
            })
        })
        .collect::<Res<Vec<_>>>()?;

    // Aufbau: 0..255 Systembereich und Beschreibungen, 256 Anker, 257 Partitionsstart.
    const PART: u32 = 257;
    let root_fe: u32 = 2;
    let root_dir: u32 = 3;
    let vts_fe: u32 = 4;
    // Verzeichnisdaten VIDEO_TS: je FID maximal 64 Byte
    let vts_dir: u32 = 5;
    let vts_dir_size_guess = 40 + list.iter().map(|f| ((38 + f.name.len() + 1 + 3) & !3) as u32).sum::<u32>();
    let vts_dir_secs = vts_dir_size_guess.div_ceil(SECTOR as u32).max(1);
    let audio_fe = vts_dir + vts_dir_secs;
    let audio_dir = audio_fe + 1;
    let mut next_lb = audio_dir + 1;
    for f in &mut list {
        f.fe_lb = next_lb;
        next_lb += 1;
    }
    // Daten beginnen auf einer ECC-Grenze (16 Sektoren).
    let mut data_abs = PART + next_lb;
    data_abs = data_abs.div_ceil(16) * 16;
    let first_data = data_abs;
    for f in &mut list {
        f.start = data_abs;
        data_abs += sectors_of(f.size);
    }
    let total = data_abs + 1 + 1; // letzter Anker und Endmarke
    let part_len = total - PART - 1;

    let mut out = super::create_output(out_path)?;
    let write_at = |out: &mut File, sector: u32, data: &[u8]| -> Res<()> {
        out.seek(SeekFrom::Start(u64::from(sector) * SECTOR as u64)).map_err(|e| e.to_string())?;
        out.write_all(data).map_err(|e| trf!("ISO schreiben: {e}", "Writing ISO: {e}"))
    };

    // Vorspann vollständig belegen
    let header_sectors = first_data as usize;
    out.write_all(&vec![0u8; header_sectors * SECTOR]).map_err(|e| trf!("ISO schreiben: {e}", "Writing ISO: {e}"))?;

    // --- ISO 9660 ---
    let mut pvd = vec![0u8; SECTOR];
    pvd[0] = 1;
    pvd[1..6].copy_from_slice(b"CD001");
    pvd[6] = 1;
    fill_a(&mut pvd[8..40], "");
    fill_a(&mut pvd[40..72], &label);
    both32(&mut pvd, 80, total);
    both16(&mut pvd, 120, 1);
    both16(&mut pvd, 124, 1);
    both16(&mut pvd, 128, SECTOR as u16);
    // Pfadtabellen (21, 22), Wurzel (23), VIDEO_TS (24), AUDIO_TS (25)
    let pt_len = 10 + (8 + 8) + (8 + 8);
    both32(&mut pvd, 132, pt_len);
    set32(&mut pvd, 140, 21);
    pvd[148..152].copy_from_slice(&22u32.to_be_bytes());
    let root_rec = iso_record(23, SECTOR as u32, true, &[0], &t);
    pvd[156..156 + 34].copy_from_slice(&root_rec[..34]);
    fill_a(&mut pvd[190..318], "");
    fill_a(&mut pvd[318..446], "");
    fill_a(&mut pvd[446..574], "");
    fill_a(&mut pvd[574..702], "DVDMANAGER");
    fill_a(&mut pvd[702..739], "");
    fill_a(&mut pvd[739..776], "");
    fill_a(&mut pvd[776..813], "");
    let vdate = iso_vol_date(&t);
    pvd[813..830].copy_from_slice(&vdate);
    pvd[830..847].copy_from_slice(&vdate);
    pvd[847..864].copy_from_slice(b"0000000000000000\0");
    pvd[864..881].copy_from_slice(b"0000000000000000\0");
    pvd[881] = 1;
    write_at(&mut out, 16, &pvd)?;
    let mut term = vec![0u8; SECTOR];
    term[0] = 255;
    term[1..6].copy_from_slice(b"CD001");
    term[6] = 1;
    write_at(&mut out, 17, &term)?;
    for (i, id) in [b"BEA01", b"NSR02", b"TEA01"].iter().enumerate() {
        let mut d = vec![0u8; SECTOR];
        d[1..6].copy_from_slice(*id);
        d[6] = 1;
        write_at(&mut out, 18 + i as u32, &d)?;
    }

    // Pfadtabellen
    let mut pt_l = Vec::new();
    let mut pt_m = Vec::new();
    for (name, lba, parent) in [(&[0u8][..], 23u32, 1u16), (b"AUDIO_TS", 25, 1), (b"VIDEO_TS", ISO_VTS_DIR, 1)] {
        pt_l.push(name.len() as u8);
        pt_l.push(0);
        pt_l.extend_from_slice(&lba.to_le_bytes());
        pt_l.extend_from_slice(&parent.to_le_bytes());
        pt_l.extend_from_slice(name);
        if name.len() % 2 == 1 {
            pt_l.push(0);
        }
        pt_m.push(name.len() as u8);
        pt_m.push(0);
        pt_m.extend_from_slice(&lba.to_be_bytes());
        pt_m.extend_from_slice(&parent.to_be_bytes());
        pt_m.extend_from_slice(name);
        if name.len() % 2 == 1 {
            pt_m.push(0);
        }
    }
    write_at(&mut out, 21, &pt_l)?;
    write_at(&mut out, 22, &pt_m)?;

    // Verzeichnisse der Brücke; alphabetisch, wie ISO 9660 es verlangt
    // VIDEO_TS kann bei vielen Titelsets mehrere Sektoren belegen.
    let mut sorted: Vec<&IsoFile> = list.iter().collect();
    sorted.sort_by(|a, b| a.name.cmp(&b.name));
    let mut iso_vts = Vec::new();
    iso_vts.extend_from_slice(&iso_record(ISO_VTS_DIR, SECTOR as u32, true, &[0], &t));
    iso_vts.extend_from_slice(&iso_record(23, SECTOR as u32, true, &[1], &t));
    for f in &sorted {
        let name = format!("{};1", f.name);
        // Über 4 GB braucht ISO 9660 mehrere Einträge mit demselben Namen.
        let mut left = f.size;
        let mut lba = f.start;
        loop {
            let part = left.min(ISO_MAX_EXTENT);
            let mut rec = iso_record(lba, part as u32, false, name.as_bytes(), &t);
            if part < left {
                rec[25] |= 0x80;
            }
            // Verzeichniseinträge dürfen Sektorgrenzen nicht überschreiten
            let used = iso_vts.len() % SECTOR;
            if used + rec.len() > SECTOR {
                iso_vts.resize(iso_vts.len() + (SECTOR - used), 0);
            }
            iso_vts.extend_from_slice(&rec);
            left -= part;
            lba += sectors_of(part);
            if left == 0 {
                break;
            }
        }
    }
    let vts_dir_len = iso_vts.len().div_ceil(SECTOR) * SECTOR;
    if ISO_VTS_DIR as usize + vts_dir_len / SECTOR > 256 {
        return Err(tr!("Zu viele Dateien für das ISO-Verzeichnis", "Too many files for the ISO directory").into());
    }
    let self_rec = iso_record(ISO_VTS_DIR, vts_dir_len as u32, true, &[0], &t);
    iso_vts[..self_rec.len()].copy_from_slice(&self_rec);

    let mut iso_root = Vec::new();
    iso_root.extend_from_slice(&iso_record(23, SECTOR as u32, true, &[0], &t));
    iso_root.extend_from_slice(&iso_record(23, SECTOR as u32, true, &[1], &t));
    iso_root.extend_from_slice(&iso_record(25, SECTOR as u32, true, b"AUDIO_TS", &t));
    iso_root.extend_from_slice(&iso_record(ISO_VTS_DIR, vts_dir_len as u32, true, b"VIDEO_TS", &t));
    write_at(&mut out, 23, &iso_root)?;
    write_at(&mut out, ISO_VTS_DIR, &iso_vts)?;
    let mut iso_audio = Vec::new();
    iso_audio.extend_from_slice(&iso_record(25, SECTOR as u32, true, &[0], &t));
    iso_audio.extend_from_slice(&iso_record(23, SECTOR as u32, true, &[1], &t));
    write_at(&mut out, 25, &iso_audio)?;

    // --- UDF: Volume-Beschreibung ---
    let write_vds = |out: &mut File, base: u32| -> Res<()> {
        // Primär
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 1);
        set_dstring(&mut d, 24, 32, &label);
        set16(&mut d, 56, 1);
        set16(&mut d, 58, 1);
        set16(&mut d, 60, 2);
        set16(&mut d, 62, 2);
        set32(&mut d, 64, 1);
        set32(&mut d, 68, 1);
        set_dstring(&mut d, 72, 128, &format!("{:016X}{}", u64::from(total) << 16 | 0xD15C, label));
        set_charspec(&mut d, 200);
        set_charspec(&mut d, 264);
        set_regid(&mut d, 344, b"", &[]);
        set_timestamp(&mut d, 376, &t);
        set_regid(&mut d, 388, IMPL, &[]);
        set16(&mut d, 488, 1);
        finish_tag(&mut d, 1, base, 512 - 16);
        write_at(out, base, &d)?;
        // Implementierung
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 2);
        set_regid(&mut d, 20, b"*UDF LV Info", DOMAIN_SUFFIX);
        set_charspec(&mut d, 52);
        set_dstring(&mut d, 116, 128, &label);
        set_regid(&mut d, 352, IMPL, &[]);
        finish_tag(&mut d, 4, base + 1, 512 - 16);
        write_at(out, base + 1, &d)?;
        // Partition
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 3);
        set16(&mut d, 20, 1);
        set16(&mut d, 22, 0);
        set_regid(&mut d, 24, b"+NSR02", &[]);
        set32(&mut d, 184, 1);
        set32(&mut d, 188, PART);
        set32(&mut d, 192, part_len);
        set_regid(&mut d, 196, IMPL, &[]);
        finish_tag(&mut d, 5, base + 2, 512 - 16);
        write_at(out, base + 2, &d)?;
        // Logisches Volume
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 4);
        set_charspec(&mut d, 20);
        set_dstring(&mut d, 84, 128, &label);
        set32(&mut d, 212, SECTOR as u32);
        set_regid(&mut d, 216, b"*OSTA UDF Compliant", DOMAIN_SUFFIX);
        set32(&mut d, 248, SECTOR as u32);
        set32(&mut d, 252, 0);
        set16(&mut d, 256, 0);
        set32(&mut d, 264, 6);
        set32(&mut d, 268, 1);
        set_regid(&mut d, 272, IMPL, &[]);
        set32(&mut d, 432, 2 * SECTOR as u32);
        set32(&mut d, 436, 64);
        d[440] = 1;
        d[441] = 6;
        set16(&mut d, 442, 1);
        set16(&mut d, 444, 0);
        finish_tag(&mut d, 6, base + 3, 446 - 16);
        write_at(out, base + 3, &d)?;
        // Nicht belegter Platz
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 5);
        finish_tag(&mut d, 7, base + 4, 24 - 16);
        write_at(out, base + 4, &d)?;
        // Ende
        let mut d = vec![0u8; SECTOR];
        finish_tag(&mut d, 8, base + 5, 512 - 16);
        write_at(out, base + 5, &d)
    };
    write_vds(&mut out, 32)?;
    write_vds(&mut out, 48)?;

    // Integritätssequenz
    let mut d = vec![0u8; SECTOR];
    set_timestamp(&mut d, 16, &t);
    set32(&mut d, 28, 1);
    set64(&mut d, 40, 16 + list.len() as u64 + 3);
    set32(&mut d, 72, 1);
    set32(&mut d, 76, 46);
    set32(&mut d, 80, 0);
    set32(&mut d, 84, part_len);
    set_regid(&mut d, 88, IMPL, &[]);
    set32(&mut d, 120, list.len() as u32);
    set32(&mut d, 124, 3);
    set16(&mut d, 128, 0x0102);
    set16(&mut d, 130, 0x0102);
    set16(&mut d, 132, 0x0102);
    finish_tag(&mut d, 9, 64, 134 - 16);
    write_at(&mut out, 64, &d)?;
    let mut d = vec![0u8; SECTOR];
    finish_tag(&mut d, 8, 65, 512 - 16);
    write_at(&mut out, 65, &d)?;

    // Anker
    let anchor = |loc: u32| -> Vec<u8> {
        let mut d = vec![0u8; SECTOR];
        set32(&mut d, 16, 16 * SECTOR as u32);
        set32(&mut d, 20, 32);
        set32(&mut d, 24, 16 * SECTOR as u32);
        set32(&mut d, 28, 48);
        finish_tag(&mut d, 2, loc, 512 - 16);
        d
    };
    write_at(&mut out, 256, &anchor(256))?;

    // Dateisatz
    let mut d = vec![0u8; SECTOR];
    set_timestamp(&mut d, 16, &t);
    set16(&mut d, 28, 3);
    set16(&mut d, 30, 3);
    set32(&mut d, 32, 1);
    set32(&mut d, 36, 1);
    set_charspec(&mut d, 48);
    set_dstring(&mut d, 112, 128, &label);
    set_charspec(&mut d, 240);
    set_dstring(&mut d, 304, 32, &label);
    set32(&mut d, 400, SECTOR as u32);
    set32(&mut d, 404, root_fe);
    set_regid(&mut d, 416, b"*OSTA UDF Compliant", DOMAIN_SUFFIX);
    finish_tag(&mut d, 256, 0, 512 - 16);
    write_at(&mut out, PART, &d)?;
    let mut d = vec![0u8; SECTOR];
    finish_tag(&mut d, 8, 1, 512 - 16);
    write_at(&mut out, PART + 1, &d)?;

    // Wurzel: VIDEO_TS und AUDIO_TS
    let mut dir_root = Vec::new();
    push_fid(&mut dir_root, "", 0x0A, root_fe, root_dir);
    push_fid(&mut dir_root, "AUDIO_TS", 0x02, audio_fe, root_dir);
    push_fid(&mut dir_root, "VIDEO_TS", 0x02, vts_fe, root_dir);
    write_at(&mut out, PART + root_dir, &dir_root)?;
    write_at(&mut out, PART + root_fe, &file_entry(root_fe, true, 2 + 2, dir_root.len() as u64, &[(PART + root_dir, dir_root.len() as u64)], 1, &t))?;

    // VIDEO_TS
    let mut dir_vts = Vec::new();
    push_fid(&mut dir_vts, "", 0x0A, root_fe, vts_dir);
    for f in &list {
        let lb = vts_dir + (dir_vts.len() / SECTOR) as u32;
        push_fid(&mut dir_vts, &f.name, 0x00, f.fe_lb, lb);
    }
    if dir_vts.len() > vts_dir_secs as usize * SECTOR {
        return Err(tr!("Interner Fehler: UDF-Verzeichnis zu groß", "Internal error: UDF directory too large").into());
    }
    write_at(&mut out, PART + vts_dir, &dir_vts)?;
    write_at(&mut out, PART + vts_fe, &file_entry(vts_fe, true, 2, dir_vts.len() as u64, &[(PART + vts_dir, dir_vts.len() as u64)], 2, &t))?;

    // AUDIO_TS (leer)
    let mut dir_audio = Vec::new();
    push_fid(&mut dir_audio, "", 0x0A, root_fe, audio_dir);
    write_at(&mut out, PART + audio_dir, &dir_audio)?;
    write_at(&mut out, PART + audio_fe, &file_entry(audio_fe, true, 2, dir_audio.len() as u64, &[(PART + audio_dir, dir_audio.len() as u64)], 3, &t))?;

    // Dateieinträge
    for (i, f) in list.iter().enumerate() {
        // Ein UDF-Abschnitt fasst knapp 1 GB; längere Dateien bestehen aus mehreren.
        let mut extents = Vec::new();
        let (mut start, mut left) = (f.start, f.size);
        while left > 0 {
            let part = left.min(UDF_MAX_EXTENT);
            extents.push((start, part));
            start += sectors_of(part);
            left -= part;
        }
        if extents.is_empty() {
            extents.push((f.start, 0));
        }
        if 176 + extents.len() * 8 > SECTOR {
            return Err(trf!("{} ist für ein ISO-Abbild zu groß.", "{} is too large for an ISO image.", f.name));
        }
        let fe = file_entry(f.fe_lb, false, 1, f.size, &extents, 16 + i as u64, &t);
        write_at(&mut out, PART + f.fe_lb, &fe)?;
    }

    // Dateiinhalte
    let total_bytes: u64 = list.iter().map(|f| f.size).sum();
    let mut done = 0u64;
    let mut buf = vec![0u8; 1 << 20];
    for f in &list {
        out.seek(SeekFrom::Start(u64::from(f.start) * SECTOR as u64)).map_err(|e| e.to_string())?;
        let mut src = File::open(&f.path).map_err(|e| format!("{}: {e}", f.path.display()))?;
        loop {
            let n = src.read(&mut buf).map_err(|e| e.to_string())?;
            if n == 0 {
                break;
            }
            out.write_all(&buf[..n]).map_err(|e| trf!("ISO schreiben: {e}", "Writing ISO: {e}"))?;
            done += n as u64;
            if !progress(done, total_bytes) {
                return Err(tr!("Abgebrochen", "Cancelled").into());
            }
        }
    }
    write_at(&mut out, data_abs, &anchor(data_abs))?;
    write_at(&mut out, data_abs + 1, &anchor(data_abs + 1))?;
    out.flush().map_err(|e| e.to_string())?;
    Ok(())
}

fn fill_a(d: &mut [u8], text: &str) {
    d.fill(b' ');
    let b = text.as_bytes();
    let n = b.len().min(d.len());
    d[..n].copy_from_slice(&b[..n]);
}

fn both32(d: &mut [u8], o: usize, v: u32) {
    d[o..o + 4].copy_from_slice(&v.to_le_bytes());
    d[o + 4..o + 8].copy_from_slice(&v.to_be_bytes());
}

fn both16(d: &mut [u8], o: usize, v: u16) {
    d[o..o + 2].copy_from_slice(&v.to_le_bytes());
    d[o + 2..o + 4].copy_from_slice(&v.to_be_bytes());
}

fn iso_vol_date(t: &Ymd) -> [u8; 17] {
    let s = format!(
        "{:04}{:02}{:02}{:02}{:02}{:02}00",
        t.year, t.month, t.day, t.hour, t.minute, t.second
    );
    let mut a = [0u8; 17];
    a[..16].copy_from_slice(s.as_bytes());
    a
}

fn iso_record(lba: u32, size: u32, dir: bool, name: &[u8], t: &Ymd) -> Vec<u8> {
    let mut len = 33 + name.len();
    if len % 2 == 1 {
        len += 1;
    }
    let mut r = vec![0u8; len];
    r[0] = len as u8;
    r[2..6].copy_from_slice(&lba.to_le_bytes());
    r[6..10].copy_from_slice(&lba.to_be_bytes());
    r[10..14].copy_from_slice(&size.to_le_bytes());
    r[14..18].copy_from_slice(&size.to_be_bytes());
    r[18] = (t.year.saturating_sub(1900)) as u8;
    r[19] = t.month;
    r[20] = t.day;
    r[21] = t.hour;
    r[22] = t.minute;
    r[23] = t.second;
    r[25] = if dir { 2 } else { 0 };
    r[28..30].copy_from_slice(&1u16.to_le_bytes());
    r[30..32].copy_from_slice(&1u16.to_be_bytes());
    r[32] = name.len() as u8;
    r[33..33 + name.len()].copy_from_slice(name);
    r
}

fn push_fid(out: &mut Vec<u8>, name: &str, chars: u8, icb_lb: u32, location: u32) {
    let id: Vec<u8> = if name.is_empty() {
        Vec::new()
    } else {
        std::iter::once(8u8).chain(name.bytes()).collect()
    };
    let len = (38 + id.len() + 3) & !3;
    let mut d = vec![0u8; len];
    set16(&mut d, 16, 1);
    d[18] = chars;
    d[19] = id.len() as u8;
    set32(&mut d, 20, SECTOR as u32);
    set32(&mut d, 24, icb_lb);
    d[38..38 + id.len()].copy_from_slice(&id);
    finish_tag(&mut d, 257, location, len - 16);
    out.extend_from_slice(&d);
}

fn file_entry(
    location: u32,
    dir: bool,
    links: u16,
    size: u64,
    extents: &[(u32, u64)],
    unique: u64,
    t: &Ymd,
) -> Vec<u8> {
    let mut d = vec![0u8; SECTOR];
    // ICB-Etikett
    set16(&mut d, 16 + 4, 4);
    set16(&mut d, 16 + 8, 1);
    d[16 + 11] = if dir { 4 } else { 5 };
    set16(&mut d, 16 + 18, 0); // kurze Zuordnungen
    set32(&mut d, 36, u32::MAX);
    set32(&mut d, 40, u32::MAX);
    set32(&mut d, 44, 0x0000_14A5 | 0x0000_0421);
    set16(&mut d, 48, links);
    set64(&mut d, 56, size);
    let blocks: u64 = extents.iter().map(|e| e.1.div_ceil(SECTOR as u64)).sum();
    set64(&mut d, 64, blocks);
    set_timestamp(&mut d, 72, t);
    set_timestamp(&mut d, 84, t);
    set_timestamp(&mut d, 96, t);
    set32(&mut d, 108, 1);
    set_regid(&mut d, 128, IMPL, &[]);
    set64(&mut d, 160, unique);
    set32(&mut d, 168, 0);
    let mut o = 176usize;
    for (start, len) in extents {
        set32(&mut d, o, *len as u32);
        set32(&mut d, o + 4, start - 257);
        o += 8;
    }
    set32(&mut d, 172, (o - 176) as u32);
    finish_tag(&mut d, 261, location, o - 16);
    d
}
