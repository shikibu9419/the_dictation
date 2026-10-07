use anyhow::{Context, Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    io::BufRead,
    path::Path,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use whisper_rs::{FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters};

// Keep the model's whitespace between independently decoded windows. Only trim
// the complete displayed result, never individual windows or the live prefix.
fn transcript_text(prefix: &str, chunks: &[String]) -> String {
    format!("{prefix}{}", chunks.concat())
}

#[derive(Default)]
struct Audio {
    samples: Vec<i16>,
    rate: u32,
    finish: bool,
    closed: bool,
    version: u64,
    decoded: usize,
    prefix: String,
}
impl Audio {
    fn append(&mut self, rate: u32, samples: Vec<i16>) -> Result<()> {
        ensure!((1000..=192000).contains(&rate), "Invalid sample rate");
        ensure!(
            self.rate == 0 || self.rate == rate,
            "Sample rate changed within recording"
        );
        ensure!(!self.finish, "Audio sent after finish");
        self.rate = rate;
        self.samples.extend(samples);
        Ok(())
    }
    fn reset(&mut self) {
        let version = self.version + 1;
        *self = Self {
            version,
            ..Self::default()
        };
    }
}
fn emit(value: Value) {
    crate::recognition::emit(value);
}
fn resample(samples: &[i16], rate: u32) -> Vec<f32> {
    if samples.is_empty() {
        return vec![];
    }
    let len = (samples.len() as u64 * 16000 / rate as u64) as usize;
    (0..len)
        .map(|i| {
            let position = i as f64 * rate as f64 / 16000.;
            let index = position as usize;
            let fraction = (position - index as f64) as f32;
            let a = samples[index.min(samples.len() - 1)] as f32;
            let b = samples[(index + 1).min(samples.len() - 1)] as f32;
            (a + (b - a) * fraction) / 32768.
        })
        .collect()
}
fn batch_windows(pcm: &[f32]) -> Vec<std::ops::Range<usize>> {
    let mut windows = Vec::new();
    let mut start = 0;
    while start < pcm.len() {
        let maximum = (start + 25 * 16000).min(pcm.len());
        let mut end = maximum;
        if maximum < pcm.len() {
            let minimum = start + 18 * 16000;
            for candidate in (minimum..maximum.saturating_sub(319)).step_by(320).rev() {
                let energy = pcm[candidate..candidate + 320]
                    .iter()
                    .map(|n| n * n)
                    .sum::<f32>()
                    / 320.;
                if energy < 0.0001 {
                    end = candidate + 160;
                    break;
                }
            }
        }
        windows.push(start..end);
        start = end;
    }
    windows
}
pub fn worker(model: &Path, language: &str, mode: &str) -> Result<()> {
    ensure!(matches!(mode, "live" | "batch"), "Invalid Whisper mode");
    emit(json!({"type":"status","text":"Loading Whisper large-v3…"}));
    let context = WhisperContext::new_with_params(model, WhisperContextParameters::default())?;
    ensure!(
        context.model_n_mels() == 128
            && context.model_n_audio_layer() == 32
            && context.model_n_text_layer() == 32,
        "Expected Whisper large-v3 model (128 mel bins, 32 encoder layers)"
    );
    let mut decoder = context.create_state()?;
    let shared = Arc::new((Mutex::new(Audio::default()), Condvar::new()));
    let generation = Arc::new(AtomicU64::new(0));
    let reader_shared = shared.clone();
    let reader_generation = generation.clone();
    std::thread::spawn(move || {
        let result = (|| -> Result<()> {
            for line in std::io::stdin().lock().lines() {
                let message: Value = serde_json::from_str(&line?)?;
                let (lock, ready) = &*reader_shared;
                let mut audio = lock.lock().unwrap();
                match message["type"].as_str() {
                    Some("audio") => {
                        let bytes =
                            STANDARD.decode(message["pcm"].as_str().context("Missing PCM")?)?;
                        ensure!(bytes.len() % 2 == 0, "Invalid s16le PCM");
                        let rate = u32::try_from(
                            message["sample_rate"].as_u64().context("Missing rate")?,
                        )?;
                        audio.append(
                            rate,
                            bytes
                                .chunks_exact(2)
                                .map(|b| i16::from_le_bytes([b[0], b[1]]))
                                .collect(),
                        )?;
                        emit(json!({"type":"accepted"}));
                    }
                    Some("finish") => audio.finish = true,
                    Some("cancel") => {
                        audio.reset();
                        reader_generation.store(audio.version, Ordering::Release);
                        emit(json!({"type":"cancelled"}));
                    }
                    _ => anyhow::bail!("Unknown Whisper command"),
                }
                ready.notify_one();
            }
            Ok(())
        })();
        if let Err(e) = result {
            emit(json!({"type":"error","text":format!("{e:#}")}));
        }
        let (lock, ready) = &*reader_shared;
        lock.lock().unwrap().closed = true;
        reader_generation.fetch_add(1, Ordering::AcqRel);
        ready.notify_one();
    });
    emit(json!({"type":"ready"}));
    let language = language.split(['-', '_']).next().unwrap_or("ja");
    loop {
        let (samples, rate, final_result, version, prefix, consumed) = {
            let (lock, ready) = &*shared;
            let mut audio = lock.lock().unwrap();
            loop {
                if audio.closed {
                    return Ok(());
                }
                if audio.finish
                    || (mode == "live"
                        && audio.rate > 0
                        && audio.samples.len() >= audio.decoded + audio.rate as usize * 2)
                {
                    break;
                }
                audio = ready.wait(audio).unwrap();
            }
            let consumed = if mode == "live" && !audio.finish {
                audio.samples.len().min(audio.rate as usize * 12)
            } else {
                audio.samples.len()
            };
            (
                audio.samples[..consumed].to_vec(),
                audio.rate,
                audio.finish,
                audio.version,
                audio.prefix.clone(),
                consumed,
            )
        };
        let pcm = resample(&samples, rate.max(1));
        let windows = if final_result {
            batch_windows(&pcm)
        } else {
            std::iter::once(0..pcm.len()).collect()
        };
        let mut chunks = Vec::new();
        for window in windows {
            let mut block = pcm[window.clone()].to_vec();
            let silent =
                block.iter().map(|n| n * n).sum::<f32>() / (block.len().max(1) as f32) < 0.000001;
            if !silent {
                block.resize(block.len().max(16000), 0.);
                let mut params = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
                params.set_language(Some(language));
                params.set_no_context(true);
                params.set_print_special(false);
                params.set_print_progress(false);
                params.set_print_realtime(false);
                params.set_print_timestamps(false);
                params.set_n_threads(
                    std::thread::available_parallelism().map_or(4, |n| n.get().min(8)) as i32,
                );
                let cancellation = (generation.clone(), version);
                unsafe extern "C" fn aborted(data: *mut std::ffi::c_void) -> bool {
                    let (generation, version) = unsafe { &*(data as *const (Arc<AtomicU64>, u64)) };
                    generation.load(Ordering::Acquire) != *version
                }
                // The callback data remains alive until the synchronous full() call returns.
                unsafe {
                    params.set_abort_callback(Some(aborted));
                    params.set_abort_callback_user_data(
                        &cancellation as *const _ as *mut std::ffi::c_void,
                    );
                }
                let result = decoder.full(params, &block);
                if generation.load(Ordering::Acquire) != version {
                    break;
                }
                result?;
                chunks.push(
                    decoder
                        .as_iter()
                        .map(|segment| segment.to_string())
                        .collect::<String>(),
                );
            }
            if final_result {
                emit(
                    json!({"type":"status","text":"Whisper audio segment recognized","segment_start":window.start as f64/16000.,"segment_end":window.end as f64/16000.}),
                );
            }
        }
        let (lock, _) = &*shared;
        let mut audio = lock.lock().unwrap();
        if audio.version != version {
            continue;
        }
        let result = transcript_text(&prefix, &chunks);
        if final_result {
            emit(json!({"type":"final","text":result.trim()}));
            audio.reset();
            generation.store(audio.version, Ordering::Release);
        } else {
            emit(json!({"type":"partial","text":result.trim()}));
            if consumed >= rate as usize * 12 {
                audio.samples.drain(..consumed);
                audio.prefix = result;
                audio.decoded = 0;
            } else {
                audio.decoded = consumed;
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn batch_and_live_boundaries_preserve_model_whitespace() {
        let chunks = vec![" It was quiet.".into(), " Today is sunny. ".into()];
        assert_eq!(
            transcript_text("", &chunks).trim(),
            "It was quiet. Today is sunny."
        );
        assert_eq!(
            transcript_text(" It was quiet.", &chunks[1..]).trim(),
            "It was quiet. Today is sunny."
        );
        assert_eq!(transcript_text("hello ", &["world".into()]), "hello world");
        assert_eq!(
            transcript_text("こんにちは。", &["今日は晴れです。".into()]),
            "こんにちは。今日は晴れです。"
        );
    }
    #[test]
    fn batch_windows_cover_all_samples_without_overlap_or_gaps() {
        for length in [0, 1, 16000, 400001, 16000 * 180 + 317] {
            let pcm = vec![0.1; length];
            let windows = batch_windows(&pcm);
            let mut cursor = 0;
            for window in windows {
                assert_eq!(window.start, cursor);
                assert!(window.end - window.start <= 25 * 16000);
                cursor = window.end;
            }
            assert_eq!(cursor, length);
        }
    }
    #[test]
    fn long_audio_cuts_at_silence_before_limit() {
        let mut pcm = vec![0.1; 16000 * 40];
        pcm[16000 * 22..16000 * 23].fill(0.);
        let windows = batch_windows(&pcm);
        assert!((16000 * 22..16000 * 23).contains(&windows[0].end));
        assert_eq!(windows.last().unwrap().end, pcm.len());
    }
    #[test]
    fn resampling_preserves_length_and_scale() {
        let pcm = resample(&vec![16384; 9997], 9997);
        assert_eq!(pcm.len(), 16000);
        assert!(pcm.iter().all(|n| (*n - 0.5).abs() < 1e-6));
        assert!(resample(&[], 16000).is_empty());
    }
    #[test]
    fn cancel_removes_previous_audio_and_prefix() {
        let mut audio = Audio::default();
        audio.append(16000, vec![1, 2, 3]).unwrap();
        audio.prefix = "old text".into();
        audio.finish = true;
        audio.reset();
        assert_eq!(audio.version, 1);
        assert!(audio.samples.is_empty());
        assert!(audio.prefix.is_empty());
        assert!(!audio.finish);
        audio.append(9997, vec![4]).unwrap();
    }
    #[test]
    fn rejects_rate_changes_and_audio_after_final() {
        let mut audio = Audio::default();
        audio.append(16000, vec![1]).unwrap();
        assert!(audio.append(9997, vec![2]).is_err());
        audio.finish = true;
        assert!(audio.append(16000, vec![3]).is_err());
    }
}
