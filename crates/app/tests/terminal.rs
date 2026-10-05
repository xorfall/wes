//! Runs the documented example files through real process and PTY boundaries.
#[test]
#[cfg(unix)]
fn ssh_recipes_and_terminals_preserve_explicit_target_admission() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    for terminal in [false, true] {
        let mut command = std::process::Command::new("python3");
        command
            .arg(root.join("examples/ssh-execution/check.py"))
            .args(["--binary", env!("CARGO_BIN_EXE_wes")]);
        if terminal {
            command.arg("--terminal");
        }
        let output = command
            .output()
            .expect("Python synthetic SSH client fixture");
        assert!(
            output.status.success(),
            "terminal={terminal}: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
#[cfg(unix)]
fn docker_terminals_and_compose_targets_share_captured_identity_and_lifetime() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/docker-execution/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python synthetic Docker Engine fixture");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
#[cfg(unix)]
fn desktop_terminal_discovers_local_commands_without_user_startup_files() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check-path.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 for isolated desktop PATH acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn prepared_workspace_is_available_to_agent_child_processes() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 for synthetic terminal acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn terminal_output_wakes_and_drains_without_a_periodic_ui_delay() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check-performance.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 for synthetic terminal performance acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
#[cfg(unix)]
fn desktop_terminal_accepts_turkish_input_without_startup_artifacts() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check-unicode.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 for synthetic terminal Unicode acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn terminal_prompt_tracks_git_without_evaluating_repository_text() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check-prompt.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 and Git for isolated prompt acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn terminal_command_history_survives_reopen_until_pane_forget() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/terminal/check-history.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python 3 and native shells for isolated history acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
#[cfg(unix)]
fn retained_workspace_streams_do_not_delay_paced_terminal_keys() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("node")
        .arg(root.join("examples/terminal/check-input-contention.mjs"))
        .arg(env!("CARGO_BIN_EXE_wes"))
        .output()
        .expect("Node 22 and GUI dependencies for isolated input contention acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    println!("{}", String::from_utf8_lossy(&output.stdout));
}

#[test]
#[cfg(unix)]
fn workspace_deletion_joins_terminal_and_preserves_independent_pane_history() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let output = std::process::Command::new("python3")
        .arg(root.join("examples/workspace-deletion/check.py"))
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .expect("Python synthetic workspace deletion acceptance");
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
