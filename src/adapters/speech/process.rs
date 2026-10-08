use super::{
    EngineCommand, EngineConfig, EngineReply, Reply, SpeechEngine,
    native_session::NativeSession,
    run_control::{ExecutionControl, NativeControl},
};
use crate::{
    filter::Filter,
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use std::sync::Arc;
use tokio::process::Command;

pub struct ProcessEngine {
    helper: Helper,
    name: String,
    filter: Option<Filter>,
    native: Option<NativeSession>,
    control: Option<Arc<dyn ExecutionControl>>,
}
impl ProcessEngine {
    pub async fn start(
        config: &EngineConfig,
        language: &str,
        mode: &str,
        output: Output,
    ) -> Result<Self> {
        let custom = std::env::var_os("INDEX_VOICE_SPEECH_COMMAND");
        let native = (custom.is_none() && matches!(config, EngineConfig::Qwen { .. }))
            .then(NativeSession::default);
        let (mut command, name) = if let Some(path) = custom {
            (Command::new(path), "external speech engine")
        } else if let EngineConfig::Qwen { root } = config {
            crate::qwen_runtime::model_ready(root)?;
            let mut command = Command::new(crate::qwen_runtime::executable()?);
            command.arg(root.join("model"));
            (command, "Qwen3-ASR MLX")
        } else if let EngineConfig::Whisper { model } = config {
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("__whisper").arg(model);
            (command, "Whisper large-v3")
        } else {
            (
                Command::new(
                    executable(
                        "SpeechStream",
                        include_str!("../../../native/SpeechStream.swift"),
                        &output,
                    )
                    .await?,
                ),
                "Apple SpeechAnalyzer",
            )
        };
        command.arg(language).arg(mode);
        let (activity, observer) = if native.is_some() {
            let (activity, observer) = NativeControl::observe();
            (Some(activity), Some(observer))
        } else {
            (None, None)
        };
        let helper = Helper::spawn_observed(command, output.clone(), mode.into(), observer).await?;
        let control = activity.map(|activity| {
            Arc::new(NativeControl::new(
                helper.input(),
                activity,
                output,
                mode.into(),
            )) as Arc<dyn ExecutionControl>
        });
        Ok(Self {
            helper,
            name: name.into(),
            filter: None,
            native,
            control,
        })
    }
}
impl SpeechEngine for ProcessEngine {
    fn input_backlogged(&self) -> bool {
        self.native
            .as_ref()
            .zip(self.filter.as_ref())
            .is_some_and(|(session, filter)| session.backlogged(filter.rate))
    }
    fn control(&self) -> Option<Arc<dyn ExecutionControl>> {
        self.control.clone()
    }
    fn name(&self) -> &str {
        &self.name
    }
    fn send(&mut self, command: EngineCommand) -> Reply<'_, ()> {
        Box::pin(async move {
            let samples = match &command {
                EngineCommand::Audio { samples, .. } => samples.len(),
                _ => 0,
            };
            let value = match command {
                EngineCommand::Audio { samples, rate } => {
                    let filter = self.filter.get_or_insert_with(|| Filter::new(rate));
                    ensure!(filter.rate == rate, "Sample rate changed within recording");
                    let pcm: Vec<u8> = filter
                        .process(&samples)
                        .iter()
                        .flat_map(|n| n.to_le_bytes())
                        .collect();
                    json!({"type":"audio","sample_rate":rate,"pcm":STANDARD.encode(pcm)})
                }
                EngineCommand::Finish => {
                    self.filter = None;
                    json!({"type":"finish"})
                }
                EngineCommand::Cancel => {
                    self.filter = None;
                    json!({"type":"cancel"})
                }
            };
            let value = if let Some(native) = &mut self.native {
                native.command(value, samples)
            } else {
                value
            };
            self.helper.send(&value).await
        })
    }
    fn event(&mut self) -> Reply<'_, EngineReply> {
        Box::pin(async move {
            loop {
                let value = self.helper.event().await?;
                if let Some(native) = &mut self.native
                    && !native.accept(&value)?
                {
                    continue;
                }
                if self.native.is_some() && value["type"] == "ready" {
                    ensure!(
                        value["capabilities"]
                            .as_array()
                            .is_some_and(|caps| caps.iter().any(|c| c == "permit_ack")),
                        "QwenNative is outdated; rebuild the native worker for synchronized execution control"
                    );
                }
                return Ok(serde_json::from_value(value)?);
            }
        })
    }
    fn close(&mut self) -> Reply<'_, ()> {
        Box::pin(async move {
            self.helper.close().await;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{path::Path, time::Duration};
    fn wav(path: &Path) -> Vec<i16> {
        let data = std::fs::read(path).unwrap();
        assert_eq!(&data[..4], b"RIFF");
        let mut at = 12;
        while at + 8 <= data.len() {
            let size = u32::from_le_bytes(data[at + 4..at + 8].try_into().unwrap()) as usize;
            if &data[at..at + 4] == b"data" {
                return data[at + 8..at + 8 + size]
                    .chunks_exact(2)
                    .map(|v| i16::from_le_bytes([v[0], v[1]]))
                    .collect();
            }
            at += 8 + size + size % 2;
        }
        panic!("Missing PCM fixture");
    }
    async fn until(engine: &mut ProcessEngine, kind: &str) -> EngineReply {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let reply = engine.event().await.unwrap();
                assert_ne!(reply.kind(), "error", "{reply:?}");
                if reply.kind() == kind {
                    return reply;
                }
            }
        })
        .await
        .unwrap()
    }
    #[tokio::test]
    #[ignore = "Native model, isolated manifest, no UI/BLE/microphone; needs INDEX_QWEN_MODEL and INDEX_QWEN_FIXTURES"]
    async fn native_adapter_reuses_verified_model_and_resets_between_recordings() {
        let model = std::env::var_os("INDEX_QWEN_MODEL").unwrap();
        let fixtures = std::env::var_os("INDEX_QWEN_FIXTURES").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(model, root.path().join("model")).unwrap();
        let output = Output::new(false, None).unwrap();
        // No downloads are needed: reuse and hash the existing pinned assets.
        crate::qwen_setup::setup_model(root.path().into(), output.clone())
            .await
            .unwrap();
        crate::qwen_setup::setup_model(root.path().into(), output.clone())
            .await
            .unwrap();
        assert!(crate::qwen_runtime::ready(root.path()));
        assert!(!root.path().join(".venv").exists());
        let config = EngineConfig::Qwen {
            root: root.path().into(),
        };
        let mut engine = ProcessEngine::start(&config, "ja_JP", "batch", output)
            .await
            .unwrap();
        until(&mut engine, "ready").await;
        let short = wav(&Path::new(&fixtures).join("short.wav"));
        for _ in 0..2 {
            for block in short.chunks(3200) {
                engine
                    .send(EngineCommand::Audio {
                        samples: block.to_vec(),
                        rate: 16000,
                    })
                    .await
                    .unwrap();
                until(&mut engine, "accepted").await;
            }
            engine.send(EngineCommand::Finish).await.unwrap();
            let result = serde_json::to_value(until(&mut engine, "final").await).unwrap();
            assert!(
                result["text"]
                    .as_str()
                    .unwrap()
                    .contains("これは音声認識の動作確認です"),
                "{result}"
            );
            assert_eq!(result["consumed_samples"], short.len());
        }
        // A cancel discards queued PCM and advances the parent generation.
        engine
            .send(EngineCommand::Audio {
                samples: short,
                rate: 16000,
            })
            .await
            .unwrap();
        until(&mut engine, "accepted").await;
        engine.send(EngineCommand::Cancel).await.unwrap();
        until(&mut engine, "cancelled").await;
        engine.send(EngineCommand::Finish).await.unwrap();
        let result = serde_json::to_value(until(&mut engine, "final").await).unwrap();
        assert_eq!(result["text"], "");
        assert_eq!(result["consumed_samples"], 0);
        tokio::time::timeout(Duration::from_secs(2), engine.close())
            .await
            .unwrap()
            .unwrap();
    }
    #[tokio::test]
    #[ignore = "Two native GPU workers, paced synthesized PCM, no UI; needs INDEX_QWEN_MODEL and INDEX_QWEN_FIXTURES"]
    async fn live_preempts_computing_batch_and_resumes_the_same_recording() {
        use super::super::run_control::LivePriority;
        use serde_json::Value;
        use std::time::Instant;
        async fn capture(
            engine: &mut ProcessEngine,
            kind: &str,
            events: &mut Vec<(Instant, Value)>,
        ) -> Value {
            tokio::time::timeout(Duration::from_secs(30), async {
                loop {
                    let reply = engine.event().await.unwrap();
                    assert_ne!(reply.kind(), "error", "{reply:?}");
                    let matched = reply.kind() == kind;
                    let value = serde_json::to_value(reply).unwrap();
                    events.push((Instant::now(), value.clone()));
                    if matched {
                        return value;
                    }
                }
            })
            .await
            .unwrap()
        }
        async fn run_live(live: &mut ProcessEngine, short: &[i16]) -> Value {
            let started = Instant::now();
            let mut events = vec![];
            let mut sent = 0;
            for chunk in short.chunks(3200) {
                sent += chunk.len();
                tokio::time::sleep_until(
                    (started + Duration::from_secs_f64(sent as f64 / 16000.0)).into(),
                )
                .await;
                live.send(EngineCommand::Audio {
                    samples: chunk.to_vec(),
                    rate: 16000,
                })
                .await
                .unwrap();
                capture(live, "accepted", &mut events).await;
            }
            let first = events
                .iter()
                .find(|(_, v)| v["type"] == "partial")
                .expect("Live result required before finish")
                .0
                .duration_since(started)
                .as_secs_f64();
            assert!(first < 5.0, "Live first partial took {first}s");
            let max_lag = events
                .iter()
                .filter_map(|(_, v)| {
                    Some(
                        (v["accepted_samples"].as_u64()? - v["consumed_samples"].as_u64()?) as f64
                            / 16000.0,
                    )
                })
                .fold(0.0, f64::max);
            live.send(EngineCommand::Finish).await.unwrap();
            let finish_started = Instant::now();
            let final_result = capture(live, "final", &mut events).await;
            let finish_seconds = finish_started.elapsed().as_secs_f64();
            assert_eq!(final_result["consumed_samples"], short.len());
            assert!(
                final_result["text"]
                    .as_str()
                    .unwrap()
                    .contains("これは音声認識の動作確認です"),
                "{final_result}"
            );
            let peak_memory = events
                .iter()
                .filter_map(|(_, v)| v["peak_memory_bytes"].as_u64())
                .max()
                .unwrap_or(0);
            json!({"first_partial_seconds":first,"max_pcm_lag_seconds":max_lag,"finish_seconds":finish_seconds,"peak_memory_bytes":peak_memory,"text":final_result["text"]})
        }
        let model = std::env::var_os("INDEX_QWEN_MODEL").unwrap();
        let fixtures = std::env::var_os("INDEX_QWEN_FIXTURES").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(model, root.path().join("model")).unwrap();
        let output = Output::new(false, None).unwrap();
        crate::qwen_setup::setup_model(root.path().into(), output.clone())
            .await
            .unwrap();
        let config = EngineConfig::Qwen {
            root: root.path().into(),
        };
        let mut live = ProcessEngine::start(&config, "ja_JP", "live", output.clone())
            .await
            .unwrap();
        until(&mut live, "ready").await;
        let mut batch = ProcessEngine::start(&config, "ja_JP", "batch", output)
            .await
            .unwrap();
        until(&mut batch, "ready").await;
        let mut priority = LivePriority::new(live.control(), batch.control().unwrap())
            .await
            .unwrap();
        let long = wav(&Path::new(&fixtures).join("long.wav"));
        let short = wav(&Path::new(&fixtures).join("short.wav"));
        priority.acquire().await.unwrap();
        let baseline = run_live(&mut live, &short).await;
        priority.release().await.unwrap();
        let mut batch_events = vec![];
        for chunk in long.chunks(3200) {
            batch
                .send(EngineCommand::Audio {
                    samples: chunk.to_vec(),
                    rate: 16000,
                })
                .await
                .unwrap();
            capture(&mut batch, "accepted", &mut batch_events).await;
        }
        batch.send(EngineCommand::Finish).await.unwrap();
        while !batch_events
            .iter()
            .any(|(_, v)| v["text"] == "Qwen batch inference started")
        {
            capture(&mut batch, "status", &mut batch_events).await;
        }
        // Start from an actual model job, not from an empty idle worker.
        tokio::time::sleep(Duration::from_millis(80)).await;
        let mut metrics = vec![];
        for recording in 1..=2 {
            let handoff = Instant::now();
            priority.acquire().await.unwrap();
            let pause_seconds = handoff.elapsed().as_secs_f64();
            assert!(!batch.control().unwrap().activity().borrow().permitted);
            // Drain responses already in the pipe before measuring the pause.
            while let Ok(reply) =
                tokio::time::timeout(Duration::from_millis(10), batch.event()).await
            {
                batch_events.push((
                    Instant::now(),
                    serde_json::to_value(reply.unwrap()).unwrap(),
                ));
            }
            assert!(
                !batch_events.iter().any(|(_, v)| v["type"] == "final"),
                "Batch had already finished before preemption"
            );
            let mut live_metrics = run_live(&mut live, &short).await;
            // The paused batch must not publish more computation while live owns the slot.
            assert!(
                tokio::time::timeout(Duration::from_millis(20), batch.event())
                    .await
                    .is_err()
            );
            priority.release().await.unwrap();
            live_metrics["recording"] = json!(recording);
            live_metrics["pause_ack_seconds"] = json!(pause_seconds);
            metrics.push(live_metrics);
            tokio::time::sleep(Duration::from_millis(80)).await;
        }
        let result = capture(&mut batch, "final", &mut batch_events).await;
        assert_eq!(result["consumed_samples"], long.len());
        let text = result["text"].as_str().unwrap();
        assert!(
            text.starts_with("最初の確認です。")
                && text.ends_with("これで最後の確認を終わります。"),
            "{text}"
        );
        let spans: Vec<_> = batch_events
            .iter()
            .filter_map(|(_, v)| Some((v["segment_start"].as_f64()?, v["segment_end"].as_f64()?)))
            .collect();
        assert_eq!(spans.first().unwrap().0, 0.0);
        assert_eq!(spans.last().unwrap().1, long.len() as f64 / 16000.0);
        assert!(spans.windows(2).all(|p| p[0].1 == p[1].0));
        eprintln!(
            "{}",
            json!({"live_without_batch":baseline,"live_while_batch_paused":metrics,"batch_text":text,"batch_segments":spans})
        );
        live.close().await.unwrap();
        batch.close().await.unwrap();
    }
}
