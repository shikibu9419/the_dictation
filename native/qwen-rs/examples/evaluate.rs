//! Headless fixed-model evaluation; emits JSON and never plays audio.
use anyhow::{Context, Result};
use index_qwen::{
    backend::mlx::stream::init_mlx, inference::AsrInference, mel::WhisperFeatureExtractor,
    tensor::Device,
};
use std::{path::Path, time::Instant};
fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let model_dir = args.get(1).context("usage: evaluate MODEL_DIR WAV...")?;
    init_mlx(true);
    let began = Instant::now();
    let model = AsrInference::load(Path::new(model_dir), Device::gpu())?;
    eprintln!("Model constructed in {:.3}s", began.elapsed().as_secs_f64());
    for path in &args[2..] {
        let mut wav = hound::WavReader::open(path)?;
        anyhow::ensure!(
            wav.spec().sample_rate == 16000
                && wav.spec().channels == 1
                && wav.spec().bits_per_sample == 16,
            "Need mono 16kHz s16 WAV"
        );
        let pcm: Vec<f32> = wav
            .samples::<i16>()
            .map(|v| v.map(|v| v as f32 / 32768.0))
            .collect::<Result<_, _>>()?;
        if let Ok(dest) = std::env::var("INDEX_QWEN_DUMP_MEL") {
            let mel = WhisperFeatureExtractor::new(400, 160, 128, 16000, Device::gpu())
                .extract(&pcm, Device::gpu())?;
            std::fs::write(
                dest,
                serde_json::to_vec(
                    &serde_json::json!({"shape": mel.size(), "values": mel.to_vec_f32()}),
                )?,
            )?;
        }
        let began = Instant::now();
        let result = model.transcribe_samples(&pcm, Some("Japanese"), &[], || false)?;
        println!(
            "{}",
            serde_json::json!({"file":path,"seconds":began.elapsed().as_secs_f64(),"audio_seconds":result.duration_seconds,"text":result.text,"tokens":result.token_ids,"timings":result.timings})
        );
    }
    Ok(())
}
