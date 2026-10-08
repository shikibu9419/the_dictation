use base64::Engine;
use std::{os::unix::fs::PermissionsExt, process::Command};
fn executable(path: &std::path::Path, source: &str) {
    let source = source.replace("AQACAA==", &base64::engine::general_purpose::STANDARD.encode(vec![0u8; 8000]));
    std::fs::write(path, source).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
#[test]
fn external_input_and_engine_use_pcm_contract_and_capture_source_logs() {
    let temp = tempfile::tempdir().unwrap();
    let engine = temp.path().join("engine");
    let source = temp.path().join("source");
    let log = temp.path().join("log");
    executable(
        &engine,
        r#"#!/bin/sh
printf '{"type":"ready"}\n'
while IFS= read -r line; do
case "$line" in
*'"type":"audio"'*) printf '{"type":"accepted"}\n';;
*'"type":"finish"'*) printf '{"type":"final","text":"adapter final result"}\n';;
*'"type":"cancel"'*) printf '{"type":"cancelled"}\n';;
esac
done
"#,
    );
    executable(
        &source,
        r#"#!/bin/sh
printf 'microphone diagnostic\n' >&2
printf '{"type":"state","collecting":true}\n'
printf '{"type":"audio","key":"mic-one","rate":16000,"pcm":"AQACAA==","final":false}\n'
printf '{"type":"state","collecting":false}\n'
printf '{"type":"audio","key":"mic-one","rate":16000,"pcm":"","final":true}\n'
"#,
    );
    let output = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["stream", "--input-command"])
        .arg(&source)
        .arg("--log")
        .arg(&log)
        .env("INDEX_VOICE_SPEECH_COMMAND", &engine)
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("adapter final result"));
    let log = std::fs::read_to_string(log).unwrap();
    assert!(log.contains("microphone diagnostic"));
    assert!(log.contains("adapter final result"));
    assert!(
        !temp
            .path()
            .join("pebble-index-rust/bluetooth.lock")
            .exists()
    );
    executable(
        &source,
        r#"#!/bin/sh
printf '{"type":"audio","key":"unfinished","rate":16000,"pcm":"AQACAA==","final":false}\n'
"#,
    );
    let incomplete = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["stream", "--input-command"])
        .arg(&source)
        .env("INDEX_VOICE_SPEECH_COMMAND", &engine)
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    assert!(!incomplete.status.success());
    assert!(String::from_utf8_lossy(&incomplete.stderr).contains("Input ended before final audio"));
}

#[test]
fn batch_only_plan_starts_one_engine_and_flushes_repeated_recordings() {
    let temp = tempfile::tempdir().unwrap();
    let engine = temp.path().join("engine");
    let source = temp.path().join("source");
    let modes = temp.path().join("modes");
    let config = temp.path().join("pebble-index-rust");
    std::fs::create_dir(&config).unwrap();
    std::fs::write(config.join("settings.json"), r#"{"presentation":{"live_text":false,"final_text":false},"batch_speech":"apple"}"#).unwrap();
    executable(&engine, r#"#!/bin/sh
printf '%s\n' "$2" >> "$ENGINE_MODES"
printf '{"type":"ready"}\n'
while IFS= read -r line; do
case "$line" in
*'"type":"audio"'*) printf '{"type":"accepted"}\n';;
*'"type":"finish"'*) printf '{"type":"final","text":"complete batch"}\n';;
*'"type":"cancel"'*) printf '{"type":"cancelled"}\n';;
esac
done
"#);
    executable(&source, r#"#!/bin/sh
for key in one two three; do
printf '{"type":"state","collecting":true}\n'
printf '{"type":"audio","key":"%s","rate":16000,"pcm":"AQACAA==","final":false}\n' "$key"
printf '{"type":"state","collecting":false}\n'
printf '{"type":"audio","key":"%s","rate":16000,"pcm":"AQACAA==","final":true}\n' "$key"
done
"#);
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["stream", "--input-command"]).arg(source)
        .env("INDEX_VOICE_SPEECH_COMMAND", engine)
        .env("ENGINE_MODES", &modes)
        .env("XDG_CONFIG_HOME", temp.path())
        .output().unwrap();
    assert!(result.status.success(), "{}", String::from_utf8_lossy(&result.stderr));
    assert_eq!(std::fs::read_to_string(modes).unwrap(), "batch\n");
    assert_eq!(String::from_utf8_lossy(&result.stdout).matches("complete batch").count(), 3);
}
