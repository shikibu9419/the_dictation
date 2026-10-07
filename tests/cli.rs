use fs2::FileExt;
use std::{fs::OpenOptions, io::Write, process::Command};

#[test]
fn locked_rust_instance_prevents_any_ble_or_speech_startup() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("pebble-index-rust");
    std::fs::create_dir(&config).unwrap();
    let mut lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.join("bluetooth.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    write!(lock, "12345").unwrap();
    lock.flush().unwrap();
    let log = temp.path().join("listen");
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["-v", "--log"])
        .arg(&log)
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let text = std::fs::read_to_string(log).unwrap();
    assert!(text.contains("PID 12345"));
    assert!(text.contains("start pid="));
    assert!(text.contains("stop pid="));
    assert!(!text.contains("Preparing"));
    assert!(!text.contains("Connecting"));
}
#[test]
fn help_has_all_commands_and_shared_speech_settings() {
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    for name in [
        "pair",
        "scan",
        "inspect",
        "listen",
        "fetch",
        "serve",
        "transcribe",
        "microphone",
        "settings",
        "download-model",
    ] {
        assert!(text.contains(name));
    }
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["transcribe", "--help"])
        .output()
        .unwrap();
    assert!(result.status.success());
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(!text.contains("--model"));
    assert!(text.contains("ja-JP"));
}
#[test]
fn invalid_timeout_fails_before_bluetooth_access() {
    let temp = tempfile::tempdir().unwrap();
    for value in ["0", "NaN", "inf"] {
        let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
            .args(["--timeout", value])
            .env("XDG_CONFIG_HOME", temp.path())
            .output()
            .unwrap();
        assert!(!result.status.success());
        assert!(String::from_utf8_lossy(&result.stderr).contains("must be positive"));
    }
}

#[test]
fn foreign_configuration_and_lock_are_not_used() {
    let temp = tempfile::tempdir().unwrap();
    let foreign = temp.path().join("pebble-index");
    std::fs::create_dir(&foreign).unwrap();
    std::fs::write(foreign.join("device.json"), "not even valid JSON").unwrap();
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(foreign.join("bluetooth.lock"))
        .unwrap();
    lock.lock_exclusive().unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["listen", "--no-transcribe"])
        .env("XDG_CONFIG_HOME", temp.path())
        .output()
        .unwrap();
    assert!(!result.status.success());
    let error = String::from_utf8_lossy(&result.stderr);
    assert!(error.contains("No saved ring"), "{error}");
    assert!(!error.contains("すでに起動"));
    assert!(!temp.path().join("pebble-index-rust/device.json").exists());
    assert_eq!(
        std::fs::read_to_string(foreign.join("device.json")).unwrap(),
        "not even valid JSON"
    );
}

#[test]
fn settings_change_input_without_touching_pairing_and_reject_missing_model() {
    let temp = tempfile::tempdir().unwrap();
    let config = temp.path().join("pebble-index-rust");
    std::fs::create_dir(&config).unwrap();
    let device = r#"{"address":"B727FE94-D092-6484-A27E-A94795942322"}"#;
    std::fs::write(config.join("device.json"), device).unwrap();
    let run = |args: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_pebble-index"))
            .args(args)
            .env("XDG_CONFIG_HOME", temp.path())
            .env("XDG_CACHE_HOME", temp.path().join("cache"))
            .output()
            .unwrap()
    };
    let changed = run(&["settings", "--input", "microphone"]);
    assert!(
        changed.status.success(),
        "{}",
        String::from_utf8_lossy(&changed.stderr)
    );
    let settings: serde_json::Value = serde_json::from_slice(&changed.stdout).unwrap();
    assert_eq!(settings["input"], "microphone");
    assert_eq!(settings["speech"], "apple");
    let before = std::fs::read(config.join("settings.json")).unwrap();
    let missing = run(&["settings", "--speech", "whisper-large-v3"]);
    assert!(!missing.status.success());
    assert_eq!(std::fs::read(config.join("settings.json")).unwrap(), before);
    assert!(run(&["settings", "--input", "index"]).status.success());
    assert_eq!(
        std::fs::read_to_string(config.join("device.json")).unwrap(),
        device
    );
}
