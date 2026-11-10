//! Tests für Kapitelmarken, Projektdateien und Stapelverarbeitung mit der Beispiel-DVD.
//! Sie brauchen kein ffmpeg.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::Arc;

use discslim::demo;
use discslim::dvd::model::Disc;
use discslim::dvd::source::{Source, Space};
use discslim::dvd::vob::*;
use discslim::dvd::{be32, SECTOR};
use discslim::engine::backup::{run, BackupMsg, BackupRequest, Output};
use discslim::engine::batch::{AutoSettings, BatchItem, PlanSource, Queue, Status};
use discslim::engine::plan::*;
use discslim::engine::project::{Fingerprint, Project};

fn work(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("target").join("testwork").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn open_analysed(path: &Path) -> Disc {
    let mut d = Disc::open(Source::open(path).unwrap()).unwrap();
    d.analyse(true, &mut |_, _| true).unwrap();
    d
}

fn run_backup(disc: Disc, plan: Plan, output: Output) {
    let (tx, rx) = channel();
    run(BackupRequest { disc: Arc::new(disc), plan, output, deep_analysis: false }, tx, Arc::new(AtomicBool::new(false)));
    for m in rx.iter() {
        match m {
            BackupMsg::Done(_) => return,
            BackupMsg::Failed(e) => panic!("Backup fehlgeschlagen: {e}"),
            BackupMsg::Cancelled => panic!("abgebrochen"),
            _ => {}
        }
    }
    panic!("keine Rückmeldung");
}

/// VOBU-Tabelle, Zellen und Sprungverweise im Titelbereich müssen zusammenpassen.
fn check_cells(path: &Path) {
    let disc = Disc::open(Source::open(path).unwrap()).unwrap();
    for (n, v) in &disc.vts {
        let mut r = disc.source.reader(Space::title(*n)).unwrap();
        let total = r.sectors() as u32;
        let mut buf = Vec::new();
        let mut set = HashSet::new();
        for (i, a) in v.admap.iter().enumerate() {
            r.read(u64::from(*a), 1, &mut buf).unwrap();
            assert!(is_nav(&buf), "VOBU-Tabelle zeigt nicht auf ein NAV-Paket: {a}");
            let ea = Nav::vobu_ea(&buf);
            let next = v.admap.get(i + 1).copied().unwrap_or(total);
            assert_eq!(*a + ea + 1, next, "VOBU-Ende bei {a}");
            let nv = be32(&buf, DSI_OFF + DSI_SRI + 80) & 0x3FFF_FFFF;
            if nv != SRI_END && nv != 0 {
                assert_eq!(*a + nv, next, "Sprung zur nächsten VOBU");
            }
            set.insert(*a);
        }
        let mut last_end = None;
        for c in v.cells.iter() {
            assert!(set.contains(&c.start), "Zellstart ist keine VOBU");
            if let Some(e) = last_end {
                assert_eq!(c.start, e + 1, "Lücke zwischen Zellen");
            }
            last_end = Some(c.last);
        }
        assert_eq!(last_end.map(|e| e + 1), Some(total), "Zellen decken den Titel nicht ab");
        for p in &v.pgcs {
            for c in &p.cells {
                assert!(v.cells.iter().any(|a| a.start == c.first_sector && a.last == c.last_sector), "Zelle unbekannt");
                assert!(v.admap.contains(&c.last_vobu_start), "letzte VOBU ist keine VOBU");
            }
        }
    }
}

fn longest_title(disc: &Disc) -> usize {
    (0..disc.titles.len()).max_by_key(|i| disc.titles[*i].ms).unwrap()
}

#[test]
fn custom_chapter_marks() {
    let w = work("kapitel");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);
    let ti = longest_title(&disc);
    let t = &disc.titles[ti];
    let last = t.chapters.len() - 1;
    let total = t.ms;
    assert!(total > 6_000, "Beispieltitel zu kurz: {total}");

    // 1) Eigene Marken ohne Schnitt
    let marks = vec![total / 4, total / 2, total * 3 / 4];
    let mut e = ReauthorEntry::new(ti, 0, last, ItemSettings::default());
    e.chapter_marks = Some(marks.clone());
    assert_eq!(e.chapter_count(&disc), 4);
    let mut plan = Plan::new(NO_TARGET);
    plan.mode = Mode::Reauthor;
    plan.reauthor.push(e.clone());
    let out = w.join("marken");
    run_backup(open_analysed(&src), plan, Output::Folder(out.clone()));
    let vdir = out.join("VIDEO_TS");
    check_cells(&vdir);
    let re = open_analysed(&vdir);
    let ch = &re.titles[0].chapters;
    println!("Kapitel: {:?}, gewünscht 0 + {marks:?}", ch.iter().map(|c| c.start_ms).collect::<Vec<_>>());
    assert_eq!(ch.len(), 4, "drei Marken ergeben vier Kapitel");
    for (c, m) in ch.iter().skip(1).zip(&marks) {
        assert!(c.start_ms.abs_diff(*m) <= 1_100, "Kapitel bei {} statt {m}", c.start_ms);
    }
    assert!(re.titles[0].ms.abs_diff(total) <= 1_000, "Länge {} statt {total}", re.titles[0].ms);

    // 2) Marken mit Schnitt: Zeiten verschieben sich um den Anfang, Marken außerhalb entfallen.
    let cut_in = total / 8;
    let cut_out = total * 5 / 8;
    e.cut = Cut { start_ms: Some(cut_in), end_ms: Some(cut_out), exact: false };
    e.chapter_marks = Some(vec![total / 16, total / 4, total / 2, total * 3 / 4]);
    let want = e.output_marks(&disc).unwrap();
    assert_eq!(want.len(), 2, "nur die Marken im Schnitt bleiben");
    let mut plan = Plan::new(NO_TARGET);
    plan.mode = Mode::Reauthor;
    plan.reauthor.push(e);
    let out = w.join("marken_schnitt");
    run_backup(open_analysed(&src), plan, Output::Folder(out.clone()));
    let vdir = out.join("VIDEO_TS");
    check_cells(&vdir);
    let re = open_analysed(&vdir);
    assert_eq!(re.titles[0].chapters.len(), 3);

    // 3) Ohne eigene Marken bleiben die Kapitel der DVD.
    let mut plan = Plan::new(NO_TARGET);
    plan.mode = Mode::Reauthor;
    plan.reauthor.push(ReauthorEntry::new(ti, 0, last, ItemSettings::default()));
    let out = w.join("original");
    run_backup(open_analysed(&src), plan, Output::Folder(out.clone()));
    let re = open_analysed(&out.join("VIDEO_TS"));
    assert_eq!(re.titles[0].chapters.len(), t.chapters.len());
}

/// Mittlere Farbe (R, G, B) des Bildes an einer Stelle eines Titels.
fn frame_rgb(disc: &Disc, title: usize, ms: f64) -> [f64; 3] {
    use discslim::engine::preview::{self, PreviewCmd};
    let last = disc.titles[title].chapters.len() - 1;
    let seg = preview::title_segments(disc, title, 0, last);
    let h = preview::spawn(disc.source.clone(), seg, 0.0, None, 0, None);
    h.cmd.send(PreviewCmd::SeekTime { ms, exact: true, id: 1 }).unwrap();
    loop {
        let f = h.frames.recv_timeout(std::time::Duration::from_secs(10)).expect("kein Bild");
        if f.seek_id == 1 && !f.eos {
            let mut acc = [0f64; 3];
            let mut n = 0.0;
            for c in f.rgba.chunks_exact(4) {
                for k in 0..3 {
                    acc[k] += f64::from(c[k]);
                }
                n += 1.0;
            }
            return acc.map(|v| v / n);
        }
    }
}

/// Mittlere Helligkeit (0..255) des Bildes an einer Stelle eines Titels.
fn brightness(disc: &Disc, title: usize, ms: f64) -> f64 {
    use discslim::engine::preview::{self, PreviewCmd};
    let last = disc.titles[title].chapters.len() - 1;
    let seg = preview::title_segments(disc, title, 0, last);
    let h = preview::spawn(disc.source.clone(), seg, 0.0, None, 0, None);
    h.cmd.send(PreviewCmd::SeekTime { ms, exact: true, id: 1 }).unwrap();
    loop {
        let f = h.frames.recv_timeout(std::time::Duration::from_secs(10)).expect("kein Bild");
        if f.seek_id == 1 && !f.eos {
            let px = f.rgba.chunks_exact(4);
            let n = px.len() as f64;
            return px.map(|c| (u32::from(c[0]) + u32::from(c[1]) + u32::from(c[2])) as f64 / 3.0).sum::<f64>() / n;
        }
    }
}

/// Pegel (Effektivwert, 16 Bit) der ersten LPCM-Spur je Paket, mit Zeit ab Titelbeginn (ms).
fn audio_levels(vdir: &Path) -> Vec<(f64, f64)> {
    use discslim::engine::audio::{audio_pes, Codec, TrackChoice};
    let track = TrackChoice { phys: 0, codec: Codec::Lpcm, channels: 2 };
    let mut files: Vec<PathBuf> = std::fs::read_dir(vdir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| {
            let n = p.file_name().unwrap().to_string_lossy().to_uppercase();
            n.starts_with("VTS_01_") && n.ends_with(".VOB") && !n.ends_with("_0.VOB")
        })
        .collect();
    files.sort();
    let mut base: Option<u64> = None;
    let mut next = 0.0;
    let mut out = Vec::new();
    for f in files {
        let data = std::fs::read(f).unwrap();
        for s in data.chunks_exact(SECTOR) {
            if base.is_none() && classify(s) == Stream::Nav {
                base = Some(u64::from(be32(s, PCI_OFF + 12)));
            }
            let Some(p) = audio_pes(s, &track) else { continue };
            let t = match (p.pts, base) {
                (Some(pts), Some(b)) => (pts as f64 - b as f64) / 90.0,
                _ => next,
            };
            let vals: Vec<f64> = p.data.chunks_exact(2).map(|c| f64::from(i16::from_be_bytes([c[0], c[1]]))).collect();
            let n = vals.len() / 2;
            next = t + n as f64 / 48.0;
            if n > 0 {
                let rms = (vals.iter().map(|v| v * v).sum::<f64>() / vals.len() as f64).sqrt();
                out.push((t + n as f64 / 96.0, rms));
            }
        }
    }
    out
}

#[test]
fn fades_at_cut_points() {
    let w = work("blende");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);
    let ti = longest_title(&disc);
    let last = disc.titles[ti].chapters.len() - 1;
    let (cut_in, cut_out) = (4_000u64, 20_000u64);

    // (bildgenau, Ton mitblenden)
    for (exact, audio) in [(true, true), (false, true), (true, false)] {
        let mut e = ReauthorEntry::new(ti, 0, last, ItemSettings::default());
        e.cut = Cut { start_ms: Some(cut_in), end_ms: Some(cut_out), exact };
        e.fade = Fades { fade_in: FadeKind::Black, fade_out: FadeKind::White, ms: 1_500, audio };
        let mut plan = Plan::new(NO_TARGET);
        plan.mode = Mode::Reauthor;
        plan.reauthor.push(e);
        let out = w.join(format!("blende_{exact}_{audio}"));
        run_backup(open_analysed(&src), plan, Output::Folder(out.clone()));
        let vdir = out.join("VIDEO_TS");
        check_cells(&vdir);
        let re = open_analysed(&vdir);
        let len = re.titles[0].ms as f64;
        let first = brightness(&re, 0, 0.0);
        let quarter = brightness(&re, 0, 600.0);
        let mid = brightness(&re, 0, len / 2.0);
        let end = brightness(&re, 0, len - 60.0);
        let reference = brightness(&disc, ti, cut_in as f64 + len / 2.0);
        println!("bildgenau={exact}: Anfang {first:.0}, nach 0,6 s {quarter:.0}, Mitte {mid:.0} (Quelle {reference:.0}), Ende {end:.0}");
        assert!((mid - reference).abs() < 12.0, "Mitte verändert: {mid} statt {reference}");
        let lv = audio_levels(&vdir);
        let level = |a: f64, b: f64| {
            let v: Vec<f64> = lv.iter().filter(|(t, _)| *t >= a && *t < b).map(|x| x.1).collect();
            assert!(!v.is_empty(), "kein Ton zwischen {a} und {b} ms");
            (v.iter().map(|x| x * x).sum::<f64>() / v.len() as f64).sqrt()
        };
        let a_first = level(0.0, 100.0);
        let a_ramp = level(600.0, 800.0);
        let a_mid = level(len / 2.0 - 500.0, len / 2.0 + 500.0);
        let a_end = level(len - 100.0, len);
        println!("Ton: Anfang {a_first:.0}, 0,6-0,8 s {a_ramp:.0}, Mitte {a_mid:.0}, Ende {a_end:.0}");
        assert!(a_mid > 2_500.0, "Ton in der Mitte zu leise: {a_mid}");
        if !exact {
            // Schnitt an Bildgruppen: eingestellte Blenden wirken nicht, nichts wird neu kodiert
            assert!(first > 40.0 && end < 200.0, "Blende trotz Schnitt an Bildgruppen: {first} / {end}");
            assert!(a_first > 2_500.0 && a_end > 2_500.0, "Ton geblendet: {a_first} / {a_end}");
            continue;
        }
        // Bild: aus Schwarz, nach Weiß; Ton: still am Anfang und Ende
        assert!(first < 30.0, "Anfang nicht schwarz: {first}");
        assert!(quarter > first + 10.0 && quarter < mid, "Einblenden fehlt: {quarter}");
        assert!(end > 200.0, "Ende nicht weiß: {end}");
        if !audio {
            // Ton-Blende nicht zugeschaltet: Ton in voller Lautstärke bis zum Rand
            assert!(a_first > 2_500.0 && a_end > 2_500.0, "Ton geblendet, obwohl abgeschaltet: {a_first} / {a_end}");
            continue;
        }
        assert!(a_first < 300.0, "Ton am Anfang nicht still: {a_first}");
        assert!(a_ramp > 600.0 && a_ramp < a_mid * 0.75, "Ton blendet nicht ein: {a_ramp}");
        assert!(a_end < 300.0, "Ton am Ende nicht still: {a_end}");
        {
            // Gleich nach dem Einblenden: wie die Quelle, kein Nachflackern
            for t in [1_560.0, 1_620.0, 1_700.0, 2_100.0] {
                let got = brightness(&re, 0, t);
                let want = brightness(&disc, ti, cut_in as f64 + t);
                assert!((got - want).abs() < 6.0, "nach der Blende bei {t} ms: {got} statt {want}");
            }
        }
    }
}

#[test]
fn scene_detection_splits_title() {
    use discslim::engine::scenes::{scan, split_entry};
    let w = work("szenen");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);
    let ti = longest_title(&disc);
    let last = disc.titles[ti].chapters.len() - 1;
    let mut e = ReauthorEntry::new(ti, 0, last, ItemSettings::default());
    let total = entry_duration_ms(&disc, &e);
    e.cut = Cut::whole(total, false);
    // Im Beispiel wechseln an jedem Kapitelanfang Farbe und Ziffer
    let chapters = e.original_marks(&disc);
    assert_eq!(chapters.len(), 3);

    let cancel = AtomicBool::new(false);
    let mut last_p = 0.0;
    let res = scan(&disc, &e, &cancel, &mut |p| last_p = p).unwrap();
    assert!((last_p - 1.0).abs() < 1e-6);
    for c in res.candidates.iter().filter(|c| c.score > 0.05) {
        println!("Kandidat {} ms (genau {:?}): Stärke {:.3}, Umgebung {:.3}", c.at_ms, c.exact_ms, c.score, c.base);
    }
    for exact in [false, true] {
        let cuts = res.cuts(0.5, 2_000, exact);
        println!("bildgenau={exact}: {cuts:?}, Kapitel {chapters:?}");
        assert_eq!(cuts.len(), chapters.len(), "Szenen {cuts:?} statt {chapters:?}");
        for (c, k) in cuts.iter().zip(&chapters) {
            let tol = if exact { 45 } else { 260 };
            assert!(c.abs_diff(*k) <= tol, "Wechsel bei {c} statt {k}");
        }
    }
    // Hohe Mindestlänge fasst zusammen, geringe Empfindlichkeit findet nichts Kleines
    assert!(res.cuts(0.5, 30_000, false).len() < 3);
    // Abbruch
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(scan(&disc, &e, &cancel, &mut |_| {}).is_err());

    // Aufteilen und sichern: vier Einträge, zusammen so lang wie der Titel
    let cuts = res.cuts(0.5, 2_000, true);
    let parts = split_entry(&disc, &e, &cuts, true);
    assert_eq!(parts.len(), 4);
    assert_eq!(parts[0].cut.start_ms, Some(0));
    assert_eq!(parts[3].cut.end_ms, Some(total));
    let mut plan = Plan::new(NO_TARGET);
    plan.mode = Mode::Reauthor;
    plan.reauthor = parts;
    let out = w.join("ausgabe");
    run_backup(open_analysed(&src), plan, Output::Folder(out.clone()));
    let vdir = out.join("VIDEO_TS");
    check_cells(&vdir);
    let re = open_analysed(&vdir);
    assert_eq!(re.titles.len(), 4);
    let sum: u64 = re.titles.iter().map(|t| t.ms).sum();
    println!("Längen {:?}, Summe {sum} von {total}", re.titles.iter().map(|t| t.ms).collect::<Vec<_>>());
    assert!(sum.abs_diff(total) < 400, "Summe {sum} statt {total}");
    // Jeder Teil zeigt nur eine Szene: Anfang und Ende gleich gefärbt
    for k in 0..4 {
        let len = re.titles[k].ms as f64;
        let a = frame_rgb(&re, k, 100.0);
        let b = frame_rgb(&re, k, len - 100.0);
        let d: f64 = a.iter().zip(&b).map(|(x, y)| (x - y).abs()).sum();
        assert!(d < 40.0, "Teil {k} enthält zwei Szenen: {a:?} / {b:?}");
    }
}

/// Zeitstempel einer MPEG-Datei: (SCR je Paket, PTS je Paket mit Stream-ID), in 90 kHz.
fn mpeg_times(path: &Path) -> (Vec<u64>, Vec<(u8, u64)>) {
    let data = std::fs::read(path).unwrap();
    let mut scr = Vec::new();
    let mut pts = Vec::new();
    for s in data.chunks_exact(SECTOR) {
        if !is_pack(s) {
            continue;
        }
        scr.push(get_scr(s) / 300);
        let h = pack_header_len(s);
        if h + 14 > s.len() || s[h] != 0 || s[h + 1] != 0 || s[h + 2] != 1 {
            continue;
        }
        let id = s[h + 3];
        if matches!(id, 0xE0 | 0xBD | 0xC0..=0xC7) && s[h + 7] & 0x80 != 0 {
            pts.push((id, read_ts(&s[h + 9..])));
        }
    }
    (scr, pts)
}

#[test]
fn mpeg_timestamps_continuous() {
    let w = work("mpeg_zeit");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);
    let whole = |t: usize| {
        let mut e = ReauthorEntry::new(t, 0, disc.titles[t].chapters.len() - 1, ItemSettings::default());
        e.cut = Cut::whole(entry_duration_ms(&disc, &e), false);
        e
    };
    let part = |t: usize, a: u64, b: u64, exact: bool| {
        let mut e = whole(t);
        e.cut = Cut { start_ms: Some(a), end_ms: Some(b), exact };
        e
    };
    // Wie „In Einzeltitel zerlegen“: ein Kapitel je Eintrag, ohne Schnitt
    let chapter = |t: usize, c: usize| {
        let mut e = ReauthorEntry::new(t, c, c, ItemSettings::default());
        e.cut = Cut::whole(entry_duration_ms(&disc, &e), false);
        e
    };
    let cases: Vec<(&str, Vec<ReauthorEntry>)> = vec![
        ("drei Titel", vec![whole(0), whole(1), whole(2)]),
        ("Titel rückwärts", vec![whole(2), whole(1), whole(0)]),
        ("Ausschnitte", vec![part(0, 40_000, 50_000, false), part(0, 5_000, 15_000, true), part(1, 3_000, 9_000, false)]),
        ("später Anfang", vec![part(0, 30_000, 60_000, false), whole(2)]),
        ("Einzelkapitel", vec![chapter(0, 2), chapter(0, 1)]),
    ];
    for (name, entries) in cases {
        let total: u64 = entries
            .iter()
            .map(|e| {
                let t = entry_duration_ms(&disc, e);
                let (a, b) = e.cut.effective(t);
                b.min(t) - a
            })
            .sum();
        let mut plan = Plan::new(NO_TARGET);
        plan.mode = Mode::Reauthor;
        plan.reauthor = entries;
        let out = w.join(format!("{name}.mpg"));
        run_backup(open_analysed(&src), plan, Output::Mpeg { path: out.clone(), audio: None });
        let (scr, pts) = mpeg_times(&out);
        let video: Vec<u64> = pts.iter().filter(|p| p.0 == 0xE0).map(|p| p.1).collect();
        let (first, last) = (video[0], *video.iter().max().unwrap());
        let span_ms = (last - first) / 90;
        let max_any = pts.iter().map(|p| p.1).max().unwrap();
        let min_any = pts.iter().map(|p| p.1).min().unwrap();
        // Was ein Player am Anfang und am Ende der Datei findet
        let tail_ms = video[video.len().saturating_sub(5)..].iter().max().unwrap().saturating_sub(first) / 90;
        let scr_back = scr.windows(2).filter(|w| w[1] < w[0]).count();
        println!(
            "{name}: Soll {total} ms, Video {first}..{last} ({span_ms} ms, Ende {tail_ms} ms), alle PTS {min_any}..{max_any}, SCR rückwärts {scr_back}x"
        );
        assert!(max_any < 90_000 * 3600 * 4, "{name}: Zeitstempel übergelaufen ({max_any})");
        assert!(first < 90_000 * 10, "{name}: Datei beginnt nicht bei null ({first})");
        assert!(min_any + 90_000 > first, "{name}: Ton liegt weit vor dem ersten Bild ({min_any} < {first})");
        assert_eq!(scr_back, 0, "{name}: Systemuhr läuft rückwärts");
        assert!(tail_ms.abs_diff(total) < 1_500, "{name}: Länge am Dateiende {tail_ms} statt {total} ms");
        // VLC sucht das Ende nur in den letzten 200 000 Bytes; das Abschlusspaket trägt die Endzeit.
        let data = std::fs::read(&out).unwrap();
        let last = &data[data.len() - SECTOR..];
        let h = pack_header_len(last);
        assert_eq!(last[h + 3], 0xE0, "{name}: kein Abschlusspaket");
        let end_ms = (read_ts(&last[h + 9..]) - first) / 90;
        assert!(end_ms.abs_diff(total) < 1_500, "{name}: Endzeit {end_ms} statt {total} ms");
    }
}

#[test]
fn project_file_round_trip() {
    let w = work("projekt");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);
    let mut plan = Plan::new(dvd5_sectors());
    plan.mode = Mode::Reauthor;
    plan.default_video = VideoMode::Auto;
    let mut e = ReauthorEntry::new(0, 0, disc.titles[0].chapters.len() - 1, ItemSettings::default());
    e.chapter_marks = Some(vec![2_000]);
    plan.reauthor.push(e);
    plan.items.insert(ItemId::Title(1), ItemSettings { video: VideoMode::Custom(0.5), ..Default::default() });

    let file = w.join("test.dvdproj");
    Project::new(&disc, &plan).save(&file).unwrap();
    assert!(Project::is_project_path(&file));
    let p = Project::load(&file).unwrap();
    assert_eq!(p.source, disc.source.origin);
    let back = p.plan_for(&disc).unwrap();
    assert_eq!(back.mode, Mode::Reauthor);
    assert_eq!(back.reauthor[0].chapter_marks, Some(vec![2_000]));
    assert_eq!(back.items[&ItemId::Title(1)].video, VideoMode::Custom(0.5));

    // Gehört das Projekt zu einer anderen DVD, wird es abgelehnt.
    let mut other = p.clone();
    other.fingerprint = Fingerprint { titles: vec![(1, 1)] };
    assert!(other.plan_for(&disc).is_err());

    // Kaputte Datei
    std::fs::write(w.join("kaputt.dvdproj"), "{ nein").unwrap();
    assert!(Project::load(&w.join("kaputt.dvdproj")).is_err());
}

#[test]
fn batch_queue_runs_all() {
    let w = work("stapel");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let disc = open_analysed(&src);

    let auto = AutoSettings {
        target_sectors: (disc.total_sectors as f64 * 0.8) as u64,
        only_lang: None,
        remove_puo: false,
        region_free: false,
        split_vob: true,
    };
    let mut fixed = Plan::new(NO_TARGET);
    fixed.mode = Mode::Reauthor;
    fixed.reauthor.push(ReauthorEntry::new(0, 0, 0, ItemSettings::default()));
    let fp = Fingerprint::of(&disc);

    let mut q = Queue::default();
    q.push(BatchItem::new(src.clone(), "A".into(), PlanSource::Auto(auto.clone()), Output::Iso(w.join("a.iso")), false));
    q.push(BatchItem::new(
        src.clone(),
        "B".into(),
        PlanSource::Fixed { plan: Box::new(fixed.clone()), fingerprint: fp.clone() },
        Output::Folder(w.join("b")),
        false,
    ));
    q.push(BatchItem::new(w.join("gibt-es-nicht"), "C".into(), PlanSource::Auto(auto), Output::Iso(w.join("c.iso")), false));
    q.push(BatchItem::new(
        src.clone(),
        "D".into(),
        PlanSource::Fixed { plan: Box::new(fixed), fingerprint: Fingerprint { titles: vec![(5, 1)] } },
        Output::Folder(w.join("d")),
        false,
    ));
    assert_eq!(q.waiting(), 4);
    q.start(|| {});
    q.wait();
    assert!(!q.is_running());

    let items = q.items().clone();
    for i in &items {
        println!("{}: {:?}", i.label, match &i.status {
            Status::Done(_) => "fertig".to_string(),
            Status::Failed(e) => e.clone(),
            s => format!("{s:?}"),
        });
    }
    assert!(matches!(items[0].status, Status::Done(_)));
    assert!(matches!(items[1].status, Status::Done(_)));
    assert!(matches!(items[2].status, Status::Failed(_)), "fehlende Quelle");
    assert!(matches!(items[3].status, Status::Failed(_)), "falsche DVD");
    assert!(w.join("a.iso").exists());
    assert!(w.join("b").join("VIDEO_TS").join("VIDEO_TS.IFO").exists());
    if let Status::Done(s) = &items[0].status {
        assert!(s.fits, "ISO passt nicht aufs Ziel");
    }
    let re = open_analysed(&w.join("b").join("VIDEO_TS"));
    assert_eq!(re.titles.len(), 1);
    let _ = SECTOR;

    // Nochmal versuchen und aufräumen
    q.retry_failed();
    assert_eq!(q.waiting(), 2);
    q.clear_finished();
    assert_eq!(q.items().len(), 2);
}
