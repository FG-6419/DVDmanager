//! Bettet unter Windows das Programmsymbol in die Exe ein, damit der Explorer es zeigt.
//! Das Symbol wird aus derselben Zeichnung erzeugt wie das Fenstersymbol (src/icon.rs).

#[path = "src/icon.rs"]
mod icon;

fn main() {
    println!("cargo:rerun-if-changed=src/icon.rs");
    println!("cargo:rerun-if-changed=build.rs");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let ico = out.join("dvdmanager.ico");
    std::fs::write(&ico, ico_bytes(&[16, 24, 32, 48, 64, 128, 256])).expect("Symboldatei schreiben");

    let mut res = winresource::WindowsResource::new();
    res.set_icon(ico.to_str().unwrap());
    res.set("FileDescription", "DVDmanager");
    res.set("ProductName", "DVDmanager");
    if let Err(e) = res.compile() {
        // Ohne Ressourcen-Compiler baut das Programm trotzdem, nur ohne Exe-Symbol.
        println!("cargo:warning=Programmsymbol nicht eingebettet: {e}");
    }
}

/// ICO-Datei mit unkomprimierten 32-Bit-Bildern (BGRA, von unten nach oben, mit Maske).
fn ico_bytes(sizes: &[u32]) -> Vec<u8> {
    let images: Vec<Vec<u8>> = sizes.iter().map(|&s| dib(s, &icon::disc_rgba(s))).collect();
    let mut out = Vec::new();
    out.extend_from_slice(&0u16.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&(sizes.len() as u16).to_le_bytes());
    let mut offset = 6 + 16 * sizes.len() as u32;
    for (s, img) in sizes.iter().zip(&images) {
        let dim = if *s >= 256 { 0 } else { *s as u8 };
        out.extend_from_slice(&[dim, dim, 0, 0]);
        out.extend_from_slice(&1u16.to_le_bytes()); // Ebenen
        out.extend_from_slice(&32u16.to_le_bytes()); // Bit je Pixel
        out.extend_from_slice(&(img.len() as u32).to_le_bytes());
        out.extend_from_slice(&offset.to_le_bytes());
        offset += img.len() as u32;
    }
    for img in images {
        out.extend_from_slice(&img);
    }
    out
}

fn dib(size: u32, rgba: &[u8]) -> Vec<u8> {
    let mask_row = size.div_ceil(32) * 4;
    let mut d = Vec::new();
    d.extend_from_slice(&40u32.to_le_bytes());
    d.extend_from_slice(&(size as i32).to_le_bytes());
    d.extend_from_slice(&((size * 2) as i32).to_le_bytes()); // Bild + Maske
    d.extend_from_slice(&1u16.to_le_bytes());
    d.extend_from_slice(&32u16.to_le_bytes());
    d.extend_from_slice(&[0u8; 24]); // ohne Kompression, Rest 0
    for y in (0..size).rev() {
        for x in 0..size {
            let i = ((y * size + x) * 4) as usize;
            d.extend_from_slice(&[rgba[i + 2], rgba[i + 1], rgba[i], rgba[i + 3]]);
        }
    }
    // Maske: alles sichtbar, die Transparenz steckt im Alphakanal
    d.extend(std::iter::repeat_n(0u8, (mask_row * size) as usize));
    d
}
