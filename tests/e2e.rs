//! Ende-zu-Ende-Tests. Die Gegenprobe mit einem fremden MPEG-2-Decoder läuft nur,
//! wenn unter target/devtools ein ffmpeg liegt; sonst werden diese Tests übersprungen.

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::channel;
use std::sync::Arc;

use discslim::demo::{self, TitleSpec, TrackSpec};
use discslim::dvd::model::Disc;
use discslim::dvd::source::{Source, Space};
use discslim::dvd::vob::*;
use discslim::dvd::{be32, SECTOR};
use discslim::engine::backup::{run, BackupMsg, BackupRequest, Output};
use discslim::engine::plan::*;
use discslim::engine::plan::Cut;
use discslim::mpeg2::decode::Decoder;
use discslim::mpeg2::requant::{Requant, VideoOpts};

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn ffmpeg() -> Option<PathBuf> {
    let dir = root().join("target").join("devtools").join("ffdl").join("x").join("imageio_ffmpeg").join("binaries");
    std::fs::read_dir(dir).ok()?.flatten().map(|e| e.path()).find(|p| p.extension().is_some_and(|e| e == "exe"))
}

fn work(name: &str) -> PathBuf {
    let d = root().join("target").join("testwork").join(name);
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn make_es(ff: &Path, out: &Path, extra: &[&str], seconds: &str) {
    let mut c = Command::new(ff);
    c.args(["-y", "-v", "error", "-f", "lavfi", "-i"]);
    c.arg(format!("testsrc2=size=720x576:rate=25:duration={seconds}"));
    c.args(["-c:v", "mpeg2video", "-pix_fmt", "yuv420p", "-b:v", "6M", "-maxrate", "9M", "-bufsize", "1835k", "-g", "12", "-bf", "2"]);
    c.args(extra);
    c.args(["-f", "mpeg2video"]);
    c.arg(out);
    let st = c.status().expect("ffmpeg");
    assert!(st.success());
}

/// Dekodiert mit ffmpeg zu rohem YUV; meldet alles, was ffmpeg als Fehler ansieht.
fn ff_decode(ff: &Path, input: &Path) -> (Vec<u8>, String) {
    let out = Command::new(ff)
        .args(["-v", "error", "-err_detect", "explode", "-i"])
        .arg(input)
        .args(["-map", "0:v:0", "-f", "rawvideo", "-pix_fmt", "yuv420p", "-"])
        .output()
        .expect("ffmpeg");
    (out.stdout, String::from_utf8_lossy(&out.stderr).to_string())
}

fn my_decode(es: &[u8]) -> Vec<u8> {
    let mut d = Decoder::new();
    let mut out = Vec::new();
    let collect = |d: &mut Decoder, out: &mut Vec<u8>| {
        while let Some(f) = d.pop_frame() {
            for r in 0..f.height {
                out.extend_from_slice(&f.y[r * f.stride..r * f.stride + f.width]);
            }
            let cs = f.stride / 2;
            for r in 0..f.height / 2 {
                out.extend_from_slice(&f.u[r * cs..r * cs + f.width / 2]);
            }
            for r in 0..f.height / 2 {
                out.extend_from_slice(&f.v[r * cs..r * cs + f.width / 2]);
            }
        }
    };
    for chunk in es.chunks(1 << 16) {
        d.push(chunk);
        collect(&mut d, &mut out);
    }
    d.flush();
    collect(&mut d, &mut out);
    out
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let n = a.len().min(b.len());
    assert!(n > 0, "keine Bilddaten");
    let mut sse = 0f64;
    for i in 0..n {
        let d = f64::from(a[i]) - f64::from(b[i]);
        sse += d * d;
    }
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0 * 255.0 * n as f64 / sse).log10()
}

const VARIANTS: &[(&str, &[&str])] = &[
    ("plain", &[]),
    ("intra_vlc", &["-intra_vlc", "1"]),
    ("alt_scan_nl", &["-alternate_scan", "1", "-non_linear_quant", "1", "-qmax", "28"]),
    ("interlaced", &["-flags", "+ildct+ilme"]),
    ("interlaced_alt", &["-flags", "+ildct+ilme", "-alternate_scan", "1", "-intra_vlc", "1", "-non_linear_quant", "1", "-qmax", "28"]),
    ("trellis_mv", &["-mbd", "rd", "-trellis", "2", "-cmp", "2", "-subcmp", "2"]),
];

#[test]
fn decoder_matches_reference() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("decoder");
    for (name, extra) in VARIANTS {
        let es_path = w.join(format!("{name}.m2v"));
        make_es(&ff, &es_path, extra, "3");
        let es = std::fs::read(&es_path).unwrap();
        let (reference, err) = ff_decode(&ff, &es_path);
        assert!(err.is_empty(), "{name}: {err}");
        let mine = my_decode(&es);
        assert_eq!(mine.len(), reference.len(), "{name}: andere Bildanzahl");
        let p = psnr(&mine, &reference);
        println!("{name}: Decoder gegen Referenz {p:.1} dB");
        assert!(p > 45.0, "{name}: {p:.1} dB");
    }
}

#[test]
fn requant_stays_valid_and_hits_target() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("requant");
    for (name, extra) in VARIANTS {
        let es_path = w.join(format!("{name}.m2v"));
        make_es(&ff, &es_path, extra, "6");
        let es = std::fs::read(&es_path).unwrap();
        let (reference, _) = ff_decode(&ff, &es_path);

        for ratio in [0.8f32, 0.6] {
            let mut rq = Requant::new();
            rq.set_opts(VideoOpts { ratio, smooth_cutoff: 64, still: false });
            let mut out = Vec::new();
            for gop in split_es(&es) {
                let r = rq.process(gop);
                out.extend_from_slice(&r.es);
            }
            let got = out.len() as f64 / es.len() as f64;
            let out_path = w.join(format!("{name}_{ratio}.m2v"));
            std::fs::write(&out_path, &out).unwrap();
            let (decoded, err) = ff_decode(&ff, &out_path);
            let p = psnr(&decoded, &reference);
            let mine = psnr(&my_decode(&out), &decoded);
            println!(
                "{name} Ziel {ratio}: erreicht {got:.3}, {p:.1} dB, Fallback {}/{}, eigener Decoder {mine:.1} dB",
                rq.slices_fallback, rq.slices_total
            );
            assert!(err.is_empty(), "{name} {ratio}: {err}");
            assert_eq!(decoded.len(), reference.len());
            assert_eq!(rq.slices_fallback, 0, "{name}: Slices nicht verstanden");
            assert!((got - f64::from(ratio)).abs() < 0.12, "{name}: Ziel {ratio}, erreicht {got}");
            assert!(p > 24.0, "{name}: {p:.1} dB");
        }

        // Unverändert heißt: Byte für Byte gleich.
        let mut rq = Requant::new();
        rq.set_opts(VideoOpts { ratio: 1.0, smooth_cutoff: 64, still: false });
        let out = rq.process(&es);
        assert_eq!(out.es, es);
    }
}

#[test]
fn smoothing_and_still_stay_valid() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("still");
    let es_path = w.join("a.m2v");
    make_es(&ff, &es_path, &[], "6");
    let es = std::fs::read(&es_path).unwrap();
    let (reference, _) = ff_decode(&ff, &es_path);

    for (name, opts) in [
        ("smooth", VideoOpts { ratio: 1.0, smooth_cutoff: 18, still: false }),
        ("still", VideoOpts { ratio: 0.12, smooth_cutoff: 64, still: true }),
    ] {
        let mut rq = Requant::new();
        rq.set_opts(opts);
        let mut out = Vec::new();
        for gop in split_es(&es) {
            out.extend_from_slice(&rq.process(gop).es);
        }
        let p = w.join(format!("{name}.m2v"));
        std::fs::write(&p, &out).unwrap();
        let (decoded, err) = ff_decode(&ff, &p);
        println!("{name}: {:.3} der Größe, {} Bytes dekodiert, Meldungen: {err}", out.len() as f64 / es.len() as f64, decoded.len());
        assert!(err.is_empty(), "{name}: {err}");
        assert_eq!(decoded.len(), reference.len(), "{name}: Bildanzahl");
        assert!(out.len() < es.len());
    }
}

fn split_es(es: &[u8]) -> Vec<&[u8]> {
    let mut cuts = vec![0usize];
    let mut i = 0;
    let mut first = true;
    while let Some(p) = discslim::mpeg2::bits::next_start_code(es, i) {
        if p + 4 > es.len() {
            break;
        }
        if es[p + 3] == 0xB3 || (es[p + 3] == 0xB8 && first) {
            if !first {
                cuts.push(p);
            }
            first = false;
        }
        i = p + 4;
    }
    cuts.push(es.len());
    cuts.windows(2).map(|w| &es[w[0]..w[1]]).collect()
}

fn fixture(w: &Path, ff: &Path) -> PathBuf {
    let main_path = w.join("main.m2v");
    make_es(ff, &main_path, &["-flags", "+ildct+ilme"], "20");
    let extra_path = w.join("extra.m2v");
    make_es(ff, &extra_path, &[], "6");
    let t = vec![TrackSpec { lang: b"de", freq: 440.0 }, TrackSpec { lang: b"en", freq: 660.0 }];
    let main = TitleSpec {
        es: std::fs::read(&main_path).unwrap(),
        gops_per_chapter: 12,
        audio: t.clone(),
        subs: vec![b"de", b"en"],
        fps: 25.0,
        wide: true,
    };
    let extra = TitleSpec {
        es: std::fs::read(&extra_path).unwrap(),
        gops_per_chapter: 1000,
        audio: t[..1].to_vec(),
        subs: Vec::new(),
        fps: 25.0,
        wide: true,
    };
    let menu = TitleSpec {
        es: std::fs::read(&extra_path).unwrap(),
        gops_per_chapter: 1000,
        audio: Vec::new(),
        subs: Vec::new(),
        fps: 25.0,
        wide: true,
    };
    let dir = w.join("source").join("VIDEO_TS");
    demo::author_disc(&dir, vec![(vec![main, extra], Some(menu))], None, true).unwrap();
    dir
}

fn open_analysed(path: &Path, full: bool) -> Disc {
    let mut d = Disc::open(Source::open(path).unwrap()).unwrap();
    d.analyse(full, &mut |_, _| true).unwrap();
    d
}

fn run_backup(disc: Disc, plan: Plan, output: Output) -> discslim::engine::backup::BackupSummary {
    let (tx, rx) = channel();
    run(
        BackupRequest { disc: Arc::new(disc), plan, output, deep_analysis: false },
        tx,
        Arc::new(AtomicBool::new(false)),
    );
    for m in rx.iter() {
        match m {
            BackupMsg::Done(s) => return s,
            BackupMsg::Failed(e) => panic!("Backup fehlgeschlagen: {e}"),
            BackupMsg::Cancelled => panic!("abgebrochen"),
            _ => {}
        }
    }
    panic!("keine Rückmeldung");
}

/// Prüft die innere Stimmigkeit einer DVD: VOBU-Tabellen, Zellen, Sprungverweise.
fn check_structure(path: &Path) {
    let disc = Disc::open(Source::open(path).unwrap()).unwrap();
    let mut acc = u32::try_from(disc.vmg.raw.len() / SECTOR * 2).unwrap();
    acc += disc.source.reader(Space::menu(0)).unwrap().sectors() as u32;
    assert_eq!(be32(&disc.vmg.raw, 0x0C), acc - 1, "VMG-Größe");

    for (n, v) in &disc.vts {
        let tt = disc.vmg.titles.iter().find(|t| t.vts == *n).expect("Titel zum Titelset");
        assert_eq!(tt.vts_sector, acc, "Lage von Titelset {n}");

        let menu_sectors = disc.source.reader(Space::menu(*n)).unwrap().sectors() as u32;
        let title_sectors = disc.source.reader(Space::title(*n)).unwrap().sectors() as u32;
        let ifo = (v.raw.len() / SECTOR) as u32;
        let size = ifo * 2 + menu_sectors + title_sectors;
        assert_eq!(be32(&v.raw, 0x0C), size - 1, "Größe Titelset {n}");
        if menu_sectors > 0 {
            assert_eq!(be32(&v.raw, 0xC0), ifo);
        }
        assert_eq!(be32(&v.raw, 0xC4), ifo + menu_sectors);
        acc += size;

        for (space, admap, cells, total) in [
            (Space::title(*n), &v.admap, &v.cells, title_sectors),
            (Space::menu(*n), &v.menu_admap, &v.menu_cells, menu_sectors),
        ] {
            let mut r = disc.source.reader(space).unwrap();
            let mut buf = Vec::new();
            let mut set = HashSet::new();
            assert_eq!(r.sectors() as u32, total);
            for (i, a) in admap.iter().enumerate() {
                r.read(u64::from(*a), 1, &mut buf).unwrap();
                assert!(is_nav(&buf), "VOBU-Tabelle zeigt nicht auf ein NAV-Paket: {a}");
                assert_eq!(Nav::lbn(&buf), *a, "LBN");
                let ea = Nav::vobu_ea(&buf);
                let next = admap.get(i + 1).copied().unwrap_or(total);
                assert_eq!(*a + ea + 1, next, "VOBU-Ende bei {a}");
                for k in 0..3 {
                    let v = be32(&buf, DSI_OFF + 12 + k * 4);
                    assert!(v <= ea, "Referenzende hinter der VOBU");
                }
                let nv = be32(&buf, DSI_OFF + DSI_SRI + 80) & 0x3FFF_FFFF;
                if nv != SRI_END && nv != 0 {
                    assert_eq!(*a + nv, next, "Sprung zur nächsten VOBU");
                }
                let pv = be32(&buf, DSI_OFF + DSI_SRI + 84) & 0x3FFF_FFFF;
                if pv != SRI_END && pv != 0 && i > 0 {
                    assert_eq!(a - pv, admap[i - 1], "Sprung zur vorigen VOBU");
                }
                set.insert(*a);
            }
            let mut last_end = None;
            for c in cells.iter() {
                assert!(set.contains(&c.start), "Zellstart ist keine VOBU");
                if let Some(e) = last_end {
                    assert_eq!(c.start, e + 1, "Lücke zwischen Zellen");
                }
                last_end = Some(c.last);
            }
            if let Some(e) = last_end {
                assert_eq!(e + 1, total, "Zellen decken die Zone nicht ab");
            }
        }

        for p in &v.pgcs {
            for c in &p.cells {
                assert!(v.cells.iter().any(|a| a.start == c.first_sector && a.last == c.last_sector), "Zelle unbekannt");
                assert!(v.admap.contains(&c.last_vobu_start), "letzte VOBU ist keine VOBU");
            }
        }
    }
}

#[test]
fn full_disc_end_to_end() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("fulldisc");
    let src = fixture(&w, &ff);
    check_structure(&src);

    let disc = open_analysed(&src, true);
    assert_eq!(disc.titles.len(), 2);
    assert_eq!(disc.titles[0].category, discslim::dvd::model::Category::MainMovie);
    assert!(disc.titles[0].chapters.len() >= 2, "Kapitel");
    assert_eq!(disc.titles[0].audio.len(), 2);
    assert_eq!(disc.titles[0].subs.len(), 2);
    assert!(!disc.check_scrambled());
    let total = disc.total_sectors;

    // 1) Nichts ändern: Das Video muss Byte für Byte gleich bleiben.
    let plan = Plan { target_sectors: total * 2, ..Plan::new(total * 2) };
    let out = w.join("same");
    run_backup(open_analysed(&src, true), plan, Output::Folder(out.clone()));
    let a = std::fs::read(src.join("VTS_01_1.VOB")).unwrap();
    let b = std::fs::read(out.join("VIDEO_TS").join("VTS_01_1.VOB")).unwrap();
    assert_eq!(a.len(), b.len());
    assert!(a == b, "unverändertes VOB weicht ab");
    check_structure(&out.join("VIDEO_TS"));

    // 2) Auf 70 % verkleinern
    let target = (total as f64 * 0.7) as u64;
    let mut plan = Plan::new(target);
    plan.default_video = VideoMode::Auto;
    let est = plan.estimate(&disc);
    assert!(est.fits);
    assert!(est.auto_ratio < 0.95, "Automatik sollte verkleinern: {}", est.auto_ratio);
    let out = w.join("small");
    let s = run_backup(open_analysed(&src, true), plan, Output::Folder(out.clone()));
    println!("Ziel {target}, erreicht {} Sektoren von {total}", s.sectors);
    assert!(s.fits, "{} > {target}", s.sectors);
    assert!(s.sectors as f64 > target as f64 * 0.75, "viel zu klein: {}", s.sectors);
    check_structure(&out.join("VIDEO_TS"));

    let (reference, _) = ff_decode(&ff, &src.join("VTS_01_1.VOB"));
    let (decoded, err) = ff_decode(&ff, &out.join("VIDEO_TS").join("VTS_01_1.VOB"));
    println!("Meldungen des Referenz-Decoders: {err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(decoded.len(), reference.len());
    let p = psnr(&decoded, &reference);
    println!("Ganze DVD auf 70 %: {p:.1} dB");
    assert!(p > 25.0);

    // 3) Tonspur und Untertitel abwählen, Menü als Standbild
    let mut plan = Plan::new(total * 2);
    let mut s0 = ItemSettings { video: VideoMode::Keep, ..Default::default() };
    s0.audio[1] = false;
    s0.subs[1] = false;
    plan.items.insert(ItemId::Title(0), s0);
    plan.items.insert(ItemId::Title(1), ItemSettings { video: VideoMode::Custom(0.5), ..Default::default() });
    plan.items.insert(ItemId::Menu(1), ItemSettings { video: VideoMode::Still, ..Default::default() });
    let out = w.join("tracks");
    run_backup(open_analysed(&src, true), plan, Output::Folder(out.clone()));
    let vdir = out.join("VIDEO_TS");
    check_structure(&vdir);
    let re = open_analysed(&vdir, true);
    assert_eq!(re.titles[0].audio.len(), 1, "Tonspur abgewählt");
    assert_eq!(re.titles[0].subs.len(), 1, "Untertitel abgewählt");
    let st = re.title_stats(&re.titles[0]);
    assert_eq!(st.audio[1], 0.0, "Tondaten entfernt");
    assert_eq!(st.sub[1], 0.0, "Untertiteldaten entfernt");
    assert!(st.audio[0] > 0.0);
    let (d, err) = ff_decode(&ff, &vdir.join("VTS_01_0.VOB"));
    assert!(err.is_empty(), "Menü: {err}");
    assert!(!d.is_empty());
}

#[test]
fn iso_and_reauthor() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("iso");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, false);
    let total = disc.total_sectors;

    // ISO schreiben und über den eigenen Leser wieder öffnen
    let iso = w.join("disc.iso");
    let mut plan = Plan::new((total as f64 * 0.8) as u64);
    plan.default_video = VideoMode::Auto;
    run_backup(open_analysed(&src, false), plan, Output::Iso(iso.clone()));
    assert!(iso.exists());
    assert!(!w.join("disc.iso.arbeitsordner").exists(), "Arbeitsordner bleibt liegen");
    let re = open_analysed(&iso, false);
    assert_eq!(re.titles.len(), 2);
    assert_eq!(re.source.label, "SOURCE");
    // Die Tabelle nennt die Lage der Titelsets relativ zu VIDEO_TS.IFO; das muss im Abbild stimmen.
    let offset = |name: &str| match re.source.files.get(name) {
        Some(discslim::dvd::source::FileRef::Iso { offset, .. }) => *offset,
        _ => panic!("{name} fehlt"),
    };
    let first = offset("VIDEO_TS.IFO");
    let vts_at = u64::from(re.vmg.titles[0].vts_sector) * SECTOR as u64;
    assert_eq!(offset("VTS_01_0.IFO") - first, vts_at, "Titelset liegt anders als angegeben");

    // Neu zusammenstellen: nur zwei Kapitel des Hauptfilms, dazu das Extra
    let mut plan = Plan::new(total);
    plan.mode = Mode::Reauthor;
    let main = disc.titles.iter().position(|t| t.category == discslim::dvd::model::Category::MainMovie).unwrap();
    let extra = 1 - main;
    let mut a = ItemSettings { video: VideoMode::Custom(0.7), ..Default::default() };
    a.audio[1] = false;
    plan.reauthor.push(ReauthorEntry::new(main, 1, 2, a));
    plan.reauthor.push(ReauthorEntry::new(extra, 0, 0, ItemSettings { video: VideoMode::Keep, ..Default::default() }));
    let out = w.join("reauthor");
    run_backup(open_analysed(&src, true), plan, Output::Folder(out.clone()));
    let vdir = out.join("VIDEO_TS");
    let re = open_analysed(&vdir, true);
    check_structure(&vdir);
    assert_eq!(re.titles.len(), 2);
    assert_eq!(re.vts.len(), 2, "je Titel ein Titelset");
    let first = re.titles.iter().find(|t| t.number == 1).unwrap();
    assert_eq!(first.chapters.len(), 2, "zwei Kapitel übernommen");
    assert_eq!(first.audio.len(), 1, "eine Tonspur übrig");
    let (d, err) = ff_decode(&ff, &vdir.join("VTS_01_1.VOB"));
    assert!(err.is_empty(), "{err}");
    assert!(!d.is_empty());
}

#[test]
fn sample_disc_is_valid() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("sample");
    let dir = demo::build_sample(&w, |_| {}).unwrap();
    check_structure(&dir);
    let disc = open_analysed(&dir, true);
    println!("Beispiel-DVD: {} Sektoren, {} Titel", disc.total_sectors, disc.titles.len());
    assert_eq!(disc.titles.len(), 3);
    for name in ["VTS_01_1.VOB", "VTS_02_1.VOB", "VTS_01_0.VOB", "VIDEO_TS.VOB"] {
        let (d, err) = ff_decode(&ff, &dir.join(name));
        assert!(err.is_empty(), "{name}: {err}");
        assert!(!d.is_empty(), "{name}");
    }
    let mut plan = Plan::new((disc.total_sectors as f64 * 0.7) as u64);
    plan.default_video = VideoMode::Auto;
    let out = w.join("out");
    let s = run_backup(open_analysed(&dir, true), plan, Output::Folder(out.clone()));
    assert!(s.fits);
    check_structure(&out.join("VIDEO_TS"));
}

/// Fremd gemuxte DVD-Daten (mit AC-3-Ton und anderem Paketaufbau) müssen die Umwandlung
/// ebenso überstehen; Bildanzahl und Ton bleiben erhalten.
#[test]
fn foreign_mux_survives() {
    use discslim::dvd::source::Source;
    use discslim::engine::plan::{CellDecision, VideoMode};
    use discslim::engine::transcode::{transcode_vobu, VobuReader};

    let Some(ff) = ffmpeg() else { return };
    let w = work("foreign");
    let dir = w.join("VIDEO_TS");
    std::fs::create_dir_all(&dir).unwrap();
    let vob = dir.join("VTS_01_1.VOB");
    let st = Command::new(&ff)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=720x576:rate=25:duration=30"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=30:sample_rate=48000"])
        .args(["-map", "0:v", "-map", "1:a", "-map", "1:a"])
        .args(["-flags", "+ildct+ilme", "-target", "pal-dvd", "-b:v", "6M", "-ac", "2"])
        .arg(&vob)
        .status()
        .unwrap();
    assert!(st.success());

    // VIDEO_TS.IFO wird hier nicht gebraucht; die Quelle liest nur die VOB-Datei.
    std::fs::write(dir.join("VIDEO_TS.IFO"), [0u8; 2048]).unwrap();
    std::fs::write(dir.join("VTS_01_0.IFO"), [0u8; 2048]).unwrap();
    let source = Source::open(&dir).unwrap();
    let reader = source.reader(Space::title(1)).unwrap();
    let total = reader.sectors();
    let mut vr = VobuReader::new(reader, 0, total);
    let mut rq = Requant::new();
    let d = CellDecision { mode: VideoMode::Auto, ..CellDecision::keep_all() };
    let opts = VideoOpts { ratio: 0.65, smooth_cutoff: 64, still: false };
    let mut out = Vec::new();
    let mut vobus = 0;
    while let Some((_, data)) = vr.next().unwrap() {
        out.extend_from_slice(&transcode_vobu(&data, &d, opts, &mut rq).data);
        vobus += 1;
    }
    println!("{vobus} VOBUs, {total} Sektoren, danach {}, Fallback {}/{}", out.len() / SECTOR, rq.slices_fallback, rq.slices_total);
    assert!(vobus > 10);
    assert_eq!(rq.slices_fallback, 0);
    assert!((out.len() / SECTOR) < total as usize * 95 / 100);
    let result = w.join("out.vob");
    std::fs::write(&result, &out).unwrap();

    let (a, _) = ff_decode(&ff, &vob);
    let (b, err) = ff_decode(&ff, &result);
    println!("Meldungen: {err}");
    assert!(err.is_empty(), "{err}");
    assert_eq!(a.len(), b.len(), "Bildanzahl");
    println!("Bildqualität: {:.1} dB", psnr(&a, &b));

    // Beide Tonspuren müssen vollständig dekodierbar sein.
    let mut sizes = [[0usize; 2]; 2];
    for (run, path) in [&vob, &result].into_iter().enumerate() {
        for track in 0..2 {
            let o = Command::new(&ff)
                .args(["-v", "fatal", "-i"])
                .arg(path)
                .args(["-map", &format!("0:a:{track}"), "-f", "s16le", "-"])
                .output()
                .unwrap();
            assert!(o.status.success());
            sizes[run][track] = o.stdout.len();
        }
    }
    println!("Ton vorher {:?}, nachher {:?}", sizes[0], sizes[1]);
    assert!(sizes[0][0] > 1_000_000);
    assert_eq!(sizes[0], sizes[1], "Tonspuren müssen vollständig bleiben");
}

/// Abbrechen räumt auf, und ein Backup darf seine eigene Quelle nie überschreiben.
#[test]
fn cancel_and_source_guard() {
    let w = work("guards");
    let src = demo::build_sample(&w, |_| {}).unwrap();
    let total_before = std::fs::read_dir(&src).unwrap().count();

    let target = w.join("abbruch");
    let (tx, rx) = channel();
    let disc = open_analysed(&src, false);
    let plan = Plan::new(disc.total_sectors);
    run(
        BackupRequest { disc: Arc::new(disc), plan, output: Output::Folder(target.clone()), deep_analysis: false },
        tx,
        Arc::new(AtomicBool::new(true)),
    );
    assert!(rx.iter().any(|m| matches!(m, BackupMsg::Cancelled)), "Abbruch nicht gemeldet");
    assert!(!target.exists(), "Abbruch lässt Dateien liegen");

    let (tx, rx) = channel();
    let disc = open_analysed(&src, false);
    let plan = Plan::new(disc.total_sectors);
    run(
        BackupRequest { disc: Arc::new(disc), plan, output: Output::Folder(w.clone()), deep_analysis: false },
        tx,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(rx.iter().any(|m| matches!(m, BackupMsg::Failed(_))), "Quelle wäre überschrieben worden");
    assert_eq!(std::fs::read_dir(&src).unwrap().count(), total_before, "Quelle verändert");
    assert!(src.join("VTS_01_1.VOB").exists());
}

/// Gewählte Titel als eine einzige Datei: alles dekodierbar, Zeitstempel durchgehend.
#[test]
fn single_vob_file() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("vobfile");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, true);
    let total = disc.total_sectors;

    // 1) Hauptfilm und Extra ohne Größengrenze, zweite Tonspur abgewählt
    let mut base = Plan::new(total);
    let mut s0 = ItemSettings { video: VideoMode::Auto, ..Default::default() };
    s0.audio[1] = false;
    s0.subs[1] = false;
    base.items.insert(ItemId::Title(0), s0);
    let plan = base.for_single_file(&disc, &[true, true], false);
    assert_eq!(plan.reauthor.len(), 2);
    let out = w.join("ziel").join("alles.vob");
    let s = run_backup(open_analysed(&src, true), plan, Output::Vob(out.clone()));
    assert!(out.exists());
    assert_eq!(s.sectors * SECTOR as u64, std::fs::metadata(&out).unwrap().len());

    // Eine Datei mit beiden Titeln: Bildanzahl stimmt, keine Fehlermeldung.
    let (a, _) = ff_decode(&ff, &w.join("main.m2v"));
    let (b, _) = ff_decode(&ff, &w.join("extra.m2v"));
    let (all, err) = ff_decode(&ff, &out);
    assert!(err.is_empty(), "{err}");
    assert_eq!(all.len(), a.len() + b.len(), "Bildanzahl");
    // Ohne Grenze bleibt das Bild unverändert.
    let joined: Vec<u8> = a.iter().chain(b.iter()).copied().collect();
    let p = psnr(&all, &joined);
    println!("Eine Datei, Original: {p:.1} dB");
    assert!(p > 90.0, "{p:.1} dB");

    // Zeit läuft durch, die Sprungverweise zeigen in die eigene Datei.
    let data = std::fs::read(&out).unwrap();
    let mut last_scr = 0u64;
    let mut last_end: Option<u32> = None;
    let mut last_nav: Option<u32> = None;
    let mut navs = 0;
    for (i, sec) in data.chunks_exact(SECTOR).enumerate() {
        assert!(is_pack(sec), "Sektor {i} ist kein Paket");
        let scr = get_scr(sec);
        assert!(scr > last_scr, "Systemuhr springt zurück bei Sektor {i}");
        last_scr = scr;
        assert!(!matches!(classify(sec), Stream::Audio(1) | Stream::Sub(1)), "abgewählte Spur noch da");
        if is_nav(sec) {
            navs += 1;
            assert_eq!(Nav::lbn(sec), i as u32, "NAV-Sektornummer");
            let s_ptm = be32(sec, PCI_OFF + 12);
            if let Some(e) = last_end {
                let gap = i64::from(s_ptm) - i64::from(e);
                assert!(gap.abs() < 45_000, "Zeitsprung {gap} bei Sektor {i}");
            }
            last_end = Some(be32(sec, PCI_OFF + 16));
            let back = be32(sec, DSI_OFF + DSI_SRI + 84) & 0x3FFF_FFFF;
            if back != SRI_END && back != 0 {
                assert_eq!(i as u32 - back, last_nav.unwrap(), "Verweis zurück");
            }
            last_nav = Some(i as u32);
        }
    }
    assert!(navs > 10);

    // 2) Mit Zielgröße: Die Automatik verkleinert nur für diese Datei.
    let mut limited = Plan::new((total as f64 * 0.5) as u64);
    limited.mode = Mode::Reauthor;
    limited.reauthor.push(ReauthorEntry::new(0, 1, 2, ItemSettings { video: VideoMode::Auto, ..Default::default() }));
    let plan = limited.for_single_file(&disc, &[true], true);
    let est = plan.estimate(&disc);
    let small_target = (est.source_total * 0.7) as u64;
    let mut plan = plan;
    plan.target_sectors = small_target;
    let out2 = w.join("kapitel.vob");
    let s = run_backup(open_analysed(&src, true), plan, Output::Vob(out2.clone()));
    println!("Kapitelauswahl: Ziel {small_target}, erreicht {}", s.sectors);
    assert!(s.fits, "{} > {small_target}", s.sectors);
    let (part, err) = ff_decode(&ff, &out2);
    assert!(err.is_empty(), "{err}");
    assert!(!part.is_empty() && part.len() < a.len());

    // 3) Quelle darf nicht überschrieben werden.
    let (tx, rx) = channel();
    let d = open_analysed(&src, false);
    let plan = Plan::new(total).for_single_file(&d, &[true, false], false);
    let before = std::fs::metadata(src.join("VTS_01_1.VOB")).unwrap().len();
    run(
        BackupRequest { disc: Arc::new(d), plan, output: Output::Vob(src.join("VTS_01_1.VOB")), deep_analysis: false },
        tx,
        Arc::new(AtomicBool::new(false)),
    );
    assert!(rx.iter().any(|m| matches!(m, BackupMsg::Failed(_))));
    assert_eq!(std::fs::metadata(src.join("VTS_01_1.VOB")).unwrap().len(), before);
}

/// Ohne Teilung entsteht eine einzige Datei; die DVD-Tabellen bleiben stimmig.
#[test]
fn unsplit_structure() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("unsplit");
    let src = fixture(&w, &ff);

    // Der Schreiber allein: knapp über die Teilungsgrenze hinaus.
    let chunk = vec![0u8; SECTOR * 1024];
    for (split, parts) in [(true, 2usize), (false, 1)] {
        let dir = w.join(format!("schreiber{split}"));
        std::fs::create_dir_all(&dir).unwrap();
        let mut sw = discslim::engine::transcode::SpaceWriter::new(&dir, "VTS_01_", false, split);
        for _ in 0..513 {
            sw.write(&chunk).unwrap();
        }
        sw.finish().unwrap();
        assert_eq!(sw.paths().len(), parts);
    }

    // Eine DVD ohne Teilung bleibt lesbar und in sich stimmig, auch als ISO.
    let mut plan = Plan::new(open_analysed(&src, false).total_sectors);
    plan.split_vob = false;
    let out = w.join("ordner");
    run_backup(open_analysed(&src, false), plan.clone(), Output::Folder(out.clone()));
    check_structure(&out.join("VIDEO_TS"));
    let iso = w.join("voll.iso");
    run_backup(open_analysed(&src, false), plan, Output::Iso(iso.clone()));
    let re = open_analysed(&iso, false);
    assert_eq!(re.titles.len(), 2);
}

/// Über 1 GB und über 4 GB: mehrere Abschnitte im ISO, trotzdem eine Datei.
/// Schreibt mehrere GB, deshalb nur auf Anforderung.
#[test]
#[ignore]
fn iso_with_huge_file() {
    let w = work("hugeiso");
    let dir = w.join("VIDEO_TS");
    std::fs::create_dir_all(&dir).unwrap();
    let ifo = dir.join("VIDEO_TS.IFO");
    std::fs::write(&ifo, vec![7u8; SECTOR]).unwrap();
    let vob = dir.join("VTS_01_1.VOB");
    let size = (4u64 << 30) + (600 << 20);
    let f = std::fs::File::create(&vob).unwrap();
    f.set_len(size).unwrap();
    drop(f);
    let out = w.join("gross.iso");
    discslim::dvd::iso::write_iso(&out, "GROSS", &[ifo, vob], |_, _| true).unwrap();
    let (label, entries) = discslim::dvd::iso::read_video_ts(&out).unwrap();
    assert_eq!(label, "GROSS");
    let e = entries.iter().find(|e| e.0 == "VTS_01_1.VOB").unwrap();
    assert_eq!(e.2, size);
}

fn frames_of(yuv: &[u8]) -> usize {
    yuv.len() / (720 * 576 * 3 / 2)
}

/// Schneiden: am Schlüsselbild ohne Neukodierung, bildgenau mit neu kodierten Schnittstellen.
#[test]
fn cutting_titles() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("cut");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, true);
    let main = disc.titles.iter().position(|t| t.category == discslim::dvd::model::Category::MainMovie).unwrap();
    let last = disc.titles[main].chapters.len() - 1;
    let (reference, _) = ff_decode(&ff, &w.join("main.m2v"));
    let fs = 720 * 576 * 3 / 2;
    // Schnitt von 3,12 s bis 9,48 s: mitten in Bildgruppen (eine Gruppe dauert 0,48 s)
    let (cut_in, cut_out) = (3_120u64, 9_480u64);
    let want = ((cut_out - cut_in) / 40) as usize;

    for exact in [false, true] {
        let mut e = ReauthorEntry::new(main, 0, last, ItemSettings { video: VideoMode::Keep, ..Default::default() });
        e.cut = Cut { start_ms: Some(cut_in), end_ms: Some(cut_out), exact };
        let mut plan = Plan::new(disc.total_sectors);
        plan.mode = Mode::Reauthor;
        plan.reauthor.push(e);

        // Als DVD: Struktur stimmt, alles dekodierbar
        let out = w.join(format!("dvd_{exact}"));
        run_backup(open_analysed(&src, true), plan.clone(), Output::Folder(out.clone()));
        let vdir = out.join("VIDEO_TS");
        check_structure(&vdir);
        let re = open_analysed(&vdir, true);
        let ms = re.titles[0].ms;
        let (dvd, err) = ff_decode(&ff, &vdir.join("VTS_01_1.VOB"));
        assert!(err.is_empty(), "{err}");
        println!("bildgenau={exact}: DVD {} Bilder, Länge laut Tabelle {ms} ms", frames_of(&dvd));

        // Als einzelne Datei: Anzahl und Inhalt der Bilder prüfen
        let vob = w.join(format!("schnitt_{exact}.vob"));
        let file_plan = plan.for_single_file(&disc, &[true], false);
        run_backup(open_analysed(&src, true), file_plan, Output::Vob(vob.clone()));
        let (got, err) = ff_decode(&ff, &vob);
        assert!(err.is_empty(), "{err}");
        let n = frames_of(&got);
        println!("bildgenau={exact}: Datei {n} Bilder, erwartet {want}");
        // Der Ton bleibt erhalten und ist so lang wie das Bild
        let audio = Command::new(&ff)
            .args(["-v", "error", "-i"])
            .arg(&vob)
            .args(["-map", "0:a:0", "-f", "s16le", "-ac", "2", "-ar", "48000", "-"])
            .output()
            .unwrap()
            .stdout;
        let audio_ms = audio.len() as f64 / 4.0 / 48.0;
        println!("bildgenau={exact}: Ton {audio_ms:.0} ms bei {} ms Bild", n * 40);
        assert!((audio_ms - (n * 40) as f64).abs() < 700.0, "Tonlänge {audio_ms}");
        if exact {
            assert!((n as i64 - want as i64).abs() <= 1, "{n} statt {want}");
            let first = (cut_in / 40) as usize;
            let a = psnr(&got[..fs], &reference[first * fs..(first + 1) * fs]);
            let z = psnr(&got[(n - 1) * fs..n * fs], &reference[(first + n - 1) * fs..(first + n) * fs]);
            let mid = psnr(&got[(n / 2) * fs..(n / 2 + 1) * fs], &reference[(first + n / 2) * fs..(first + n / 2 + 1) * fs]);
            println!("erstes Bild {a:.1} dB, Mitte {mid:.1} dB, letztes Bild {z:.1} dB");
            assert!(a > 28.0 && z > 28.0 && mid > 60.0);
            assert!((ms as i64 - (cut_out - cut_in) as i64).abs() < 200, "Länge {ms}");
        } else {
            // Schlüsselbild-Schnitt: alles Gewünschte ist drin, höchstens eine Bildgruppe mehr je Seite
            assert!(n >= want && n <= want + 2 * 12 + 2, "{n}");
            assert!(dvd.len() >= want * fs);
        }
    }
}

/// Vorschau-Ton: Dolby Digital über den Windows-Decoder. Länge und Pegel müssen zum
/// Referenz-Decoder passen, die Zeiten lückenlos weiterlaufen.
#[cfg(windows)]
#[test]
fn preview_audio_decodes() {
    use discslim::dvd::ifo::AudioAttr;
    use discslim::engine::audio::{audio_pes, Codec, TrackChoice, TrackDecoder};
    let Some(ff) = ffmpeg() else { return };
    let w = work("ton");
    let vob = w.join("ton.vob");
    let st = Command::new(&ff)
        .args(["-y", "-v", "error", "-f", "lavfi", "-i", "testsrc2=size=720x576:rate=25:duration=8"])
        .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=8:sample_rate=48000"])
        .args(["-map", "0:v", "-map", "1:a", "-target", "pal-dvd", "-ac", "2"])
        .arg(&vob)
        .status()
        .unwrap();
    assert!(st.success());
    let reference = Command::new(&ff)
        .args(["-v", "error", "-i"])
        .arg(&vob)
        .args(["-map", "0:a:0", "-f", "s16le", "-ac", "2", "-ar", "48000", "-"])
        .output()
        .unwrap()
        .stdout;
    let rms = |s: &[i16]| (s.iter().map(|v| f64::from(*v) * f64::from(*v)).sum::<f64>() / s.len().max(1) as f64).sqrt();
    let ref_samples: Vec<i16> = reference.chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();

    let attr = AudioAttr { coding: 0, channels: 2, ..Default::default() };
    let track = TrackChoice { phys: 0, codec: Codec::of(&attr).unwrap(), channels: 2 };
    let Some(mut dec) = TrackDecoder::new(&track) else {
        println!("Kein Dolby-Digital-Decoder in diesem Windows, Test übersprungen");
        return;
    };
    let data = std::fs::read(&vob).unwrap();
    let mut out = Vec::new();
    for s in data.chunks_exact(SECTOR) {
        if let Some(p) = audio_pes(s, &track) {
            let t = p.pts.map(|v| v as f64 / 90.0);
            dec.feed(&p, t, &mut out);
        }
    }
    let mine: Vec<i16> = out.iter().flat_map(|c| c.1.iter().copied()).collect();
    println!("Ton: {} gegen {} Werte, Pegel {:.0} gegen {:.0}", mine.len(), ref_samples.len(), rms(&mine), rms(&ref_samples));
    assert!(mine.len() as f64 > ref_samples.len() as f64 * 0.9, "zu wenig Ton");
    assert!((rms(&mine) / rms(&ref_samples) - 1.0).abs() < 0.25, "Pegel weicht ab");
    // Die letzten Rahmen am Dateiende ausgenommen (dort endet der Strom mitten im Rahmen)
    for wnd in out[..out.len().saturating_sub(5)].windows(2) {
        let end = wnd[0].0 + (wnd[0].1.len() / 2) as f64 / 48.0;
        assert!((wnd[1].0 - end).abs() < 2.0, "Lücke im Ton bei {}: erwartet {end}", wnd[1].0);
    }
}

/// MPEG-2-Ausgabe: nur Bild und genau die gewählte Tonspur, keine Navigation, keine
/// Untertitel, keine Füllpakete; der Referenz-Decoder liest die Datei fehlerfrei.
#[test]
fn mpeg_file_output() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("mpeg");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, true);
    let plan = Plan::new(disc.total_sectors).for_single_file(&disc, &[true, true], false);
    let out = w.join("film.mpg");
    run_backup(open_analysed(&src, true), plan, Output::Mpeg { path: out.clone(), audio: Some(1) });

    let data = std::fs::read(&out).unwrap();
    let mut audio = [0usize; 8];
    let mut video = 0usize;
    for s in data.chunks_exact(SECTOR) {
        match classify(s) {
            Stream::Video => video += 1,
            Stream::Audio(a) => audio[a as usize] += 1,
            other => panic!("unerwartetes Paket: {other:?}"),
        }
    }
    println!("MPEG-2: {video} Bildpakete, Ton {audio:?}");
    assert!(video > 0);
    // Hauptfilm hat Spur 1 (englisch); das Extra nur Spur 0, dort ersatzweise diese
    assert!(audio[1] > 0, "gewählte Tonspur fehlt");
    assert_eq!(audio.iter().filter(|n| **n > 0).count(), 1, "genau eine Tonspur");

    let (a, _) = ff_decode(&ff, &w.join("main.m2v"));
    let (b, _) = ff_decode(&ff, &w.join("extra.m2v"));
    let (all, err) = ff_decode(&ff, &out);
    assert!(err.is_empty(), "{err}");
    assert_eq!(all.len(), a.len() + b.len(), "Bildanzahl");
    let info = Command::new(&ff).args(["-hide_banner", "-i"]).arg(&out).output().unwrap();
    let text = String::from_utf8_lossy(&info.stderr).to_string();
    let audio_streams = text.lines().filter(|l| l.contains("Stream #") && l.contains("Audio:")).count();
    let other = text.lines().filter(|l| l.contains("Stream #") && (l.contains("Subtitle:") || l.contains("Data:"))).count();
    println!("Ströme laut Referenz: {audio_streams} Ton, {other} sonstige");
    assert_eq!(audio_streams, 1);
    assert_eq!(other, 0, "Untertitel oder Daten in der MPEG-2-Datei");
}

/// Die schnelle Analyse liest nur Stichproben und liegt trotzdem nahe an der vollständigen.
#[test]
fn quick_analysis_is_close() {
    let Some(ff) = ffmpeg() else { return };
    let w = work("quick");
    let src = fixture(&w, &ff);
    let quick = open_analysed(&src, false);
    let full = open_analysed(&src, true);
    for (a, b) in quick.titles.iter().zip(full.titles.iter()) {
        let (q, f) = (quick.title_stats(a), full.title_stats(b));
        println!("Titel {}: Bild {:.0}/{:.0}, Ton {:.0}/{:.0}", a.number, q.video, f.video, q.audio[0], f.audio[0]);
        assert!((q.total - f.total).abs() < 1.0);
        assert!((q.video - f.video).abs() / f.video < 0.08, "Bild weicht ab");
        assert!((q.audio[0] - f.audio[0]).abs() / f.audio[0].max(1.0) < 0.15, "Ton weicht ab");
    }
}

/// Vorschau: bildgenaue und Schlüsselbild-Sprünge treffen die richtige Stelle.
#[test]
fn preview_seeking() {
    use discslim::engine::preview::{spawn, title_segments, PreviewCmd, PreviewFrame};
    let Some(ff) = ffmpeg() else { return };
    let w = work("seek");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, false);
    let main = disc.titles.iter().position(|t| t.category == discslim::dvd::model::Category::MainMovie).unwrap();
    let last = disc.titles[main].chapters.len() - 1;
    let segs = title_segments(&disc, main, 0, last);
    assert!(segs.iter().all(|s| !s.vobus.is_empty()));

    // Durchgehend abspielen und das Bild bei 3,12 s merken
    let h = spawn(disc.source.clone(), segs.clone(), 0.0, None, 0, None);
    let mut reference: Option<PreviewFrame> = None;
    let mut times = Vec::new();
    for f in h.frames.iter() {
        times.push(f.time_ms);
        if (f.time_ms - 3120.0).abs() < 1.0 {
            reference = Some(f);
            break;
        }
    }
    drop(h);
    let reference = reference.expect("Bild bei 3,12 s");
    assert!(times.windows(2).all(|w| (w[1] - w[0] - 40.0).abs() < 1.0), "Zeiten laufen nicht im Bildtakt");

    let next = |h: &discslim::engine::preview::PreviewHandle, id: u64| -> PreviewFrame {
        h.frames.iter().find(|f| f.seek_id == id).unwrap()
    };
    let h = spawn(disc.source.clone(), segs, 0.0, None, 0, None);
    let _ = h.frames.recv();

    // Bildgenau: genau dieses Bild, gleicher Inhalt wie beim Durchspielen
    h.cmd.send(PreviewCmd::SeekTime { ms: 3120.0, exact: true, id: 1 }).unwrap();
    let f = next(&h, 1);
    assert!((f.time_ms - 3120.0).abs() < 1.0, "{}", f.time_ms);
    assert!(f.rgba == reference.rgba, "Bild nach Sprung weicht ab");
    // Ein Bild zurück
    h.cmd.send(PreviewCmd::SeekTime { ms: 3080.0, exact: true, id: 2 }).unwrap();
    assert!((next(&h, 2).time_ms - 3080.0).abs() < 1.0);

    // Schlüsselbild: Abschnitt enthält die Zeit, gezeigt wird dessen erstes Bild
    h.cmd.send(PreviewCmd::SeekTime { ms: 3120.0, exact: false, id: 3 }).unwrap();
    let k = next(&h, 3);
    println!("Schlüsselbild: {} ({} bis {})", k.time_ms, k.key_t0, k.key_t1);
    assert!(k.key_t0 <= 3120.0 && 3120.0 < k.key_t1);
    assert!(k.time_ms >= k.key_t0 && k.time_ms < k.key_t0 + 3.0 * 40.0 + 1.0);
    // Nächstes Schlüsselbild
    h.cmd.send(PreviewCmd::SeekTime { ms: k.key_t1 + 20.0, exact: false, id: 4 }).unwrap();
    let k2 = next(&h, 4);
    assert!((k2.key_t0 - k.key_t1).abs() < 1.0, "{} statt {}", k2.key_t0, k.key_t1);
    // Zurück
    h.cmd.send(PreviewCmd::SeekTime { ms: k2.key_t0 - 1.0, exact: false, id: 5 }).unwrap();
    assert!((next(&h, 5).key_t0 - k.key_t0).abs() < 1.0);
}

/// Rückwärts abspielen: lückenlos im Bildtakt, auch über eine Zellgrenze hinweg, mit
/// denselben Bildern wie beim Vorwärtsabspielen.
#[test]
fn preview_reverse_playback() {
    use discslim::engine::preview::{spawn, title_segments, PreviewCmd, PreviewFrame};
    let Some(ff) = ffmpeg() else { return };
    let w = work("rueckwaerts");
    let src = fixture(&w, &ff);
    let disc = open_analysed(&src, false);
    let main = disc.titles.iter().position(|t| t.category == discslim::dvd::model::Category::MainMovie).unwrap();
    let last = disc.titles[main].chapters.len() - 1;
    let segs = title_segments(&disc, main, 0, last);
    assert!(segs.len() > 1, "Test braucht mehrere Zellen");
    let boundary = segs[0].ms as f64;

    // Vorwärts: Bilder um die Grenze herum merken
    let h = spawn(disc.source.clone(), segs.clone(), 0.0, None, 0, None);
    let mut forward: Vec<PreviewFrame> = Vec::new();
    for f in h.frames.iter() {
        if f.time_ms > boundary + 800.0 {
            break;
        }
        if f.time_ms > boundary - 1200.0 {
            forward.push(f);
        }
    }
    drop(h);

    // Rückwärts ab kurz hinter der Grenze
    let h = spawn(disc.source.clone(), segs, 0.0, None, 0, None);
    let start = boundary + 600.0;
    h.cmd.send(PreviewCmd::Reverse { ms: start, id: 7 }).unwrap();
    let mut back: Vec<PreviewFrame> = Vec::new();
    while back.len() < 40 {
        let f = h.frames.recv_timeout(std::time::Duration::from_secs(10)).expect("Rückwärts liefert keine Bilder mehr");
        if f.seek_id == 7 {
            back.push(f);
        }
    }
    println!("Rückwärts: {:.0} bis {:.0} ms (Grenze {boundary:.0})", back[0].time_ms, back.last().unwrap().time_ms);
    assert!((back[0].time_ms - (start - 40.0)).abs() < 1.0, "erstes Bild {}", back[0].time_ms);
    for w2 in back.windows(2) {
        assert!((w2[0].time_ms - w2[1].time_ms - 40.0).abs() < 1.0, "Lücke bei {}", w2[1].time_ms);
    }
    assert!(back.last().unwrap().time_ms < boundary - 500.0, "Grenze nicht überquert");
    // Gleiche Bilder wie vorwärts
    let mut compared = 0;
    for b in &back {
        if let Some(f) = forward.iter().find(|f| (f.time_ms - b.time_ms).abs() < 1.0) {
            assert!(f.rgba == b.rgba, "Bild bei {} weicht ab", b.time_ms);
            compared += 1;
        }
    }
    println!("{compared} Bilder verglichen");
    assert!(compared > 20);
}
