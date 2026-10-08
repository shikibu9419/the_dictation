use crate::{
    audio::{Resampler, SpeechStart, RATE},
    inference::{AsrInference, TranscribeResult, WindowCache},
    protocol::{Input, Sink, VERSION},
    streaming::{BatchSession, Decoder, LiveSession},
    tensor::Device,
};
use anyhow::{ensure, Context, Result};
use serde_json::json;
use std::{io::BufReader, path::Path, sync::Arc, time::Instant};

pub fn language(code: &str) -> Result<String> {
    let canonical = match code
        .to_ascii_lowercase()
        .replace('_', "-")
        .split('-')
        .next()
        .unwrap_or("")
    {
        "ja" | "japanese" => "Japanese",
        "en" | "english" => "English",
        "zh" | "chinese" => "Chinese",
        "ko" | "korean" => "Korean",
        "de" | "german" => "German",
        "fr" | "french" => "French",
        "es" | "spanish" => "Spanish",
        "pt" | "portuguese" => "Portuguese",
        "it" | "italian" => "Italian",
        "ru" | "russian" => "Russian",
        "ar" | "arabic" => "Arabic",
        "hi" | "hindi" => "Hindi",
        "th" | "thai" => "Thai",
        "vi" | "vietnamese" => "Vietnamese",
        "id" | "indonesian" => "Indonesian",
        "tr" | "turkish" => "Turkish",
        "nl" | "dutch" => "Dutch",
        "pl" | "polish" => "Polish",
        "sv" | "swedish" => "Swedish",
        "fi" | "finnish" => "Finnish",
        "da" | "danish" => "Danish",
        "el" | "greek" => "Greek",
        "cs" | "czech" => "Czech",
        "ro" | "romanian" => "Romanian",
        "hu" | "hungarian" => "Hungarian",
        "uk" | "ukrainian" => "Ukrainian",
        "ms" | "malay" => "Malay",
        "fa" | "persian" => "Persian",
        "fil" | "tagalog" => "Filipino",
        "yue" | "cantonese" => "Cantonese",
        _ => anyhow::bail!("Unsupported Qwen language: {code}"),
    };
    Ok(canonical.into())
}
struct Session {
    epoch: u64,
    resampler: Resampler,
    onset: SpeechStart,
    live: LiveSession,
    batch: BatchSession,
    resampled: u64,
    forwarded: u64,
    consumed_source: u64,
    last_text: String,
}
struct ObservedDecoder<'a> {
    model: &'a AsrInference,
    input: &'a Input,
    epoch: u64,
    mode: &'a str,
}
impl Decoder for ObservedDecoder<'_> {
    fn decode(
        &self,
        audio: &[f32],
        language: &str,
        prefix: &[i64],
        cache: &mut WindowCache,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<TranscribeResult> {
        self.input.publish(self.epoch, json!({"type":"status","text":format!("Qwen {} inference started", self.mode),"input_window_samples":audio.len(),"window_rate":RATE}))?;
        self.model
            .transcribe_cached(audio, Some(language), prefix, cache, cancelled)
    }
    fn rollback(&self, tokens: &[i64], count: usize) -> Result<Vec<i64>> {
        self.model.rollback_prefix(tokens, count)
    }
}
impl Session {
    fn new(epoch: u64, rate: u32, language: &str) -> Result<Self> {
        Ok(Self {
            epoch,
            resampler: Resampler::new(rate)?,
            onset: SpeechStart::default(),
            live: LiveSession::new(language.into()),
            batch: BatchSession::new(language.into()),
            resampled: 0,
            forwarded: 0,
            consumed_source: 0,
            last_text: String::new(),
        })
    }
}
pub fn run(model_path: &Path, locale: &str, mode: &str) -> Result<()> {
    ensure!(matches!(mode, "live" | "batch"), "Unknown Qwen mode {mode}");
    let language = language(locale)?;
    let input = Input::new(Sink::new(std::io::stdout()));
    let reader = input.clone();
    std::thread::spawn(move || reader.read(BufReader::new(std::io::stdin())));
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        serve(model_path, &language, mode, &input)
    }));
    let result = match result {
        Ok(result) => result,
        Err(panic) => {
            let text = panic
                .downcast_ref::<String>()
                .cloned()
                .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
                .unwrap_or_else(|| "MLX inference panic".into());
            Err(anyhow::anyhow!(text))
        }
    };
    if let Err(error) = &result {
        let _ = input
            .sink
            .send(&json!({"type":"error","protocol_version":VERSION,"text":format!("{error:#}")}));
    }
    result
}
fn serve(path: &Path, language: &str, mode: &str, input: &Arc<Input>) -> Result<()> {
    crate::backend::mlx::stream::init_mlx(true);
    input.sink.send(
        &json!({"type":"status","text":"Loading native Qwen3-ASR MLX","protocol_version":VERSION}),
    )?;
    let model = AsrInference::load(path, Device::gpu())?;
    if input.closed() {
        return input.shutdown_result();
    }
    input.sink.send(
        &json!({"type":"status","text":"Warming native Qwen3-ASR MLX","protocol_version":VERSION}),
    )?;
    if let Err(e) =
        model.transcribe_samples(&vec![0.0; RATE], Some(language), &[], || input.closed())
    {
        if !input.closed() {
            return Err(e.context("Qwen warmup failed"));
        }
    }
    if input.closed() {
        return input.shutdown_result();
    }
    input.idle()?;
    input.sink.send(&json!({"type":"ready","protocol_version":VERSION,"capabilities":["session_generation","consumed_samples","permit","permit_ack"]}))?;
    let mut session: Option<Session> = None;
    while let Some(work) = input.take()? {
        if work.reset_only {
            session = None;
            continue;
        }
        if session.as_ref().is_none_or(|s| s.epoch != work.epoch) {
            session = Some(Session::new(work.epoch, work.rate, language)?);
        }
        if input.checkpoint(work.epoch, crate::backend::mlx::stream::synchronize) {
            continue;
        }
        let state = session.as_mut().context("Missing recognition session")?;
        let audio = state.resampler.feed(&work.audio, work.final_input)?;
        state.resampled += audio.len() as u64;
        let started = Instant::now();
        let paused_before = input.paused_seconds();
        let decoder = ObservedDecoder {
            model: &model,
            input,
            epoch: work.epoch,
            mode,
        };
        let updates = if mode == "live" {
            let audio = state.onset.feed(&audio);
            state.forwarded += audio.len() as u64;
            state.live.feed(&decoder, &audio, work.final_input, &|| {
                input.checkpoint(work.epoch, crate::backend::mlx::stream::synchronize)
            })
        } else {
            state.batch.feed(&decoder, &audio, work.final_input, &|| {
                input.checkpoint(work.epoch, crate::backend::mlx::stream::synchronize)
            })
        };
        if !input.current(work.epoch) {
            continue;
        }
        let updates = updates?;
        let paused_seconds = input.paused_seconds() - paused_before;
        for update in updates {
            let offset = if mode == "live" {
                state.resampled - state.forwarded
            } else {
                0
            };
            let consumed = ((offset + update.consumed_samples) * work.rate as u64 / RATE as u64)
                .min(work.delivered)
                .max(state.consumed_source);
            state.consumed_source = consumed;
            let mut status = json!({"type":"status","text":format!("Qwen {mode} consumed PCM"),"consumed_samples":consumed,
                "window_samples":update.buffered_samples,"window_rate":RATE,"decode_seconds":(started.elapsed().as_secs_f64()-paused_seconds).max(0.0),"paused_seconds":paused_seconds});
            if let Some((start, end)) = update.segment {
                status["segment_start"] = json!(start as f64 / RATE as f64);
                status["segment_end"] = json!(end as f64 / RATE as f64);
            }
            input.publish(work.epoch, status)?;
            // Segment commitment is metadata, not a replacement for the latest
            // full live text. In particular, do not briefly erase the carried tail.
            if mode == "live" && update.segment.is_some() {
                continue;
            }
            if mode == "live" && !work.final_input && update.text != state.last_text {
                input.publish(
                    work.epoch,
                    json!({"type":"partial","text":update.text,"consumed_samples":consumed}),
                )?;
            }
            state.last_text = update.text;
        }
        if work.final_input {
            let mut peak = 0usize;
            crate::backend::mlx::error::check(
                unsafe { crate::backend::mlx::ffi::mlx_get_peak_memory(&mut peak) },
                "MLX memory measurement",
            );
            input.publish(
                work.epoch,
                json!({"type":"status","text":"Qwen recording complete","peak_memory_bytes":peak}),
            )?;
            input.finish(work.epoch, &state.last_text, work.delivered)?;
            session = None;
        }
    }
    Ok(())
}
