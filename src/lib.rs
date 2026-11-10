//! Kern der App: DVD-Strukturen lesen, MPEG-2 verkleinern, Backup schreiben.
//! Die Oberfläche liegt getrennt in `main.rs` und `ui`.

// Zuerst, damit tr!/trf! in allen folgenden Modulen bekannt sind.
#[macro_use]
pub mod i18n;

pub mod demo;
pub mod dvd;
pub mod engine;
pub mod mpeg2;
pub mod version;
