use crate::output::Output;
use anyhow::{Context, Result, bail};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::{Mutex, mpsc},
};

pub async fn executable(name: &str, source: &str, output: &Output) -> Result<PathBuf> {
    let bundled = std::env::current_exe()?.with_file_name(name);
    if bundled.is_file() {
        return Ok(bundled);
    }
    let version = Command::new("xcrun")
        .args(["swiftc", "--version"])
        .output()
        .await
        .context("Install Xcode 26+ and select its Command Line Tools")?;
    if !version.status.success() {
        bail!(
            "Swift compiler unavailable: {}",
            String::from_utf8_lossy(&version.stderr)
        );
    }
    let mut hash = Sha256::new();
    hash.update(source);
    hash.update(&version.stdout);
    hash.update(std::env::consts::ARCH);
    let identity = format!("{:x}", hash.finalize());
    let cache = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap()).join("Library/Caches"))
        .join("pebble-index-rust");
    let target = cache.join(format!("rust-{name}-{}", &identity[..20]));
    if target.exists() {
        return Ok(target);
    }
    std::fs::create_dir_all(&cache)?;
    output.debug(format!("Building {name} helper…"));
    let temp = tempfile::tempdir_in(&cache)?;
    let input = temp.path().join(format!("{name}.swift"));
    std::fs::write(&input, source)?;
    let binary = temp.path().join(name);
    let architecture = if cfg!(target_arch = "aarch64") {
        "arm64"
    } else {
        "x86_64"
    };
    let result = Command::new("xcrun")
        .args(["swiftc", "-O", "-parse-as-library", "-target"])
        .arg(format!("{architecture}-apple-macos26.0"))
        .arg(input)
        .arg("-o")
        .arg(&binary)
        .kill_on_drop(true)
        .output()
        .await?;
    if !result.status.success() {
        bail!(
            "{name} helper build failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
    std::fs::rename(binary, &target)?;
    Ok(target)
}

pub struct Helper {
    pub child: Child,
    stdin: HelperInput,
    pub events: mpsc::UnboundedReceiver<Result<Value>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct HelperInput(Arc<Mutex<Option<ChildStdin>>>);
impl HelperInput {
    pub async fn send(&self, message: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        bytes.push(b'\n');
        let mut handle = self.0.lock().await;
        let stdin = handle.as_mut().context("Helper input closed")?;
        stdin.write_all(&bytes).await?;
        stdin.flush().await?;
        Ok(())
    }
}
pub type Observer = Box<dyn Fn(&Result<Value>) + Send>;
impl Helper {
    pub async fn spawn(command: Command, output: Output, label: String) -> Result<Self> {
        Self::spawn_observed(command, output, label, None).await
    }
    pub async fn spawn_observed(
        mut command: Command,
        output: Output,
        label: String,
        observer: Option<Observer>,
    ) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()?;
        let stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap()).lines();
        let mut stderr = BufReader::new(child.stderr.take().unwrap());
        let (tx, events) = mpsc::unbounded_channel();
        let logs = tx.clone();
        let readers = vec![
            tokio::spawn(async move {
                loop {
                    let event = match stdout.next_line().await {
                        Ok(Some(line)) => {
                            serde_json::from_str(&line).context("Invalid helper JSON")
                        }
                        Ok(None) => Err(anyhow::anyhow!("Helper output closed")),
                        Err(e) => Err(e.into()),
                    };
                    let failed = event.is_err();
                    if let Some(observer) = &observer {
                        observer(&event);
                    }
                    if tx.send(event).is_err() || failed {
                        break;
                    }
                }
            }),
            tokio::spawn(async move {
                let mut bytes = Vec::new();
                loop {
                    bytes.clear();
                    match stderr.read_until(b'\n', &mut bytes).await {
                        Ok(0) => break,
                        Ok(_) => {
                            // Native engines may log token fragments that are not complete UTF-8.
                            let line = String::from_utf8_lossy(&bytes);
                            let line = line.trim_end_matches(['\r', '\n']);
                            if label.is_empty() {
                                output.error(line)
                            } else {
                                output.debug(format!("[{label}] {line}"))
                            }
                        }
                        Err(e) => {
                            let _ = logs.send(Err(e.into()));
                            break;
                        }
                    }
                }
            }),
        ];
        Ok(Self {
            child,
            stdin: HelperInput(Arc::new(Mutex::new(Some(stdin)))),
            events,
            readers,
        })
    }
    pub async fn send(&mut self, message: &Value) -> Result<()> {
        self.stdin.send(message).await
    }
    pub fn input(&self) -> HelperInput {
        self.stdin.clone()
    }
    pub async fn event(&mut self) -> Result<Value> {
        self.events.recv().await.context("Helper closed")?
    }
    pub async fn close(&mut self) {
        // Closing the handle delivers EOF; shutdown() alone leaves the pipe open on macOS.
        drop(self.stdin.0.lock().await.take());
        if tokio::time::timeout(Duration::from_secs(3), self.child.wait())
            .await
            .is_err()
        {
            let _ = self.child.kill().await;
        }
    }
}
impl Drop for Helper {
    fn drop(&mut self) {
        for reader in &self.readers {
            reader.abort();
        }
    }
}

pub struct ProcessGroup(pub i32);
impl Drop for ProcessGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-self.0, libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn close_delivers_eof_before_waiting_and_is_idempotent() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "cat >/dev/null; exit 17"]);
        let mut helper = Helper::spawn(command, Output::new(false, None).unwrap(), String::new())
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), helper.close())
            .await
            .unwrap();
        assert_eq!(helper.child.try_wait().unwrap().unwrap().code(), Some(17));
        helper.close().await;
        assert!(
            helper
                .send(&serde_json::json!({"type":"audio"}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn native_token_fragments_do_not_break_protocol() {
        let dir = tempfile::tempdir().unwrap();
        let log = dir.path().join("log");
        let output = Output::new(true, Some(&log)).unwrap();
        let mut command = Command::new("/bin/sh");
        command.args([
            "-c",
            r#"printf '\344\272\n' >&2; printf '{"type":"ready"}\n'; read line"#,
        ]);
        let mut helper = Helper::spawn(command, output, "native".into())
            .await
            .unwrap();
        assert_eq!(helper.event().await.unwrap()["type"], "ready");
        helper.close().await;
        tokio::task::yield_now().await;
        assert!(std::fs::read_to_string(log).unwrap().contains('\u{fffd}'));
    }
}
