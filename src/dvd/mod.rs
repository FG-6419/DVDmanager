//! DVD-Strukturen: Quelle (Ordner, Laufwerk, ISO), IFO-Tabellen, VOB-Pakete.

pub mod ifo;
pub mod iso;
pub mod model;
pub mod source;
pub mod vob;

pub const SECTOR: usize = 2048;

pub type Res<T> = Result<T, String>;

/// Legt eine Ausgabedatei neu an. Überschreibt man eine vorhandene Datei, behält
/// Windows sonst ihr altes Erstelldatum, und der Explorer zeigt die alte Uhrzeit.
pub fn create_output(path: &std::path::Path) -> Res<std::fs::File> {
    // Gibt es die Datei schon mit anderer Groß-/Kleinschreibung (z. B. film.vob statt
    // film.VOB), behielte Windows beim Überschreiben den alten Namen. Dann vorher löschen.
    if let (Some(dir), Some(name)) = (path.parent(), path.file_name()) {
        let dir = if dir.as_os_str().is_empty() { std::path::Path::new(".") } else { dir };
        let want = name.to_string_lossy();
        if let Ok(rd) = std::fs::read_dir(dir) {
            for e in rd.flatten() {
                let have = e.file_name().to_string_lossy().to_string();
                if have != want && have.to_lowercase() == want.to_lowercase() {
                    let _ = std::fs::remove_file(e.path());
                }
            }
        }
    }
    let f = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let now = std::time::SystemTime::now();
    let times = std::fs::FileTimes::new().set_modified(now).set_accessed(now);
    #[cfg(windows)]
    let times = {
        use std::os::windows::fs::FileTimesExt;
        times.set_created(now)
    };
    let _ = f.set_times(times);
    Ok(f)
}

#[cfg(all(test, windows))]
mod output_tests {
    use std::time::{Duration, SystemTime};

    #[test]
    fn overwrite_renews_creation_time() {
        let p = std::env::temp_dir().join(format!("dvdmanager-zeit-{}.bin", std::process::id()));
        std::fs::write(&p, b"alt").unwrap();
        let old = SystemTime::now() - Duration::from_secs(3600);
        {
            use std::os::windows::fs::FileTimesExt;
            let f = std::fs::OpenOptions::new().write(true).open(&p).unwrap();
            f.set_times(std::fs::FileTimes::new().set_created(old).set_modified(old)).unwrap();
        }
        super::write_output(&p, b"neu").unwrap();
        let m = std::fs::metadata(&p).unwrap();
        let _ = std::fs::remove_file(&p);
        let fresh = |t: SystemTime| SystemTime::now().duration_since(t).unwrap_or_default() < Duration::from_secs(60);
        assert!(fresh(m.created().unwrap()), "Erstelldatum blieb alt");
        assert!(fresh(m.modified().unwrap()), "Änderungsdatum blieb alt");
    }

    /// Überschreiben mit anderer Groß-/Kleinschreibung: Der neue Name gilt.
    #[test]
    fn overwrite_takes_new_case() {
        let dir = std::env::temp_dir().join(format!("dvdmanager-endung-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(dir.join("film.vob"), b"alt").unwrap();
        super::write_output(&dir.join("film.VOB"), b"neu").unwrap();
        let names: Vec<String> = std::fs::read_dir(&dir).unwrap().flatten().map(|e| e.file_name().to_string_lossy().to_string()).collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(names, vec!["film.VOB".to_string()]);
    }
}

/// Schreibt eine Ausgabedatei vollständig, mit frischem Erstelldatum.
pub fn write_output(path: &std::path::Path, data: &[u8]) -> Res<()> {
    use std::io::Write;
    create_output(path)?.write_all(data).map_err(|e| format!("{}: {e}", path.display()))
}

#[inline]
pub fn be16(d: &[u8], o: usize) -> u16 {
    match d.get(o..o + 2) {
        Some(b) => u16::from_be_bytes([b[0], b[1]]),
        None => 0,
    }
}

#[inline]
pub fn be32(d: &[u8], o: usize) -> u32 {
    match d.get(o..o + 4) {
        Some(b) => u32::from_be_bytes([b[0], b[1], b[2], b[3]]),
        None => 0,
    }
}

#[inline]
pub fn put16(d: &mut [u8], o: usize, v: u16) {
    if let Some(b) = d.get_mut(o..o + 2) {
        b.copy_from_slice(&v.to_be_bytes());
    }
}

#[inline]
pub fn put32(d: &mut [u8], o: usize, v: u32) {
    if let Some(b) = d.get_mut(o..o + 4) {
        b.copy_from_slice(&v.to_be_bytes());
    }
}
