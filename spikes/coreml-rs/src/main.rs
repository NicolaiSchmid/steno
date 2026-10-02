mod coreml;
mod decoder;
mod pipeline;
mod wav;

use coreml::{output, parse_units, provider, Array, Model};
use std::time::Instant;

fn usage() -> ! {
    eprintln!("usage:\n  coreml-rs bench-encoder <models-dir> [units: all|cpu-ane|cpu-gpu|cpu] [iters] [wav]\n  coreml-rs transcribe <models-dir> <out-dir> <wav>... [--encoder-units U]");
    std::process::exit(2)
}

fn bench_encoder(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("models dir")?;
    let units = parse_units(args.get(1).map(String::as_str).unwrap_or("all"));
    let iters: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let mel = Array::zeros_f32(&[1, 128, 1501])?;
    let mel_len = Array::zeros_i32(&[1])?;
    mel_len.i32_mut()[0] = 1501;
    if let Some(wav) = args.get(3) {
        // real mel from the first 15 s through the Preprocessor (cpuOnly, as FluidAudio)
        let audio = wav::read_wav_mono_i16(wav)?;
        let pre = Model::load(&format!("{dir}/Preprocessor.mlmodelc"), objc2_core_ml::MLComputeUnits::CPUOnly)?;
        let a = Array::zeros_f32(&[1, pipeline::MAX_MODEL_SAMPLES])?;
        let n = audio.len().min(pipeline::MAX_MODEL_SAMPLES);
        a.f32_mut()[..n].copy_from_slice(&audio[..n]);
        let l = Array::zeros_i32(&[1])?;
        l.i32_mut()[0] = n as i32;
        let pre_in = provider(&[("audio_signal", &a), ("audio_length", &l)])?;
        let out = pre.predict(&pre_in)?;
        let m = output(&out, "mel")?;
        mel.f32_mut().copy_from_slice(m.f32_mut());
        mel_len.i32_mut()[0] = output(&out, "mel_length")?.i32_mut()[0];
        eprintln!("mel from {wav}: mel_length={}", mel_len.i32_mut()[0]);
    }
    let t0 = Instant::now();
    let enc = Model::load(&format!("{dir}/Encoder.mlmodelc"), units)?;
    let load_s = t0.elapsed().as_secs_f64();
    let input = provider(&[("mel", &mel), ("mel_length", &mel_len)])?;
    let t1 = Instant::now();
    let first = enc.predict(&input)?;
    let first_s = t1.elapsed().as_secs_f64();
    let len = output(&first, "encoder_length")?.i32_mut()[0];
    let mut times = Vec::with_capacity(iters);
    for _ in 0..iters {
        let t = Instant::now();
        let _ = enc.predict(&input)?;
        times.push(t.elapsed().as_secs_f64() * 1000.0);
    }
    times.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = times.iter().sum::<f64>() / times.len() as f64;
    println!(
        "rust encoder units={} load={:.3}s first_call={:.1}ms steady(n={}) min={:.1}ms median={:.1}ms mean={:.1}ms max={:.1}ms encoder_length={}",
        args.get(1).map(String::as_str).unwrap_or("all"),
        load_s,
        first_s * 1000.0,
        iters,
        times[0],
        times[times.len() / 2],
        mean,
        times[times.len() - 1],
        len
    );
    Ok(())
}

fn json_escape(s: &str) -> String {
    serde_json::to_string(s).unwrap()
}

fn transcribe(args: &[String]) -> Result<(), String> {
    let dir = args.first().ok_or("models dir")?;
    let out_dir = args.get(1).ok_or("out dir")?;
    let mut units = "all";
    let mut wavs = Vec::new();
    let mut i = 2;
    while i < args.len() {
        if args[i] == "--encoder-units" {
            units = args.get(i + 1).map(String::as_str).ok_or("--encoder-units value")?;
            i += 2;
        } else {
            wavs.push(args[i].clone());
            i += 1;
        }
    }
    std::fs::create_dir_all(out_dir).map_err(|e| e.to_string())?;
    let (models, load_s) = pipeline::Models::load(dir, parse_units(units))?;
    let vocab = pipeline::Vocab::load(&format!("{dir}/parakeet_v3_vocab.json"))?;
    eprintln!("models loaded in {load_s:.2}s (encoder units {units})");
    println!("| File | Audio s | Wall s | RTFx | Chunks | Tokens | Pre s | Enc s | Dec s | Dec calls | Joint calls | Recoveries |");
    println!("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|---:|");
    for wav in &wavs {
        let audio = wav::read_wav_mono_i16(wav)?;
        let audio_s = audio.len() as f64 / pipeline::SAMPLE_RATE as f64;
        let t0 = Instant::now();
        let tr = pipeline::transcribe(&models, &vocab, &audio)?;
        let wall = t0.elapsed().as_secs_f64();
        let (text, words) = pipeline::render(&tr.tokens, &vocab);
        let name = std::path::Path::new(wav).file_stem().unwrap().to_string_lossy().to_string();
        let mut json = String::from("{\n");
        json.push_str(&format!("  \"file\": {},\n  \"audio_seconds\": {audio_s:.2},\n  \"wall_seconds\": {wall:.3},\n  \"text\": {},\n  \"words\": [\n", json_escape(wav), json_escape(&text)));
        for (k, w) in words.iter().enumerate() {
            json.push_str(&format!("    {{\"word\": {}, \"start\": {:.2}, \"end\": {:.2}}}{}\n", json_escape(&w.word), w.start, w.end, if k + 1 < words.len() { "," } else { "" }));
        }
        json.push_str("  ]\n}\n");
        std::fs::write(format!("{out_dir}/{name}.rust.json"), json).map_err(|e| e.to_string())?;
        println!(
            "| {name}.wav | {audio_s:.2} | {wall:.2} | {:.1} | {} | {} | {:.2} | {:.2} | {:.2} | {} | {} | {}/{} |",
            audio_s / wall,
            tr.chunks,
            tr.tokens.len(),
            tr.timing.pre_s,
            tr.timing.enc_s,
            tr.timing.dec_s,
            tr.stats.decoder_calls,
            tr.stats.joint_calls,
            tr.stats.recoveries_accepted,
            tr.stats.recoveries_tried
        );
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let r = match args.first().map(String::as_str) {
        Some("bench-encoder") => bench_encoder(&args[1..]),
        Some("transcribe") => transcribe(&args[1..]),
        _ => usage(),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
