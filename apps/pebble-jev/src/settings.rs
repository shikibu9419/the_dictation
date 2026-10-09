use anyhow::{Context, Result, bail};
use pebble_core::config;
use pebble_ring::reception::config::Reception;
use serde::{Deserialize, Serialize};
use std::{
    io::Write,
    path::{Path, PathBuf},
};

pub const APP: &str = "pebble-jev";
const DEFAULT_INSTRUCTIONS: &str = "あなたは簡潔な日本語の音声アシスタントです。短く答えてください。\
TODOの追加を頼まれたら add_todo を、覚えておいてほしい内容は add_memo を呼び、結果を一言で伝えてください。";

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub struct Settings {
    /// Falls back to `OPENAI_API_KEY`.
    pub api_key: Option<String>,
    pub model: String,
    pub voice: String,
    /// BCP-47 language hint for user transcription.
    pub language: String,
    pub instructions: String,
    pub reception: Reception,
}
impl Default for Settings {
    fn default() -> Self {
        Self {
            api_key: None,
            model: openai_realtime::DEFAULT_MODEL.into(),
            voice: "marin".into(),
            language: "ja".into(),
            instructions: DEFAULT_INSTRUCTIONS.into(),
            reception: Reception::default(),
        }
    }
}
impl Settings {
    pub fn directory() -> PathBuf {
        config::app_directory(APP)
    }
    pub fn path() -> PathBuf {
        Self::directory().join("settings.json")
    }
    pub fn load() -> Result<Self> {
        Self::load_from(&Self::path())
    }
    pub fn load_from(path: &Path) -> Result<Self> {
        match std::fs::read(path) {
            Ok(bytes) => {
                let value: Self =
                    serde_json::from_slice(&bytes).context("Invalid pebble-jev settings")?;
                value.reception.validate()?;
                Ok(value)
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(e) => Err(e.into()),
        }
    }
    /// Write the file so it exists for editing; keeps existing content.
    pub fn ensure_saved() -> Result<PathBuf> {
        let path = Self::path();
        if !path.exists() {
            Self::default().save_to(&path)?;
        }
        Ok(path)
    }
    pub fn save_to(&self, path: &Path) -> Result<()> {
        let parent = path.parent().context("Settings directory missing")?;
        std::fs::create_dir_all(parent)?;
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.write_all(b"\n")?;
        file.as_file().sync_all()?;
        file.persist(path)?;
        Ok(())
    }
    pub fn api_key(&self) -> Result<String> {
        if let Some(key) = self.api_key.as_deref().filter(|k| !k.trim().is_empty()) {
            return Ok(key.trim().to_owned());
        }
        match openai_realtime::api_key_from_env() {
            Ok(key) => Ok(key),
            Err(_) => bail!(
                "OpenAI APIキーがありません。{} の api_key か環境変数 OPENAI_API_KEY を設定してください",
                Self::path().display()
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_roundtrip_and_reject_unknown_keys() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.json");
        assert_eq!(Settings::load_from(&path).unwrap(), Settings::default());
        let value = Settings {
            voice: "cedar".into(),
            ..Settings::default()
        };
        value.save_to(&path).unwrap();
        assert_eq!(Settings::load_from(&path).unwrap(), value);
        assert!(serde_json::from_str::<Settings>(r#"{"voices":"x"}"#).is_err());
    }

    #[test]
    fn api_key_prefers_the_settings_file() {
        let settings = Settings {
            api_key: Some(" sk-test ".into()),
            ..Settings::default()
        };
        assert_eq!(settings.api_key().unwrap(), "sk-test");
    }
}
