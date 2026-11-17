//! Lesezugriff auf eine DVD-Struktur: Ordner, Laufwerk oder ISO-Abbild.
//! Mehrteilige VOB-Dateien erscheinen als ein durchgehender Sektorraum.

use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::{iso, Res, SECTOR};

#[derive(Clone, Debug)]
pub enum FileRef {
    Disk(PathBuf),
    Iso { path: PathBuf, offset: u64, len: u64 },
}

impl FileRef {
    pub fn len(&self) -> u64 {
        match self {
            FileRef::Disk(p) => std::fs::metadata(p).map(|m| m.len()).unwrap_or(0),
            FileRef::Iso { len, .. } => *len,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn open(&self) -> Res<(File, u64)> {
        match self {
            FileRef::Disk(p) => {
                let f = File::open(p).map_err(|e| trf!("{} kann nicht geöffnet werden: {e}", "{} cannot be opened: {e}", p.display()))?;
                Ok((f, 0))
            }
            FileRef::Iso { path, offset, .. } => {
                let f = File::open(path).map_err(|e| trf!("{} kann nicht geöffnet werden: {e}", "{} cannot be opened: {e}", path.display()))?;
                Ok((f, *offset))
            }
        }
    }
}

#[derive(Clone)]
pub struct Source {
    pub label: String,
    pub origin: PathBuf,
    pub files: Arc<HashMap<String, FileRef>>,
    /// Offene Griffe, die die Quelldateien gegen Löschen und Verschieben sperren,
    /// solange irgendetwas diese Quelle noch benutzt.
    pub locks: Arc<Vec<File>>,
    /// Alle geöffneten Pfade des Benutzers; mehrere bei zusammen geöffneten Videodateien.
    /// Der erste ist `origin`.
    pub origins: Vec<PathBuf>,
    /// Namen der Titel aus den Dateinamen (zusammen geöffnete Videodateien), sonst leer
    pub title_names: Vec<String>,
}

/// Welcher VOB-Bereich gelesen wird. `vts` 0 ist der Hauptmenü-Bereich der Disc.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Space {
    pub vts: u8,
    pub menu: bool,
}

impl Space {
    pub fn title(vts: u8) -> Self {
        Self { vts, menu: false }
    }
    pub fn menu(vts: u8) -> Self {
        Self { vts, menu: true }
    }
}

impl Source {
    /// Öffnet einen Ordner, ein Laufwerk oder eine ISO-Datei.
    pub fn open(path: &Path) -> Res<Source> {
        if path.is_file() {
            let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_ascii_lowercase();
            if ext == "ifo" || ext == "vob" || ext == "bup" {
                if let Some(parent) = path.parent() {
                    return Self::open_dir(parent);
                }
            }
            return Self::open_iso(path);
        }
        Self::open_dir(path)
    }

    fn open_iso(path: &Path) -> Res<Source> {
        let (label, entries) = iso::read_video_ts(path)?;
        let mut files = HashMap::new();
        for (name, offset, len) in entries {
            files.insert(name.to_ascii_uppercase(), FileRef::Iso { path: path.to_path_buf(), offset, len });
        }
        if !files.contains_key("VIDEO_TS.IFO") {
            return Err(tr!("Im ISO-Abbild gibt es keine VIDEO_TS.IFO. Es ist wohl keine Video-DVD.", "The ISO image has no VIDEO_TS.IFO. It is probably not a video DVD.").into());
        }
        let label = if label.trim().is_empty() { file_label(path) } else { label };
        Ok(Source { label, origin: path.to_path_buf(), files: Arc::new(files), locks: Arc::default(), origins: vec![path.to_path_buf()], title_names: Vec::new() })
    }

    fn open_dir(path: &Path) -> Res<Source> {
        let dir = find_video_ts_dir(path).ok_or_else(|| {
            trf!("In „{}“ wurde keine DVD-Struktur gefunden (VIDEO_TS.IFO fehlt).", "No DVD structure was found in \"{}\" (VIDEO_TS.IFO is missing).", path.display())
        })?;
        let mut files = HashMap::new();
        let rd = std::fs::read_dir(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        for ent in rd.flatten() {
            let name = ent.file_name().to_string_lossy().to_ascii_uppercase();
            if ent.path().is_file() {
                files.insert(name, FileRef::Disk(ent.path()));
            }
        }
        if !files.contains_key("VIDEO_TS.IFO") {
            return Err(trf!("In „{}“ fehlt die Datei VIDEO_TS.IFO.", "The file VIDEO_TS.IFO is missing in \"{}\".", dir.display()));
        }
        let base = if dir.file_name().map(|n| n.eq_ignore_ascii_case("VIDEO_TS")).unwrap_or(false) {
            dir.parent().unwrap_or(&dir).to_path_buf()
        } else {
            dir.clone()
        };
        Ok(Source {
            label: file_label(&base),
            origin: path.to_path_buf(),
            files: Arc::new(files),
            locks: Arc::default(),
            origins: vec![path.to_path_buf()],
            title_names: Vec::new(),
        })
    }

    /// Sperrt die Dateien dieser Quelle (ISO bzw. alle Dateien in VIDEO_TS) gegen Löschen,
    /// Umbenennen und Verschieben, z. B. im Explorer. Lesen und Schreiben bleiben möglich.
    /// Auf einem optischen Laufwerk nicht, damit sich die Scheibe auswerfen lässt.
    pub fn lock(&mut self) {
        if is_optical(&self.origin) {
            return;
        }
        let mut paths: Vec<&Path> = self
            .files
            .values()
            .map(|f| match f {
                FileRef::Disk(p) => p.as_path(),
                FileRef::Iso { path, .. } => path.as_path(),
            })
            .collect();
        paths.sort();
        paths.dedup();
        self.locks = Arc::new(paths.into_iter().filter_map(lock_file).collect());
    }

    /// Liest eine ganze Datei; fällt bei IFO-Dateien auf die Sicherung (BUP) zurück.
    pub fn read_file(&self, name: &str) -> Res<Vec<u8>> {
        match self.read_exact_file(name) {
            Ok(v) => Ok(v),
            Err(first) => {
                if let Some(stem) = name.strip_suffix(".IFO") {
                    let bup = format!("{stem}.BUP");
                    if let Ok(v) = self.read_exact_file(&bup) {
                        return Ok(v);
                    }
                }
                Err(first)
            }
        }
    }

    fn read_exact_file(&self, name: &str) -> Res<Vec<u8>> {
        let r = self.files.get(name).ok_or_else(|| trf!("{name} fehlt", "{name} is missing"))?;
        let (mut f, off) = r.open()?;
        f.seek(SeekFrom::Start(off)).map_err(|e| format!("{name}: {e}"))?;
        let mut v = vec![0u8; r.len() as usize];
        f.read_exact(&mut v).map_err(|e| format!("{name}: {e}"))?;
        Ok(v)
    }

    pub fn has_vts(&self, vts: u8) -> bool {
        self.files.contains_key(&format!("VTS_{vts:02}_0.IFO"))
            || self.files.contains_key(&format!("VTS_{vts:02}_0.BUP"))
    }

    pub fn ifo_name(vts: u8) -> String {
        if vts == 0 {
            "VIDEO_TS.IFO".into()
        } else {
            format!("VTS_{vts:02}_0.IFO")
        }
    }

    pub fn reader(&self, space: Space) -> Res<SpaceReader> {
        let mut parts = Vec::new();
        if space.vts == 0 {
            if let Some(r) = self.files.get("VIDEO_TS.VOB") {
                parts.push((r.clone(), 0, r.len() / SECTOR as u64));
            }
        } else if space.menu {
            if let Some(r) = self.files.get(&format!("VTS_{:02}_0.VOB", space.vts)) {
                parts.push((r.clone(), 0, r.len() / SECTOR as u64));
            }
        } else {
            let mut base = 0u64;
            for i in 1..=9 {
                if let Some(r) = self.files.get(&format!("VTS_{:02}_{}.VOB", space.vts, i)) {
                    let n = r.len() / SECTOR as u64;
                    parts.push((r.clone(), base, n));
                    base += n;
                }
            }
        }
        let mut offs = Vec::with_capacity(parts.len());
        let mut acc = 0u64;
        let mut refs = Vec::with_capacity(parts.len());
        for (r, _, n) in parts {
            offs.push((acc, n));
            acc += n;
            refs.push(r);
        }
        Ok(SpaceReader { parts: refs, offs, total: acc, open: None })
    }
}

/// Öffnet eine Datei so, dass andere sie lesen und beschreiben, aber nicht löschen,
/// umbenennen oder verschieben können, solange der Griff offen ist.
pub fn lock_file(path: &Path) -> Option<File> {
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        const FILE_SHARE_WRITE: u32 = 2;
        std::fs::OpenOptions::new().read(true).share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE).open(path).ok()
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(all(test, windows))]
mod lock_tests {
    #[test]
    fn locked_file_cannot_be_deleted_or_moved_but_rewritten() {
        let dir = std::env::temp_dir().join(format!("dvdmanager-sperre-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let p = dir.join("projekt.dvdproj");
        std::fs::write(&p, b"alt").unwrap();
        let lock = super::lock_file(&p).expect("Sperre");
        assert!(std::fs::remove_file(&p).is_err(), "ließ sich löschen");
        assert!(std::fs::rename(&p, dir.join("weg.dvdproj")).is_err(), "ließ sich verschieben");
        assert!(std::fs::rename(&dir, dir.with_extension("weg")).is_err(), "Ordner ließ sich verschieben");
        crate::dvd::write_output(&p, b"neu").expect("Speichern trotz Sperre");
        assert_eq!(std::fs::read(&p).unwrap(), b"neu");
        drop(lock);
        std::fs::remove_file(&p).expect("nach dem Freigeben löschbar");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

fn file_label(path: &Path) -> String {
    path.file_stem()
        .or_else(|| path.file_name())
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| path.display().to_string())
}

fn find_video_ts_dir(path: &Path) -> Option<PathBuf> {
    let direct_ifo = |d: &Path| -> bool {
        std::fs::read_dir(d)
            .map(|rd| rd.flatten().any(|e| e.file_name().eq_ignore_ascii_case("VIDEO_TS.IFO")))
            .unwrap_or(false)
    };
    if direct_ifo(path) {
        return Some(path.to_path_buf());
    }
    let rd = std::fs::read_dir(path).ok()?;
    for ent in rd.flatten() {
        if ent.file_name().eq_ignore_ascii_case("VIDEO_TS") && ent.path().is_dir() && direct_ifo(&ent.path()) {
            return Some(ent.path());
        }
    }
    None
}

/// Liest Sektoren aus einem zusammenhängenden VOB-Bereich.
pub struct SpaceReader {
    parts: Vec<FileRef>,
    offs: Vec<(u64, u64)>,
    pub total: u64,
    open: Option<(usize, File, u64)>,
}

impl SpaceReader {
    pub fn sectors(&self) -> u64 {
        self.total
    }

    /// Liest `count` Sektoren ab `sector`. Liefert die tatsächlich gelesene Anzahl.
    pub fn read(&mut self, sector: u64, count: usize, buf: &mut Vec<u8>) -> Res<usize> {
        buf.clear();
        buf.resize(count * SECTOR, 0);
        let mut done = 0usize;
        let mut sec = sector;
        while done < count && sec < self.total {
            let idx = self.offs.partition_point(|o| o.0 <= sec) - 1;
            let (start, n) = self.offs[idx];
            let in_part = sec - start;
            let take = ((n - in_part) as usize).min(count - done);
            if self.open.as_ref().map(|o| o.0) != Some(idx) {
                let (f, base) = self.parts[idx].open()?;
                self.open = Some((idx, f, base));
            }
            let (_, f, base) = self.open.as_mut().unwrap();
            f.seek(SeekFrom::Start(*base + in_part * SECTOR as u64)).map_err(|e| e.to_string())?;
            let slice = &mut buf[done * SECTOR..(done + take) * SECTOR];
            f.read_exact(slice).map_err(|e| {
                trf!(
                    "Lesefehler bei Sektor {sec}: {e}. Falls das eine Kauf-DVD ist, verhindert vermutlich der Kopierschutz das Lesen.", "Read error at sector {sec}: {e}. If this is a commercial DVD, copy protection is probably preventing it from being read."
                )
            })?;
            done += take;
            sec += take as u64;
        }
        buf.truncate(done * SECTOR);
        Ok(done)
    }
}

/// Liegt der Pfad auf einem optischen Laufwerk (eingelegte DVD)?
pub fn is_optical(path: &Path) -> bool {
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDriveTypeW(path: *const u16) -> u32;
        }
        const DRIVE_CDROM: u32 = 5;
        let text = path.display().to_string();
        let text = text.strip_prefix(r"\\?\").unwrap_or(&text);
        let mut chars = text.chars();
        let (Some(letter), Some(':')) = (chars.next(), chars.next()) else { return false };
        if !letter.is_ascii_alphabetic() {
            return false;
        }
        let root = format!("{letter}:\\");
        let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
        unsafe { GetDriveTypeW(wide.as_ptr()) == DRIVE_CDROM }
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        false
    }
}

/// Windows-Laufwerke, in denen gerade eine Video-DVD liegt.
pub fn optical_drives_with_video() -> Vec<(String, PathBuf, bool)> {
    let mut out = Vec::new();
    #[cfg(windows)]
    {
        #[link(name = "kernel32")]
        extern "system" {
            fn GetDriveTypeW(path: *const u16) -> u32;
            fn GetLogicalDrives() -> u32;
        }
        const DRIVE_CDROM: u32 = 5;
        let mask = unsafe { GetLogicalDrives() };
        for i in 0..26u32 {
            if mask & (1 << i) == 0 {
                continue;
            }
            let letter = (b'A' + i as u8) as char;
            let root = format!("{letter}:\\");
            let wide: Vec<u16> = root.encode_utf16().chain(std::iter::once(0)).collect();
            let kind = unsafe { GetDriveTypeW(wide.as_ptr()) };
            if kind == DRIVE_CDROM {
                let path = PathBuf::from(&root);
                let has_video = path.join("VIDEO_TS").join("VIDEO_TS.IFO").exists();
                out.push((root, path, has_video));
            }
        }
    }
    out
}
