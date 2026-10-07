use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum InputSource {
    #[default]
    Index,
    Microphone,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum SpeechModel {
    #[default]
    Apple,
    WhisperLargeV3,
    #[serde(alias = "parakeet_mlx", alias = "nemotron_mlx")]
    OnDevice,
}
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    pub input: InputSource,
    pub speech: SpeechModel,
    pub whisper_model: Option<PathBuf>,
}
impl Settings {
    pub fn qwen_dir() -> PathBuf {
        PathBuf::from(std::env::var_os("HOME").unwrap())
            .join("Library/Application Support/Index Voice/qwen-mlx")
    }
    pub fn qwen_ready() -> bool {
        let root = Self::qwen_dir();
        root.join(".venv/bin/python").is_file()
            && root.join("model/config.json").is_file()
            && root.join("model/weights.safetensors").is_file()
            && root.join("model/vocab.json").is_file()
            && root.join("model/merges.txt").is_file()
            && root.join("model/tokenizer_config.json").is_file()
            && std::fs::read_to_string(root.join("ready"))
                .is_ok_and(|revision| revision == "22c8abe6a6772122dda5905967d7496d1d3e8dd2")
    }
    pub fn path() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config"))
            .join("pebble-index-rust/settings.json")
    }
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).context("Invalid Index Voice settings"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    pub fn model_path(&self) -> PathBuf {
        self.whisper_model.clone().unwrap_or_else(|| {
            std::env::var_os("XDG_CACHE_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| {
                    PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library/Caches")
                })
                .join("pebble-index-rust/ggml-large-v3.bin")
        })
    }
    pub fn validate(&self) -> Result<()> {
        if self.speech == SpeechModel::OnDevice {
            ensure!(
                Self::qwen_ready(),
                "Qwen3-ASR MLX is not installed; run pebble-index setup-qwen"
            );
        }
        if self.speech == SpeechModel::WhisperLargeV3 {
            ensure!(
                self.model_path().is_file(),
                "Whisper large-v3 model not downloaded: {}",
                self.model_path().display()
            );
        }
        Ok(())
    }
    pub fn save(&self) -> Result<()> {
        self.validate()?;
        self.save_to(&Self::path())
    }
    fn save_to(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Settings directory missing")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn defaults_preserve_index_and_apple() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            Settings::load_from(&dir.path().join("settings.json")).unwrap(),
            Settings::default()
        );
    }
    #[test]
    fn independent_selection_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        for input in [InputSource::Index, InputSource::Microphone] {
            for speech in [SpeechModel::Apple, SpeechModel::WhisperLargeV3] {
                let value = Settings {
                    input,
                    speech,
                    whisper_model: Some(dir.path().join("model.bin")),
                };
                value.save_to(&path).unwrap();
                assert_eq!(Settings::load_from(&path).unwrap(), value);
            }
        }
    }
    #[test]
    fn corrupt_settings_are_not_silently_replaced() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        std::fs::write(&path, r#"{"speech":"unknown"}"#).unwrap();
        assert!(Settings::load_from(&path).is_err());
        assert!(serde_json::from_str::<Settings>(r#"{"spech":"apple"}"#).is_err());
    }
    #[test]
    fn whisper_requires_existing_model() {
        let dir = tempfile::tempdir().unwrap();
        let value = Settings {
            speech: SpeechModel::WhisperLargeV3,
            whisper_model: Some(dir.path().join("missing.bin")),
            ..Settings::default()
        };
        assert!(value.validate().is_err());
    }
}
