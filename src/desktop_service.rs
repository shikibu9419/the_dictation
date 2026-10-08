use crate::{
    helper::{Helper, executable},
    output::Output,
};
use anyhow::{Context, Result};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

/// UI commands run independently of BLE reception and recognition workers.
pub async fn run(output: Output) -> Result<()> {
    let exe = executable("Paste", include_str!("../native/Paste.swift"), &output).await?;
    let mut helper = Helper::spawn(Command::new(exe), output.clone(), "paste".into()).await?;
    let mut lines = BufReader::new(tokio::io::stdin()).lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { helper.close().await; return Ok(()) };
                let message: Value = serde_json::from_str(&line).context("Invalid UI command")?;
                match message["type"].as_str() {
                    Some("paste" | "copy" | "capture_target" | "forget_target" | "permission") => helper.send(&message).await?,
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
