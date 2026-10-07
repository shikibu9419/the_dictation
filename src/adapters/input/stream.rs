use crate::{
    output::Output,
    recognition::{Client, Options, display_event},
};
use anyhow::{Context, Result, ensure};
use serde_json::Value;
use std::{path::PathBuf, process::Stdio};
use tokio::{
    io::{AsyncBufRead, AsyncBufReadExt, BufReader},
    process::Command,
};

/// The source executable owns device capture; stdout carries PCM input events.
pub async fn run(program: PathBuf, language: String, output: Output) -> Result<()> {
    run_source(program, language, output, false).await
}
pub async fn microphone(language: String, output: Output) -> Result<()> {
    let program = crate::helper::executable(
        "Microphone",
        include_str!("../../../native/Microphone.swift"),
        &output,
    )
    .await?;
    run_source(program, language, output, true).await
}
async fn run_source(
    program: PathBuf,
    language: String,
    output: Output,
    await_ready: bool,
) -> Result<()> {
    let mut source = Command::new(program)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .context("Start input adapter")?;
    let mut diagnostics = BufReader::new(source.stderr.take().context("Input adapter stderr")?);
    let logs = output.clone();
    let logger = tokio::spawn(async move {
        let mut bytes = Vec::new();
        loop {
            bytes.clear();
            match diagnostics.read_until(b'\n', &mut bytes).await {
                Ok(0) => break,
                Ok(_) => logs.error(String::from_utf8_lossy(&bytes).trim_end_matches(['\r', '\n'])),
                Err(error) => {
                    logs.error(format!("Input adapter log: {error}"));
                    break;
                }
            }
        }
    });
    let reader = BufReader::new(source.stdout.take().context("Input adapter stdout")?);
    let result = receive(reader, language, output, await_ready).await;
    if result.is_err() {
        let _ = source.kill().await;
    }
    let status = source.wait().await?;
    logger.await?;
    result?;
    ensure!(status.success(), "Input adapter exited: {status}");
    Ok(())
}
async fn receive(
    reader: impl AsyncBufRead + Unpin,
    language: String,
    output: Output,
    await_ready: bool,
) -> Result<()> {
    let mut client = Client::start(
        Options {
            address: "pcm".into(),
            language,
            command: "stream".into(),
            verbose: output.verbose,
        },
        output.clone(),
    )
    .await?;
    if !await_ready {
        output.event(&serde_json::json!({"type":"ready"}));
    }
    let mut lines = reader.lines();
    loop {
        tokio::select! {
            line = lines.next_line() => {
                let Some(line) = line? else { break };
                let message: Value = serde_json::from_str(&line).context("Invalid input adapter JSON")?;
                if message["type"] == "source_ready" {
                    output.event(&serde_json::json!({"type":"ready"}));
                    output.line("Microphone ready: hold right Option to speak.");
                    continue;
                }
                if message["type"] == "state" { output.event(&message); }
                client.send(message)?;
            }
            event = client.events.recv() => display_event(event.context("Recognition worker closed")??, &output)?,
        }
    }
    client.flush(&output).await
}
