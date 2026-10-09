use crate::settings::Settings;
use anyhow::{Result, ensure};
use pebble_core::output::Output;
use sha2::{Digest, Sha256};
use std::{io::Read, path::Path};
const URL: &str = "https://huggingface.co/ggerganov/whisper.cpp/resolve/c521a4b02f422512d734391fdf08bb08c0862f68/ggml-large-v3.bin";
const SHA256: &str = "64d182b440b98d5203c4f9bd541544d84c605196c4f7b845dfa11fb23594d1e2";
fn hash(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut sha = Sha256::new();
    let mut buffer = vec![0; 1024 * 1024];
    loop {
        let size = file.read(&mut buffer)?;
        if size == 0 {
            break;
        }
        sha.update(&buffer[..size]);
    }
    Ok(format!("{:x}", sha.finalize()))
}
pub async fn download(output: Output) -> Result<()> {
    let path = Settings::load()?.model_path();
    let parent = path.parent().unwrap();
    std::fs::create_dir_all(parent)?;
    let lock_path = parent.join("whisper-download.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock)?;
    if path.is_file() && hash(&path)? == SHA256 {
        output.line("Whisper large-v3 is ready.");
        return Ok(());
    }
    let partial = path.with_extension("bin.download");
    if partial.is_file() && hash(&partial)? == SHA256 {
        std::fs::rename(&partial, &path)?;
        output.line("Whisper large-v3 is ready.");
        return Ok(());
    }
    output.line("Downloading Whisper large-v3 (3.1 GB)…");
    let status = tokio::process::Command::new("/usr/bin/curl")
        .args(["--fail", "--location", "--continue-at", "-", "--output"])
        .arg(&partial)
        .arg(URL)
        .kill_on_drop(true)
        .status()
        .await?;
    ensure!(status.success(), "Model download failed: {status}");
    output.line("Checking Whisper model SHA-256…");
    ensure!(
        hash(&partial)? == SHA256,
        "Model checksum mismatch; remove {} and download again",
        partial.display()
    );
    std::fs::rename(partial, path)?;
    output.line("Whisper large-v3 is ready.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_streams_exact_bytes() {
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), b"abc").unwrap();
        assert_eq!(
            hash(file.path()).unwrap(),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
