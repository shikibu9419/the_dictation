use anyhow::{Result, ensure};
use serde_json::Value;
use std::{process::Stdio, time::Duration};
use tokio::process::Command;

struct Group(i32);
impl Drop for Group {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[tokio::test]
#[ignore = "Uses the actual macOS Bluetooth scanner for 30 seconds; no pairing or connection"]
async fn default_scan_replies_before_the_ipc_deadline() -> Result<()> {
    let child = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["-v", "scan", "--timeout", "30"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let _group = Group(child.id().unwrap() as i32);
    let result = tokio::time::timeout(Duration::from_secs(50), child.wait_with_output()).await??;
    let diagnostics = String::from_utf8_lossy(&result.stderr);
    ensure!(result.status.success(), "{diagnostics}");
    ensure!(
        serde_json::from_slice::<Value>(&result.stdout)?.is_array(),
        "Invalid scan results"
    );
    ensure!(
        diagnostics.contains("Bluetooth reply id=1 type=scan"),
        "No scan acknowledgement: {diagnostics}"
    );
    ensure!(
        !diagnostics.contains("already active"),
        "Scan timer left a pending command: {diagnostics}"
    );
    eprintln!("{diagnostics}");
    Ok(())
}
