use crate::output::Output;
use anyhow::{Context, Result, bail};
use pebble_index::ipc::{self, Lines};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{path::PathBuf, process::Stdio, sync::Arc, time::Duration};
use tokio::{
    io::{AsyncWriteExt, BufReader},
    process::{Child, ChildStdin, Command},
    sync::Mutex,
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
    pub events: ipc::Receiver<Result<Value>>,
    readers: Vec<tokio::task::JoinHandle<()>>,
}
#[derive(Clone)]
pub struct HelperInput(Arc<Mutex<Option<ChildStdin>>>);
impl HelperInput {
    pub async fn send(&self, message: &Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(message)?;
        anyhow::ensure!(
            bytes.len() <= ipc::MAX_LINE_BYTES,
            "Helper input exceeds {} bytes",
            ipc::MAX_LINE_BYTES
        );
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
        let mut stdout = Lines::new(
            BufReader::new(child.stdout.take().unwrap()),
            ipc::MAX_LINE_BYTES,
        );
        let mut stderr = Lines::log_chunks(
            BufReader::new(child.stderr.take().unwrap()),
            ipc::MAX_LOG_LINE_BYTES,
        );
        let (tx, events) = ipc::process_channel("helper results");
        let logs = tx.clone();
        let readers = vec![
            tokio::spawn(async move {
                loop {
                    let event = match stdout.next_line().await {
                        Ok(Some(line)) => {
                            serde_json::from_str(&line).context("Invalid helper JSON")
                        }
                        Ok(None) => Err(anyhow::anyhow!("Helper output closed")),
                        Err(e) => Err(e),
                    };
                    let failed = event.is_err();
                    if let Some(observer) = &observer {
                        observer(&event);
                    }
                    if let Err(error) = tx.send(event) {
                        if let Some(observer) = &observer {
                            observer(&Err(error));
                        }
                        break;
                    }
                    if failed {
                        break;
                    }
                }
            }),
            tokio::spawn(async move {
                loop {
                    match stderr.next_bytes().await {
                        Ok(None) => break,
                        Ok(Some(bytes)) => {
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
                            logs.fail(e);
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
        self.events.recv().await?.context("Helper closed")?
    }
    pub async fn close(&mut self) {
        // Closing the handle delivers EOF; shutdown() alone leaves the pipe open on macOS.
        // Include acquiring stdin in the deadline: a stuck writer must not
        // prevent its own owner from being stopped and replaced.
        let closed = tokio::time::timeout(Duration::from_secs(3), async {
            drop(self.stdin.0.lock().await.take());
            self.child.wait().await
        })
        .await;
        if !matches!(closed, Ok(Ok(_))) {
            let _ = self.child.kill().await;
            if let Ok(mut stdin) = self.stdin.0.try_lock() {
                drop(stdin.take());
            }
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
    async fn shutdown_deadline_includes_a_stuck_stdin_writer() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "read line"]);
        let mut helper = Helper::spawn(command, Output::new(false, None).unwrap(), "test".into())
            .await
            .unwrap();
        let input = helper.stdin.0.clone();
        let held = input.lock().await;
        tokio::time::timeout(Duration::from_secs(4), helper.close())
            .await
            .unwrap();
        assert!(helper.child.try_wait().unwrap().is_some());
        drop(held);
        helper.close().await;
        assert!(
            helper
                .send(&serde_json::json!({"type":"audio"}))
                .await
                .is_err()
        );
    }
    #[tokio::test]
    async fn control_observer_keeps_running_with_a_stalled_consumer_and_sees_overflow() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let acknowledged = Arc::new(AtomicBool::new(false));
        let observed = acknowledged.clone();
        let (failed, failure) = tokio::sync::oneshot::channel();
        let failed = std::sync::Mutex::new(Some(failed));
        let observer: Observer = Box::new(move |event| {
            if event.as_ref().is_ok_and(|v| v["type"] == "execution_state") {
                observed.store(true, Ordering::SeqCst);
            }
            if let Err(error) = event
                && let Some(failed) = failed.lock().unwrap().take()
            {
                let _ = failed.send(error.to_string());
            }
        });
        let mut command = Command::new("/bin/sh");
        command.args(["-c", r#"i=0; while [ "$i" -lt 4096 ]; do printf '{"type":"partial","text":"text"}\n'; i=$((i + 1)); done; printf '{"type":"execution_state","permitted":false}\n'; read line"#]);
        let mut helper = Helper::spawn_observed(
            command,
            Output::new(false, None).unwrap(),
            "test".into(),
            Some(observer),
        )
        .await
        .unwrap();
        // Deliberately do not consume helper.event() until the queue fills.
        let error = tokio::time::timeout(Duration::from_secs(5), failure)
            .await
            .unwrap()
            .unwrap();
        assert!(acknowledged.load(Ordering::SeqCst));
        assert!(error.contains("queue capacity exceeded"), "{error}");
        assert!(
            helper
                .event()
                .await
                .unwrap_err()
                .to_string()
                .contains("queue capacity exceeded")
        );
        helper.close().await;
    }
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
