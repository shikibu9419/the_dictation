use crate::output::Output;
use anyhow::{Result, ensure};
use tokio::process::Command;

pub async fn run(output: Output) -> Result<()> {
    let exe = std::env::current_exe()?.with_file_name("index-voice");
    ensure!(
        exe.exists(),
        "Build the GUI first: cargo build --release --bins"
    );
    let mut command = Command::new(exe);
    command.arg("--backend").arg(std::env::current_exe()?);
    if output.verbose {
        command.arg("--verbose");
    }
    if let Some(path) = &output.log_path {
        command.arg("--log").arg(path);
    }
    let status = command.kill_on_drop(true).status().await?;
    ensure!(status.success(), "Floating panel exited: {status}");
    Ok(())
}
