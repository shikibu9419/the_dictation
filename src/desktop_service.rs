use crate::{
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Context, Result};
use pebble_index::ipc::{self, Lines};
use serde_json::{Value, json};
use tokio::{io::BufReader, process::Command};

/// UI commands run independently of BLE reception and recognition workers.
pub async fn run(output: Output) -> Result<()> {
    let exe = executable("Paste", include_str!("../native/Paste.swift"), &output).await?;
    let mut helper = Helper::spawn(Command::new(exe), output.clone(), "paste".into()).await?;
    let mut lines = Lines::new(BufReader::new(tokio::io::stdin()), ipc::MAX_LINE_BYTES);
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { helper.close().await; return Ok(()) };
                let mut message: Value = serde_json::from_str(&line).context("Invalid UI command")?;
                match message["type"].as_str() {
                    Some("paste" | "paste_current" | "copy" | "capture_target" | "forget_target" | "permission") => {
                        if matches!(message["type"].as_str(), Some("paste" | "paste_current")) {
                            let now = chrono::Utc::now().timestamp_millis();
                            let elapsed = |field: &str| message[field].as_i64().map(|sent| now.saturating_sub(sent).max(0));
                            output.debug(format!("Paste command request={} collections={}..{} gesture_to_backend_ms={:?} gui_to_backend_ms={:?}",
                                message["request"], message["first_collection"], message["last_collection"], elapsed("gesture_emitted_at_ms"), elapsed("gui_sent_at_ms")));
                            message["backend_sent_at_ms"] = json!(now);
                        }
                        helper.send(&message).await?;
                    }
                    _ => output.debug("Ignoring unknown UI command"),
                }
            }
            event = helper.event() => {
                let event = event?;
                if event["type"] != "ready" {
                    // Deliberately exclude dictated text from control logs.
                    output.debug(format!("desktop service: {event}"));
                    output.event(&event);
                }
            }
        }
    }
}
pub fn error(output: &Output, error: &anyhow::Error) {
    output.event(&json!({"type":"paste_result","success":false,"text":format!("Paste service failed: {error:#}")}));
}
