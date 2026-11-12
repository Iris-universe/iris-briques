//! Décodage des fichiers audio (MP3, AAC/M4A, FLAC, WAV, OGG, AIFF…) avec symphonia,
//! puis rééchantillonnage à la fréquence du moteur (rubato).
//!
//! `Err("UNSUPPORTED")` : format inconnu de symphonia (ex. Opus/WebM), à décoder autrement.
use symphonia::core::audio::SampleBuffer;
use symphonia::core::codecs::DecoderOptions;
use symphonia::core::errors::Error;
use symphonia::core::formats::FormatOptions;
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use symphonia::core::probe::Hint;

pub struct Pcm {
    pub l: Vec<f32>,
    pub r: Vec<f32>,
    pub sr: f64,
}

pub const MAX_MIN: f64 = 20.0;

/// `Err("UNSUPPORTED")` : format inconnu de symphonia (ex. Opus/WebM) -> l'interface décode à la place
pub fn decode(source: Box<dyn symphonia::core::io::MediaSource>, ext: Option<&str>) -> Result<Pcm, String> {
    let mss = MediaSourceStream::new(source, MediaSourceStreamOptions::default());
    let mut hint = Hint::new();
    if let Some(e) = ext {
        hint.with_extension(e);
    }
    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions { enable_gapless: true, ..Default::default() }, &MetadataOptions::default())
        .map_err(|_| "UNSUPPORTED".to_string())?;
    let mut format = probed.format;
    let track = format
        .tracks()
        .iter()
        .find(|t| t.codec_params.codec != symphonia::core::codecs::CODEC_TYPE_NULL)
        .ok_or("UNSUPPORTED")?
        .clone();
    let mut dec = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .map_err(|_| "UNSUPPORTED".to_string())?;
    let mut sr = track.codec_params.sample_rate.unwrap_or(0) as f64;
    let (mut l, mut r) = (Vec::new(), Vec::new());
    if let Some(n) = track.codec_params.n_frames {
        l.reserve(n as usize);
        r.reserve(n as usize);
    }
    let mut buf: Option<SampleBuffer<f32>> = None;
    let mut errors = 0;
    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(Error::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(Error::ResetRequired) => break,
            Err(e) => {
                if l.is_empty() {
                    return Err(format!("lecture impossible : {e}"));
                }
                break;
            }
        };
        if packet.track_id() != track.id {
            continue;
        }
        match dec.decode(&packet) {
            Ok(audio) => {
                let spec = *audio.spec();
                sr = spec.rate as f64;
                let ch = spec.channels.count().max(1);
                if buf.as_ref().map(|b| b.capacity() < audio.capacity() * ch).unwrap_or(true) {
                    buf = Some(SampleBuffer::new(audio.capacity() as u64, spec));
                }
                let b = buf.as_mut().unwrap();
                b.copy_interleaved_ref(audio);
                let s = b.samples();
                for f in s.chunks_exact(ch) {
                    l.push(f[0]);
                    r.push(if ch > 1 { f[1] } else { f[0] });
                }
                if l.len() as f64 > (MAX_MIN * 60.0 + 30.0) * sr.max(8000.0) {
                    return Err(format!("Morceau trop long (plus de {} min) : c'est sûrement un mix entier. Choisis un morceau seul.", MAX_MIN as u32));
                }
            }
            Err(Error::DecodeError(_)) => {
                errors += 1;
                if errors > 200 && l.is_empty() {
                    return Err("UNSUPPORTED".into());
                }
            }
            Err(_) => break,
        }
    }
    if l.is_empty() || sr <= 0.0 {
        return Err("UNSUPPORTED".into());
    }
    Ok(Pcm { l, r, sr })
}

pub fn decode_file(path: &std::path::Path) -> Result<Pcm, String> {
    let f = std::fs::File::open(path).map_err(|e| format!("fichier introuvable : {e}"))?;
    let ext = path.extension().and_then(|e| e.to_str()).map(|s| s.to_lowercase());
    decode(Box::new(f), ext.as_deref())
}

pub fn decode_bytes(bytes: Vec<u8>, ext: Option<&str>) -> Result<Pcm, String> {
    decode(Box::new(std::io::Cursor::new(bytes)), ext)
}

/// rééchantillonne vers `sr` (qualité FFT)
pub fn resample(p: Pcm, sr: f64) -> Pcm {
    if (p.sr - sr).abs() < 0.5 {
        return p;
    }
    use rubato::{FftFixedIn, Resampler};
    let chunk = 4096;
    let Ok(mut rs) = FftFixedIn::<f32>::new(p.sr as usize, sr as usize, chunk, 2, 2) else {
        return linear(p, sr);
    };
    let n = p.l.len();
    let expect = (n as f64 * sr / p.sr).round() as usize;
    let mut ol = Vec::with_capacity(expect + chunk * 2);
    let mut or = Vec::with_capacity(expect + chunk * 2);
    let delay = rs.output_delay();
    let mut i = 0;
    while i < n {
        let end = (i + chunk).min(n);
        let mut a = p.l[i..end].to_vec();
        let mut b = p.r[i..end].to_vec();
        a.resize(chunk, 0.0);
        b.resize(chunk, 0.0);
        if let Ok(out) = rs.process(&[a, b], None) {
            ol.extend_from_slice(&out[0]);
            or.extend_from_slice(&out[1]);
        }
        i = end;
    }
    // vide le filtre
    for _ in 0..2 {
        let z = vec![0f32; chunk];
        if let Ok(out) = rs.process(&[z.clone(), z], None) {
            ol.extend_from_slice(&out[0]);
            or.extend_from_slice(&out[1]);
        }
    }
    let ol: Vec<f32> = ol.into_iter().skip(delay).take(expect).collect();
    let or: Vec<f32> = or.into_iter().skip(delay).take(expect).collect();
    Pcm { l: ol, r: or, sr }
}

fn linear(p: Pcm, sr: f64) -> Pcm {
    let ratio = p.sr / sr;
    let n = ((p.l.len() as f64) / ratio) as usize;
    let get = |v: &Vec<f32>, x: f64| {
        let i = x as usize;
        let f = (x - i as f64) as f32;
        let a = v.get(i).copied().unwrap_or(0.0);
        let b = v.get(i + 1).copied().unwrap_or(a);
        a + (b - a) * f
    };
    let l = (0..n).map(|k| get(&p.l, k as f64 * ratio)).collect();
    let r = (0..n).map(|k| get(&p.r, k as f64 * ratio)).collect();
    Pcm { l, r, sr }
}
