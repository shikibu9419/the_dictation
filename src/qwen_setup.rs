use crate::{
    output::Output,
    qwen_runtime::{self, FILES, MODEL, Manifest, ModelFile, REVISION},
    settings::Settings,
};
use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Path, PathBuf},
};
use tokio::process::Command;

fn digest(path: &Path) -> Result<String> {
    let mut input = std::fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut block = vec![0; 1024 * 1024];
    loop {
        let n = input.read(&mut block)?;
        if n == 0 {
            break;
        }
        digest.update(&block[..n]);
    }
    Ok(format!("{:x}", digest.finalize()))
}
async fn matches(path: PathBuf, expected: &'static str) -> Result<bool> {
    tokio::task::spawn_blocking(move || {
        if !path.is_file() {
            return Ok(false);
        }
        Ok(digest(&path)? == expected)
    })
    .await?
}
pub async fn setup(output: Output) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Qwen3-ASR MLX requires Apple Silicon"
    );
    qwen_runtime::executable()?;
    setup_model(Settings::qwen_dir(), output).await
}
pub(crate) async fn setup_model(root: PathBuf, output: Output) -> Result<()> {
    let dest = root.join("model");
    std::fs::create_dir_all(&dest)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("setup.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Qwen3-ASR setup is already running")?;
    output.line("Preparing Qwen3-ASR model (2.2 GB); checking SHA-256 of existing files…");
    let mut files = Vec::new();
    for &(name, expected) in FILES {
        let target = dest.join(name);
        if !matches(target.clone(), expected).await? {
            let partial = dest.join(format!("{name}.download"));
            if !matches(partial.clone(), expected).await? {
                output.line(format!("Downloading {name}…"));
                let url = format!("https://huggingface.co/{MODEL}/resolve/{REVISION}/{name}");
                // Retry corrupt partial data from zero. An existing model is
                // kept until its replacement has passed SHA-256 verification.
                let mut verified = false;
                for resume in [true, false] {
                    let mut command = Command::new("/usr/bin/curl");
                    command.args([
                        "--fail",
                        "--location",
                        "--retry",
                        "3",
                        "--proto",
                        "=https",
                        "--proto-redir",
                        "=https",
                    ]);
                    if resume {
                        command.args(["--continue-at", "-"]);
                    }
                    let status = command
                        .arg("--output")
                        .arg(&partial)
                        .arg(&url)
                        .kill_on_drop(true)
                        .status()
                        .await?;
                    if status.success() && matches(partial.clone(), expected).await? {
                        verified = true;
                        break;
                    }
                }
                ensure!(verified, "Qwen model download/checksum failed: {name}");
            }
            std::fs::rename(&partial, &target)?;
        }
        files.push(ModelFile::verified(&target, name, expected)?);
    }
    let manifest = Manifest::new(files);
    manifest.validate(&root)?;
    let mut temp = tempfile::NamedTempFile::new_in(&root)?;
    serde_json::to_writer_pretty(&mut temp, &manifest)?;
    temp.write_all(b"\n")?;
    temp.as_file().sync_all()?;
    temp.persist(root.join("native-model.json"))?;
    output.line("Qwen3-ASR native MLX is ready.");
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn digest_checks_actual_bytes_and_handles_missing_files() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("asset");
        const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
        assert!(!matches(file.clone(), ABC).await.unwrap());
        std::fs::write(&file, b"abc").unwrap();
        assert!(matches(file.clone(), ABC).await.unwrap());
        std::fs::write(&file, b"abd").unwrap();
        assert!(!matches(file, ABC).await.unwrap());
    }
}
