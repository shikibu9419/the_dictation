//! Explicit headless tests with the pinned model and generated Japanese fixtures.
use index_qwen::{
    backend::mlx::stream::init_mlx,
    inference::{AsrInference, WindowCache},
    tensor::Device,
};
use std::{cell::Cell, path::PathBuf, time::Instant};
fn wav(name: &str) -> Vec<f32> {
    let directory =
        PathBuf::from(std::env::var_os("INDEX_QWEN_FIXTURES").expect("Set INDEX_QWEN_FIXTURES"));
    let mut reader = hound::WavReader::open(directory.join(name)).unwrap();
    assert_eq!(reader.spec().sample_rate, 16000);
    assert_eq!(reader.spec().channels, 1);
    reader
        .samples::<i16>()
        .map(|v| v.unwrap() as f32 / 32768.0)
        .collect()
}
#[test]
#[ignore = "Requires pinned Qwen assets and generated Japanese WAV fixtures; no UI or playback"]
fn repeated_audio_long_coverage_prefix_reuse_and_cancellation() {
    init_mlx(true);
    let root = PathBuf::from(std::env::var_os("INDEX_QWEN_MODEL").expect("Set INDEX_QWEN_MODEL"));
    let model = AsrInference::load(&root, Device::gpu()).unwrap();
    let tokenizer = index_qwen::tokenizer::AsrTokenizer::from_dir(&root).unwrap();
    let ids = tokenizer
        .encode("こんにちは。これは音声認識の動作確認です。")
        .unwrap();
    assert_eq!(
        ids,
        vec![89015, 1773, 129562, 78685, 70074, 110790, 15767, 117748, 114277, 37541, 1773]
    );
    assert_eq!(tokenizer.decode(&[151704]).unwrap(), "<asr_text>");
    let short = wav("short.wav");
    let long = wav("long.wav");
    let mut short_ids = Vec::new();
    for (name, audio) in [("short", &short), ("long", &long), ("short_again", &short)] {
        let started = Instant::now();
        let out = model
            .transcribe_samples(audio, Some("Japanese"), &[], || false)
            .unwrap();
        eprintln!(
            "{}",
            serde_json::json!({"case":name,"seconds":started.elapsed().as_secs_f64(),"text":out.text,"timings":out.timings})
        );
        if name == "long" {
            assert!(out.text.starts_with("最初の確認です。"));
            assert!(out.text.contains("短い休憩を挟んだ場合でも"));
            assert!(out.text.ends_with("これで最後の確認を終わります。"));
        } else {
            assert_eq!(out.text, "こんにちは。これは音声認識の動作確認です。");
            if name == "short" {
                short_ids = out.token_ids;
            } else {
                assert_eq!(out.token_ids, short_ids);
            }
        }
    }
    let repeated: Vec<f32> = short.iter().copied().cycle().take(16000 * 10).collect();
    let mut cache = WindowCache::default();
    model
        .transcribe_cached(
            &repeated[..129600],
            Some("Japanese"),
            &[],
            &mut cache,
            || false,
        )
        .unwrap();
    let began = Instant::now();
    let cached = model
        .transcribe_cached(
            &repeated[..145600],
            Some("Japanese"),
            &[],
            &mut cache,
            || false,
        )
        .unwrap();
    let cached_seconds = began.elapsed().as_secs_f64();
    let began = Instant::now();
    let fresh = model
        .transcribe_samples(&repeated[..145600], Some("Japanese"), &[], || false)
        .unwrap();
    eprintln!(
        "{}",
        serde_json::json!({"case":"prefix_reuse","cached_seconds":cached_seconds,"fresh_seconds":began.elapsed().as_secs_f64(),"timings":cached.timings})
    );
    assert_eq!(cached.token_ids, fresh.token_ids);
    assert_eq!(cached.timings.reused_frames, 800);
    assert!(cached.timings.reused_positions > 0);
    cache.reset();
    let clean = model
        .transcribe_cached(&short, Some("Japanese"), &[], &mut cache, || false)
        .unwrap();
    assert_eq!(clean.token_ids, short_ids);
    assert_eq!(clean.timings.reused_positions, 0);
    assert!(model
        .transcribe_samples(&short, Some("Japanese"), &[], || true)
        .unwrap_err()
        .to_string()
        .contains("cancelled"));
    let calls = Cell::new(0);
    let result = model.transcribe_samples(&short, Some("Japanese"), &[], || {
        calls.set(calls.get() + 1);
        calls.get() >= 5
    });
    assert!(result.unwrap_err().to_string().contains("cancelled"));
    assert_eq!(
        model
            .transcribe_samples(&short, Some("Japanese"), &[], || false)
            .unwrap()
            .token_ids,
        short_ids
    );
}
