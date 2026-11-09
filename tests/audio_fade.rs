//! Ton-Blenden direkt im Datenstrom (AC-3 mit Material aus dem Windows-Encoder).

#[cfg(windows)]
mod ac3 {
    use discslim::engine::audio::*;
    use discslim::engine::audiofade::ac3::{crc_ok, scale_frame};

    fn signal(ch: usize, frames: usize) -> Vec<f32> {
        let n = 1536 * frames;
        let mut seed = 777u32;
        let mut pcm = vec![0f32; n * ch];
        for i in 0..n {
            let t = i as f32 / 48_000.0;
            seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let noise = ((seed >> 9) as f32 / (1u32 << 23) as f32 - 0.5) * 0.05;
            let v = (t * 440.0 * std::f32::consts::TAU).sin() * 0.3
                + (t * 3_100.0 * std::f32::consts::TAU).sin() * 0.1
                + noise;
            for c in 0..ch {
                pcm[i * ch + c] = v / (c as f32 + 1.0);
            }
        }
        pcm
    }

    fn encode(ch: usize, kbps: u32) -> Vec<u8> {
        let mut enc = MfEncoder::new(ch as u32, kbps).expect("AC-3-Encoder");
        let mut es = Vec::new();
        enc.encode(&signal(ch, 60), &mut es);
        enc.finish(&mut es);
        es
    }

    fn decode(es: &[u8], ch: usize) -> Vec<f32> {
        let mut dec = MfDecoder::full(Codec::Ac3, ch.max(2) as u8)
            .or_else(|| MfDecoder::full(Codec::Ac3, 2))
            .expect("AC-3-Decoder");
        let mut out = Vec::new();
        for (s, l) in ac3_frames(es) {
            out.extend(dec.decode_raw(&es[s..s + l]));
        }
        out
    }

    fn rms(v: &[f32]) -> f64 {
        (v.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / v.len().max(1) as f64).sqrt()
    }

    fn check(ch: usize, kbps: u32) {
        let es = encode(ch, kbps);
        let frames = ac3_frames(&es);
        assert!(frames.len() >= 55, "{} Rahmen", frames.len());
        for &(s, l) in &frames {
            assert!(crc_ok(&es[s..s + l]), "Prüfsumme des Originals");
        }
        let orig = decode(&es, ch);
        let och = if orig.len() > 0 { orig.len() / (frames.len().saturating_sub(1) * 1536).max(1) } else { 2 };

        // Volle Lautstärke: Rahmen werden gelesen und bleiben Byte für Byte gleich,
        // auch mit neu berechneten Prüfsummen
        let mut unity = es.clone();
        for &(s, l) in &frames {
            assert!(scale_frame(&mut unity[s..s + l], &[1.0; 6]), "Rahmen nicht lesbar ({ch} Kanäle, {kbps} kBit/s)");
        }
        assert_eq!(unity, es, "Prüfsummen weichen ab ({ch} Kanäle, {kbps} kBit/s)");

        // Halbe Lautstärke und Stille
        for (g, want) in [(0.5f32, 0.5f64), (0.0, 0.0)] {
            let mut faded = es.clone();
            for &(s, l) in &frames {
                assert!(scale_frame(&mut faded[s..s + l], &[g; 6]), "Rahmen nicht lesbar ({ch} Kanäle, {kbps} kBit/s)");
                assert!(crc_ok(&faded[s..s + l]));
            }
            let dec = decode(&faded, ch);
            assert_eq!(dec.len(), orig.len());
            let skip = 3 * 1536 * och;
            let (a, b) = (rms(&orig[skip..]), rms(&dec[skip..]));
            let ratio = b / a;
            println!("{ch} Kanäle {kbps} kBit/s: Faktor {g} -> Pegel {ratio:.3} (Original {a:.4})");
            assert!((ratio - want).abs() < 0.06, "Pegel {ratio} statt {want}");
            if g > 0.0 {
                // Form bleibt erhalten: Abweichung vom halbierten Original klein
                let diff: Vec<f32> = orig[skip..].iter().zip(&dec[skip..]).map(|(o, d)| o * g - d).collect();
                let rel = rms(&diff) / (rms(&orig[skip..]) * f64::from(g));
                println!("  relative Abweichung {rel:.4}");
                assert!(rel < 0.1, "Abweichung {rel}");
            }
        }
    }

    #[test]
    fn ac3_stereo_192() {
        check(2, 192);
    }

    #[test]
    fn ac3_stereo_448() {
        check(2, 448);
    }

    #[test]
    fn ac3_mono_96() {
        check(1, 96);
    }
}
