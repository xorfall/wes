//! Execute the actual example files against a synthetic loopback SSE service.
#[path = "support/python.rs"]
mod python;
#[test]
fn live_monitor_example_tracks_windows_and_restores_without_reconnecting() {
    let checker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/live-service-monitor/check.py");
    let output = python::command()
        .arg(checker)
        .arg("--binary")
        .arg(env!("CARGO_BIN_EXE_wes"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
