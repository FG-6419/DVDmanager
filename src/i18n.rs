//! Sprache der Oberfläche: Deutsch oder Englisch.
//!
//! Alle Texte sind fest in die Exe eingebaut. Jeder Text steht im Code als Paar aus
//! deutscher und englischer Fassung:
//!
//! - `tr!("Abbrechen", "Cancel")` liefert einen `&'static str`,
//! - `trf!("{n} Kapitel", "{n} chapters")` arbeitet wie `format!` und liefert einen `String`.
//!
//! Der Compiler prüft dabei beide Fassungen, auch dass die Platzhalter zusammenpassen.
//! Eine fehlende Übersetzung kann es so nicht geben.

use std::sync::atomic::{AtomicU8, Ordering};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Lang {
    De,
    En,
}

impl Lang {
    /// Kürzel zum Speichern in den Einstellungen.
    pub fn code(self) -> &'static str {
        match self {
            Lang::De => "de",
            Lang::En => "en",
        }
    }

    pub fn from_code(code: &str) -> Option<Lang> {
        match code {
            "de" => Some(Lang::De),
            "en" => Some(Lang::En),
            _ => None,
        }
    }
}

static LANG: AtomicU8 = AtomicU8::new(0);

/// Stellt die Sprache für alle folgenden Texte ein (auch in Hintergrund-Threads).
pub fn set_lang(lang: Lang) {
    LANG.store(if lang == Lang::En { 1 } else { 0 }, Ordering::Relaxed);
}

pub fn lang() -> Lang {
    if LANG.load(Ordering::Relaxed) == 1 { Lang::En } else { Lang::De }
}

#[inline]
pub fn is_en() -> bool {
    LANG.load(Ordering::Relaxed) == 1
}

/// Sprache von Windows: Deutsch, wenn Windows auf Deutsch läuft, sonst Englisch.
pub fn system_lang() -> Lang {
    #[cfg(windows)]
    {
        // Untere 10 Bit der Sprachkennung: 0x07 = Deutsch
        let id = unsafe { windows::Win32::Globalization::GetUserDefaultUILanguage() };
        if id & 0x3FF == 0x07 { Lang::De } else { Lang::En }
    }
    #[cfg(not(windows))]
    {
        let l = std::env::var("LANG").unwrap_or_default();
        if l.starts_with("de") { Lang::De } else { Lang::En }
    }
}

/// Dezimaltrennzeichen der aktuellen Sprache auf eine Zahl anwenden ("4.7" -> "4,7").
pub fn decimal(s: String) -> String {
    if is_en() { s } else { s.replace('.', ",") }
}

/// Anzahl mit passender Einzahl oder Mehrzahl, z. B.
/// `count(n, "Eintrag", "Einträge", "entry", "entries")` -> "1 Eintrag" / "3 entries".
pub fn count(n: usize, de_one: &str, de_many: &str, en_one: &str, en_many: &str) -> String {
    let word = match (is_en(), n == 1) {
        (false, true) => de_one,
        (false, false) => de_many,
        (true, true) => en_one,
        (true, false) => en_many,
    };
    format!("{n} {word}")
}

/// "4 Kapitel" / "1 chapter" / "4 chapters"
pub fn chapters(n: usize) -> String {
    count(n, "Kapitel", "Kapitel", "chapter", "chapters")
}

/// Text in der aktuellen Sprache: `tr!("Deutsch", "English")`.
#[macro_export]
macro_rules! tr {
    ($de:literal, $en:literal) => {
        if $crate::i18n::is_en() { $en } else { $de }
    };
}

/// Wie `format!`, mit deutscher und englischer Vorlage: `trf!("{n} Kapitel", "{n} chapters")`.
#[macro_export]
macro_rules! trf {
    ($de:literal, $en:literal $(, $($args:tt)*)?) => {
        if $crate::i18n::is_en() { format!($en $(, $($args)*)?) } else { format!($de $(, $($args)*)?) }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switches_language() {
        let n = 3;
        set_lang(Lang::En);
        assert_eq!(tr!("Abbrechen", "Cancel"), "Cancel");
        assert_eq!(trf!("{n} Kapitel", "{n} chapters"), "3 chapters");
        assert_eq!(trf!("{} von {}", "{} of {}", 1, 2), "1 of 2");
        assert_eq!(decimal("4.7".into()), "4.7");
        set_lang(Lang::De);
        assert_eq!(tr!("Abbrechen", "Cancel"), "Abbrechen");
        assert_eq!(trf!("{n} Kapitel", "{n} chapters"), "3 Kapitel");
        assert_eq!(decimal("4.7".into()), "4,7");
        assert_eq!(Lang::from_code(Lang::En.code()), Some(Lang::En));
    }
}
