use std::{os::unix::fs::PermissionsExt, process::Command};
fn executable(path: &std::path::Path, source: &str) {
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
