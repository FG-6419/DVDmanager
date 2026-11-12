# DVDmanager

DVDmanager ist ein Windows-Programm für **eigene, ungeschützte Video-DVDs**. Es verkleinert DVDs auf Rohlinggröße, stellt Titel neu zusammen, schneidet sie und gibt sie als DVD, ISO oder Videodatei aus. Bild und Ton werden dabei nicht neu kodiert, nur an bildgenauen Schnittstellen und Blenden.

Kopiergeschützte DVDs (CSS) verarbeitet das Programm bewusst nicht.

## Was es kann

**Verkleinern (Ganze DVD)**
- Passt eine DVD 9 auf einen DVD-5-Rohling (4,7 GB) oder auf eine eigene Zielgröße an.
- Das Bild wird neu quantisiert statt neu kodiert, Menüs und Struktur bleiben erhalten.
- Pro Titel, Tonspur und Untertitel lässt sich einstellen, was bleibt und wie stark verkleinert wird.

**Neu zusammenstellen**
- Titel und Kapitel in eigener Reihenfolge zu einer neuen DVD zusammenstellen.
- Schneiden am Schlüsselbild (ohne Neukodierung) oder bildgenau.
- Blenden aus und nach Schwarz oder Weiß, auf Wunsch mit Ton.
- Szenenerkennung: einen Titel an Szenenwechseln teilen.
- Kapitel-Editor mit eigenen Kapitelmarken. Auf Wunsch steht an jedem Schnitt eine Kapitelmarke, und alle Schnitte laufen als ein Titel.
- Saubere Schnittstellen im Ton: angeschnittene Tonrahmen werden entfernt, eine kurze Blende verhindert Knacken.

**Quellen**
- DVD-Laufwerk, VIDEO_TS-Ordner, ISO-Abbild.
- Einzelne VOB- und MPEG-2-Dateien. Sie werden ohne Neukodierung in eine DVD-Struktur umgepackt.

**Ausgabe**
- DVD-Ordner (VIDEO_TS), ISO-Abbild oder direkt auf Rohling (über den Windows-Brenner).
- Eine VOB-Datei, eine schlichte MPEG-2-Datei oder eine MKV-Datei mit Kapiteln.
- Nach dem Export wird die Ausgabe automatisch auf beschädigte Tonrahmen und Zeitsprünge geprüft.
- Auf Wunsch entsteht ein Übersichtsbild (JPG) mit Vorschaubildern in einstellbarem Abstand.

**Bedienung**
- Vorschau mit Ton und Zeitleiste, „Alle Schnitte abspielen“ und „Übergang anhören“.
- Projekte speichern und laden, Warteschlange für mehrere DVDs.
- Oberfläche auf Deutsch und Englisch, hell und dunkel.

## Bauen

Benötigt wird [Rust](https://www.rust-lang.org/) (stabile Version).

```bash
cargo build --release
```

Das Programm liegt danach unter `target/release/dvdmanager.exe`. Die Tests laufen mit `cargo test` und nutzen eine eingebaute Beispiel-DVD.

## Änderungen

Was sich in welcher Version geändert hat, steht in [CHANGELOG.md](CHANGELOG.md) und im Programm unter „Über“.
