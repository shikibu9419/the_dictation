use std::process::Command;

#[test]
fn gui_backend_errors_are_json_and_diagnostics_stay_on_stderr() {
    use std::process::Stdio;
    let config = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_pebble-index"))
        .args(["listen", "--gui-events", "-v"])
        .env("XDG_CONFIG_HOME", config.path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let _input = child.stdin.take().unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(!result.status.success());
    let stdout = String::from_utf8(result.stdout).unwrap();
    let events: Vec<serde_json::Value> = stdout
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0]["type"], "error");
    assert!(
        events[0]["text"]
            .as_str()
            .unwrap()
            .contains("No saved ring")
    );
    assert!(String::from_utf8_lossy(&result.stderr).contains("start pid="));
}
