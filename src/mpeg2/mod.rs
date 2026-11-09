//! MPEG-2-Video: Bit-Ein-/Ausgabe, VLC-Tabellen, Verkleinern ohne Neukodieren,
//! Dekodieren für die Vorschau und ein einfacher Encoder für die Beispiel-DVD.

pub mod bits;
pub mod decode;
pub mod encode;
pub mod headers;
pub mod reencode;
pub mod requant;
pub mod tables;
