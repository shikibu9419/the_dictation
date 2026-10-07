use super::{EngineCommand, EngineEvent, Reply, SpeechEngine};
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
}
impl ProcessEngine {
    pub async fn start(language: &str, mode: &str, output: Output) -> Result<Self> {
        let custom = std::env::var_os("INDEX_VOICE_SPEECH_COMMAND");
        let settings = crate::settings::Settings::load()?;
        let (mut command, name) = if let Some(path) = custom {
            (Command::new(path), "external speech engine")
        } else if settings.speech == crate::settings::SpeechModel::OnDevice {
            settings.validate()?;
            let root = crate::settings::Settings::qwen_dir();
            let mut command = Command::new(root.join(".venv/bin/python"));
            command
                .arg("-u")
                .arg("-c")
                .arg(include_str!("../../../native/qwen/adapter.py"))
                .arg(root)
                .env("HF_HUB_OFFLINE", "1");
            (command, "Qwen3-ASR MLX")
        } else if settings.speech == crate::settings::SpeechModel::WhisperLargeV3 {
            settings.validate()?;
            let mut command = Command::new(std::env::current_exe()?);
            command.arg("__whisper").arg(settings.model_path());
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
        })
    }
}
impl SpeechEngine for ProcessEngine {
    fn name(&self) -> &str {
        &self.name
    }
    fn send(&mut self, command: EngineCommand) -> Reply<'_, ()> {
        Box::pin(async move {
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
            self.helper.send(&value).await
        })
    }
    fn event(&mut self) -> Reply<'_, EngineEvent> {
        Box::pin(async move { Ok(serde_json::from_value(self.helper.event().await?)?) })
    }
    fn close(&mut self) -> Reply<'_, ()> {
        Box::pin(async move {
            self.helper.close().await;
            Ok(())
        })
    }
}
