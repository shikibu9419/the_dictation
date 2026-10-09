use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, Write},
    path::{Path, PathBuf},
};

fn config_root() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join(".config"))
}
/// Ring pairing, cursor and Bluetooth lock. Shared by every app so the ring
/// is paired once and never opened twice.
pub fn directory() -> PathBuf {
    config_root().join("pebble-index-rust")
}
/// Settings directory of one application.
pub fn app_directory(app: &str) -> PathBuf {
    config_root().join(app)
}
pub fn saved_address() -> Result<Option<String>> {
    let data = match fs::read(directory().join("device.json")) {
        Ok(data) => data,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    let value: serde_json::Value = serde_json::from_slice(&data)?;
    Ok(Some(
        value["address"]
            .as_str()
            .context("Invalid saved ring address")?
            .into(),
    ))
}
pub fn load_address() -> Result<String> {
    saved_address()?.context("No saved ring. Run: cargo run -- pair")
}
pub fn save_json(path: &Path, value: &serde_json::Value) -> Result<()> {
    fs::create_dir_all(path.parent().context("Missing parent directory")?)?;
    let mut file = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    writeln!(file, "{}", serde_json::to_string_pretty(value)?)?;
    file.persist(path)?;
    Ok(())
}
pub fn save_address(address: &str) -> Result<()> {
    if saved_address()?.is_some_and(|saved| saved.eq_ignore_ascii_case(address)) {
        return Ok(());
    }
    save_json(
        &directory().join("device.json"),
        &json!({"address": address}),
    )
}
pub fn save_cursor(address: &str, next: u16) -> Result<()> {
    let hash = format!("{:x}", Sha256::digest(address.to_lowercase().as_bytes()));
    save_json(
        &directory().join(format!("cursor-{}.json", &hash[..16])),
        &json!({"next_index": next}),
    )
}
pub struct BluetoothLock(File);
impl BluetoothLock {
    pub fn acquire() -> Result<Self> {
        Self::at(&directory().join("bluetooth.lock"))
    }
    pub fn at(path: &Path) -> Result<Self> {
        fs::create_dir_all(path.parent().unwrap())?;
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        if let Err(error) = file.try_lock_exclusive() {
            if error.kind() != std::io::ErrorKind::WouldBlock {
                return Err(error.into());
            }
            let mut owner = String::new();
            file.read_to_string(&mut owner)?;
            bail!(
                "pebble-indexのBLE処理がすでに起動しています (PID {})。先に起動したプロセスをCtrl+Cで停止してください。",
                owner.trim()
            );
        }
        file.rewind()?;
        file.set_len(0)?;
        write!(file, "{}", std::process::id())?;
        file.flush()?;
        Ok(Self(file))
    }
}
impl Drop for BluetoothLock {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.0);
    }
}
