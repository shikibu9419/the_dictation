use crate::{config, output::Output};
use serde_json::json;

#[test]
fn lock_rejects_second_owner_and_releases() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("bluetooth.lock");
    let first = config::BluetoothLock::at(&path).unwrap();
    assert!(config::BluetoothLock::at(&path).is_err());
    drop(first);
    let second = config::BluetoothLock::at(&path).unwrap();
    drop(second);
    assert!(path.exists());
}
#[test]
fn log_contains_stdout_stderr_and_enables_debug() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.log");
    let output = Output::new(false, Some(&path)).unwrap();
    assert!(output.verbose);
    output.line("transcript");
    output.error("diagnostic");
    output.debug("debug marker");
    let content = std::fs::read_to_string(path).unwrap();
    for text in ["transcript", "diagnostic", "debug marker"] {
        assert!(content.contains(text));
    }
}
#[test]
fn final_transcript_printed_even_when_same_as_partial() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("test.log");
    let output = Output::new(false, Some(&path)).unwrap();
    output.transcript(" hello   world ", false);
    output.transcript("hello world", false);
    output.transcript("hello world", true);
    assert_eq!(
        std::fs::read_to_string(path).unwrap(),
        "hello world\nhello world\n"
    );
}
#[test]
fn saved_address_format_compatible() {
    let temp = tempfile::tempdir().unwrap();
    let path = temp.path().join("device.json");
    config::save_json(&path, &json!({"address":"example"})).unwrap();
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&std::fs::read(path).unwrap()).unwrap()["address"],
        "example"
    );
}
