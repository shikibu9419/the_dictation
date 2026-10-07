use crate::{output::Output, settings::Settings};
use anyhow::{Context, Result, ensure};
use std::path::PathBuf;
use tokio::process::Command;

pub async fn setup(output: Output) -> Result<()> {
    ensure!(
        cfg!(all(target_os = "macos", target_arch = "aarch64")),
        "Qwen3-ASR MLX requires Apple Silicon"
    );
    let root = Settings::qwen_dir();
    std::fs::create_dir_all(&root)?;
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .open(root.join("setup.lock"))?;
    fs2::FileExt::try_lock_exclusive(&lock).context("Qwen3-ASR setup is already running")?;
    let home = PathBuf::from(std::env::var_os("HOME").context("HOME missing")?);
    let uv = std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|p| p.join("uv"))
        .chain([
            home.join(".local/bin/uv"),
            PathBuf::from("/opt/homebrew/bin/uv"),
            PathBuf::from("/usr/local/bin/uv"),
        ])
        .find(|p| p.is_file())
        .context("Install uv first: https://docs.astral.sh/uv/getting-started/installation/")?;
    std::fs::write(
        root.join("pyproject.toml"),
        include_str!("../native/qwen/pyproject.toml"),
    )?;
    std::fs::write(root.join("uv.lock"), include_str!("../native/qwen/uv.lock"))?;
    output.line("Installing isolated Qwen3-ASR MLX runtime…");
    let status = Command::new(uv)
        .args(["sync", "--frozen", "--no-dev", "--project"])
        .arg(&root)
        .kill_on_drop(true)
        .status()
        .await?;
    ensure!(
        status.success(),
        "Qwen3-ASR runtime installation failed: {status}"
    );
    output.line("Downloading Qwen3-ASR model (2.2 GB) and checking SHA-256…");
    let status = Command::new(root.join(".venv/bin/python"))
        .arg("-u")
        .arg("-c")
        .arg(include_str!("../native/qwen/adapter.py"))
        .arg("--download")
        .arg(&root)
        .kill_on_drop(true)
        .status()
        .await?;
    ensure!(
        status.success(),
        "Qwen3-ASR model installation failed: {status}"
    );
    output.line("Qwen3-ASR MLX is ready.");
    Ok(())
}
