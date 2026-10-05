//! Full stdio/PTY/application acceptance without a model account or user's workspace.
#[test]
fn mcp_communication_metrics_measure_real_stdio_without_payloads_or_protocol_changes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/agent-metrics/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python MCP metrics fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn agents_inspect_large_values_and_failed_cells_without_unbounded_exports() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/agent-inspection/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python inspection fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[cfg(unix)]
fn terminal_assistants_share_the_open_workspace_through_mcp() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/assistant/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python synthetic MCP fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn multiple_assistants_join_shared_workspaces_with_scoped_authority() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/assistant/check-shared.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python shared-workspace MCP fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn list_registry_example_files_discover_defined_views_and_restore_them() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/list-registries/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python registry fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn actual_live_monitor_runs_through_cooperative_mcp_connections() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/live-service-monitor/check-mcp.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("cooperative monitor fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn request_identities_survive_real_mcp_and_application_restart() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/agent-requests/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("request identity fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn command_feedback_preserves_causes_across_cli_ui_and_mcp() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/command-feedback/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("synthetic command feedback fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
