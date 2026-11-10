//! Versionsnummer und Änderungsliste.
//!
//! Die Nummer kommt aus `Cargo.toml` (z. B. 1.0.1) und wird als 1.0.001 angezeigt:
//! Bei jeder Änderung zählt die letzte Stelle hoch. Die Änderungsliste steht in
//! `CHANGELOG.md` und ist in die Exe eingebaut.

/// Versionsnummer zur Anzeige, z. B. "1.0.001".
pub fn version() -> String {
    format!(
        "{}.{}.{:03}",
        env!("CARGO_PKG_VERSION_MAJOR"),
        env!("CARGO_PKG_VERSION_MINOR"),
        env!("CARGO_PKG_VERSION_PATCH").parse::<u32>().unwrap_or(0)
    )
}

/// Inhalt von CHANGELOG.md.
pub const CHANGELOG: &str = include_str!("../CHANGELOG.md");

/// Ein Eintrag der Änderungsliste.
#[derive(Debug, Clone, PartialEq)]
pub struct Release {
    pub version: String,
    /// Datum und Uhrzeit, wie in der Liste angegeben
    pub date: String,
    /// Einleitender Text (ohne Aufzählungszeichen)
    pub intro: Vec<String>,
    pub items: Vec<String>,
}

/// Liest die Änderungsliste: Überschriften "## <Version> · <Datum>", darunter
/// Text und Aufzählungspunkte "- ...".
pub fn releases() -> Vec<Release> {
    let mut out: Vec<Release> = Vec::new();
    for line in CHANGELOG.lines() {
        let line = line.trim_end();
        if let Some(h) = line.strip_prefix("## ") {
            let (v, d) = h.split_once('·').unwrap_or((h, ""));
            out.push(Release { version: v.trim().to_string(), date: d.trim().to_string(), intro: Vec::new(), items: Vec::new() });
        } else if let Some(r) = out.last_mut() {
            if let Some(item) = line.strip_prefix("- ") {
                r.items.push(item.trim().to_string());
            } else if !line.trim().is_empty() {
                r.intro.push(line.trim().to_string());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn changelog_matches_version() {
        let r = releases();
        assert!(!r.is_empty(), "Änderungsliste leer");
        assert_eq!(r[0].version, version(), "Oberster Eintrag in CHANGELOG.md passt nicht zur Version in Cargo.toml");
        assert!(!r[0].date.is_empty() && !r[0].items.is_empty());
    }
}
