use std::process::Command;
#[test]
fn native_right_option_distinguishes_sides_and_deduplicates_edges() {
    let dir = tempfile::tempdir().unwrap();
    let exe = dir.path().join("microphone");
    let compile = Command::new("xcrun")
        .args([
            "swiftc",
            "-parse-as-library",
            "native/Microphone.swift",
            "-o",
        ])
        .arg(&exe)
        .output()
        .unwrap();
    assert!(
        compile.status.success(),
        "{}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let result = Command::new(exe).arg("--self-test").output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(String::from_utf8_lossy(&result.stdout).contains("7 assertions passed"));
}
