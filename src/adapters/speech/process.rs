use super::{
    EngineCommand, EngineConfig, EngineReply, Reply, SpeechEngine, native_session::NativeSession,
};
use crate::{
    filter::Filter,
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Result, ensure};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::json;
use tokio::process::Command;

pub struct ProcessEngine {
    helper: Helper,
    name: String,
    filter: Option<Filter>,
    native: Option<NativeSession>,
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
        Ok(Self {
            helper: Helper::spawn(command, output, mode.into()).await?,
            name: name.into(),
            filter: None,
            native,
        })
    }
}
impl SpeechEngine for ProcessEngine {
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
}
