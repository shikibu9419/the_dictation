//! Push-to-talk end to end without hardware: a scripted Bluetooth bridge plays
//! a long press whose audio is a spoken request, and the real Realtime API
//! must answer with a transcript, an add_todo call and speech. Needs
//! `OPENAI_API_KEY` and macOS `say`; ignored by default.
use base64::{Engine, engine::general_purpose::STANDARD};
use pebble_ring::bluetooth::{CONTROL, DATA};
use serde_json::{Value, json};
use std::{os::unix::fs::PermissionsExt, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::Command,
};

const RATE: u32 = 9997;
const PART_SAMPLES: usize = 2000;

/// One ring collection: raw PCM (80), audio metadata (82) and button history (83).
fn collection(samples: &[i16], first_index: u32, final_part: bool) -> Vec<u8> {
    let mut records = vec![80];
    records.extend((4 + samples.len() as u32 * 2).to_le_bytes());
    records.extend(RATE.to_le_bytes());
    for sample in samples {
        records.extend(sample.to_le_bytes());
    }
    records.push(82);
    records.extend(6u16.to_le_bytes());
    records.extend(first_index.to_le_bytes());
    records.extend([1, final_part as u8]);
    records.push(83);
    records.extend(8u16.to_le_bytes());
    records.extend(1u32.to_le_bytes()); // one long press
    records.extend(1u32.to_le_bytes());
    let mut raw = ((records.len() + 4) as u32).to_le_bytes().to_vec();
    raw.extend(records);
    raw
}

fn speech(dir: &Path) -> Vec<i16> {
    let wav = dir.join("request.wav");
    let status = std::process::Command::new("say")
        .args(["-v", "Kyoko", "-o"])
        .arg(&wav)
        .arg(format!("--data-format=LEI16@{RATE}"))
        .arg("牛乳を買うをTODOに追加して")
        .status()
        .expect("run say");
    assert!(status.success(), "say failed");
    let mut reader = hound::WavReader::open(&wav).unwrap();
    assert_eq!(reader.spec().sample_rate, RATE);
    reader.samples::<i16>().map(Result::unwrap).collect()
}

/// Scripted Bluetooth helper. Every poll of the ring state advances a
/// timeline: idle, then pressing with one new audio part per poll, then
/// released with the final part available.
fn bridge_script(parts: &[Vec<u8>]) -> String {
    let parts: Vec<String> = parts.iter().map(|p| STANDARD.encode(p)).collect();
    format!(
        r#"#!/usr/bin/env python3
import base64, json, struct, sys
CONTROL = {control:?}
DATA = {data:?}
PARTS = {parts}
M = len(PARTS)
polls = 0

def emit(value):
    sys.stdout.write(json.dumps(value) + "\n"); sys.stdout.flush()

def notify(payload):
    header = struct.pack("<III", 0, 0, len(payload))
    emit({{"type": "notification", "uuid": CONTROL, "data": base64.b64encode(header).decode()}})
    emit({{"type": "notification", "uuid": DATA, "data": base64.b64encode(payload).decode()}})

def available():
    return 0 if polls < 3 else min(polls - 3, M)

def pressing():
    return 3 <= polls < 3 + M

emit({{"type": "ready"}})
for line in sys.stdin:
    request = json.loads(line)
    emit({{"type": "reply", "id": request.get("id"), "value": None}})
    if request.get("type") != "write" or request.get("uuid") != CONTROL:
        continue
    packet = base64.b64decode(request["data"])
    address = struct.unpack("<I", packet[1:5])[0]
    if address == 0x4003000E:
        polls += 1
        count = 1 + available()
        flags = 32 if pressing() else 0
        notify(bytes([0, 0, 255, 255]) + struct.pack("<I", 1) + bytes([count, flags]))
    elif address == 0x40030005:
        notify(struct.pack("<HH", 1, 1 + available()))
    elif address & 0xFFFF0000 == 0x40020000:
        index = address & 0xFFFF
        notify(base64.b64decode(PARTS[index - 1]) if 1 <= index <= M else b"")
"#,
        control = CONTROL,
        data = DATA,
        parts = json!(parts),
    )
}

const PLAYBACK_SCRIPT: &str = r#"#!/bin/sh
printf '{"type":"ready"}\n'
played=0
while IFS= read -r line; do
  case "$line" in
    *'"type":"audio"'*) played=$((played + 100)); printf '{"type":"played","ms":%s}\n' "$played";;
    *'"type":"clear"'*) played=0; printf '{"type":"cleared"}\n';;
  esac
done
"#;

fn executable(path: &Path, content: &str) {
    std::fs::write(path, content).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}

#[tokio::test]
#[ignore]
async fn long_press_streams_speech_and_the_assistant_adds_the_todo() {
    let api_key = std::env::var("OPENAI_API_KEY").expect("OPENAI_API_KEY");
    let dir = tempfile::tempdir().unwrap();
    let pcm = speech(dir.path());
    assert!(pcm.len() > RATE as usize, "speech shorter than a second");
    let chunks: Vec<&[i16]> = pcm.chunks(PART_SAMPLES).collect();
    let parts: Vec<Vec<u8>> = chunks
        .iter()
        .enumerate()
        .map(|(i, chunk)| collection(chunk, 1, i + 1 == chunks.len()))
        .collect();

    // Helpers next to the executable take precedence over compiled ones.
    let bin = dir.path().join("bin");
    std::fs::create_dir(&bin).unwrap();
    std::fs::copy(env!("CARGO_BIN_EXE_pebble-jev"), bin.join("pebble-jev")).unwrap();
    executable(&bin.join("Bluetooth"), &bridge_script(&parts));
    executable(&bin.join("AudioPlayback"), PLAYBACK_SCRIPT);
    let config = dir.path().join("config");
    std::fs::create_dir_all(config.join("pebble-index-rust")).unwrap();
    std::fs::write(
        config.join("pebble-index-rust/device.json"),
        json!({"address": "synthetic-ring"}).to_string(),
    )
    .unwrap();

    let mut child = Command::new(bin.join("pebble-jev"))
        .args(["headless", "--verbose"])
        .env("XDG_CONFIG_HOME", &config)
        .env("OPENAI_API_KEY", api_key)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(std::fs::File::create(dir.path().join("session.log")).unwrap())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut lines = BufReader::new(child.stdout.take().unwrap()).lines();
    let mut recording = false;
    let mut user = String::new();
    let mut tool: Option<Value> = None;
    let mut assistant = String::new();
    let result = tokio::time::timeout(Duration::from_secs(120), async {
        while let Some(line) = lines.next_line().await.unwrap() {
            let event: Value = serde_json::from_str(&line).unwrap();
            match event["type"].as_str().unwrap_or("") {
                "recording" => recording = true,
                "error" => panic!("session error: {}", event["data"]),
                "conversation" => {
                    for entry in event["data"].as_array().unwrap() {
                        match entry["kind"].as_str().unwrap() {
                            "user" if entry["done"] == true => {
                                user = entry["text"].as_str().unwrap().to_owned();
                            }
                            "tool_call" if !entry["result"].is_null() => tool = Some(entry.clone()),
                            "assistant" if entry["done"] == true => {
                                assistant = entry["text"].as_str().unwrap().to_owned();
                            }
                            _ => {}
                        }
                    }
                    if recording && !user.is_empty() && tool.is_some() && !assistant.is_empty() {
                        return;
                    }
                }
                _ => {}
            }
        }
        panic!("session ended early");
    })
    .await;
    let log = std::fs::read_to_string(dir.path().join("session.log")).unwrap_or_default();
    assert!(
        result.is_ok(),
        "deadline; log tail:\n{}",
        log.lines().rev().take(40).collect::<Vec<_>>().join("\n")
    );
    eprintln!("user: {user}\ntool: {tool:?}\nassistant: {assistant}");
    // The press streamed live chunks before the final recording was committed.
    let commit = log
        .lines()
        .find(|line| line.contains("Recording session="))
        .expect("no commit in log");
    let sent_live: usize = commit
        .split("sent_live=")
        .nth(1)
        .and_then(|rest| rest.split_whitespace().next())
        .and_then(|n| n.parse().ok())
        .expect("sent_live in log");
    assert!(
        sent_live > 0,
        "nothing was streamed while pressing: {commit}"
    );
    let tool = tool.unwrap();
    assert_eq!(tool["name"], "add_todo");
    assert!(tool["arguments"].as_str().unwrap().contains("牛乳"));
    assert_eq!(tool["failed"], false);
    let todos: Vec<Value> =
        serde_json::from_slice(&std::fs::read(config.join("pebble-jev/todos.json")).unwrap())
            .unwrap();
    assert!(todos[0]["title"].as_str().unwrap().contains("牛乳"));
    child.kill().await.unwrap();
}
