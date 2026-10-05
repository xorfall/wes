use std::{
    io::Read,
    process::{Command, Output, Stdio},
    time::{Duration, Instant},
};

fn run_cli(command: &mut Command, source: &str, budget: Duration) -> Output {
    let mut child = command
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    eprintln!("CLI diagnostic phase: {source:?}; child pid {}", child.id());
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let out = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stdout.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let err = std::thread::spawn(move || {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).unwrap();
        bytes
    });
    let deadline = Instant::now() + budget;
    let mut timed_out = false;
    let status = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            timed_out = true;
            #[cfg(target_os = "macos")]
            {
                // The sampler targets only this owned synthetic CLI child.
                let sample = Command::new("/usr/bin/sample")
                    .args([
                        child.id().to_string(),
                        "1".into(),
                        "1".into(),
                        "-file".into(),
                        "/dev/stdout".into(),
                    ])
                    .output();
                if let Ok(sample) = sample {
                    eprintln!(
                        "Timed-out CLI stack:\n{}",
                        String::from_utf8_lossy(&sample.stdout)
                    );
                    eprintln!("Sampler: {}", String::from_utf8_lossy(&sample.stderr));
                }
            }
            let _ = child.kill(); // The child may have exited while being sampled.
            break child.wait().unwrap();
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    let output = Output {
        status,
        stdout: out.join().unwrap(),
        stderr: err.join().unwrap(),
    };
    assert!(
        !timed_out,
        "CLI phase {source:?} did not finish in {budget:?}; stdout: {}; stderr: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}
#[test]
fn cli_explains_contract_failures_names_and_backend_control_receipts() {
    let home = tempfile::tempdir().unwrap();
    let run = |source: &str| {
        run_cli(
            Command::new(env!("CARGO_BIN_EXE_wes"))
                .arg("--home")
                .arg(home.path())
                .args(["--command", source]),
            source,
            Duration::from_secs(30),
        )
    };
    let failure = run(
        ":calc { return iter.checked(iter.items(['invalid']), 'Int').collect(); } > health *> healthError",
    );
    let stderr = String::from_utf8_lossy(&failure.stderr);
    assert!(!failure.status.success());
    assert!(stderr.contains("CAL017"), "{stderr}");
    assert!(stderr.contains("TYP"), "{stderr}");
    let waiting = run(
        ":calc { return 1; } > readiness *> readinessError\n:calc { return $readinessError; } > unavailable",
    );
    let note = String::from_utf8_lossy(&waiting.stderr);
    assert!(
        note.contains("error output was not produced by $readiness"),
        "{note}"
    );
    let help = run(":help errors");
    assert!(
        help.status.success(),
        "{}",
        String::from_utf8_lossy(&help.stderr)
    );
    assert!(String::from_utf8_lossy(&help.stdout).contains("CAL015"));
    assert!(run(":calc { return 1; } > producer").status.success());
    let refresh = run(":refresh $producer");
    assert!(
        refresh.status.success(),
        "{}",
        String::from_utf8_lossy(&refresh.stderr)
    );
    assert!(String::from_utf8_lossy(&refresh.stdout).contains("refresh $producer"));
}

#[cfg(unix)]
#[test]
#[should_panic(expected = "did not finish")]
fn blocked_cli_is_sampled_reaped_and_reported_instead_of_hanging_the_suite() {
    run_cli(
        Command::new("/bin/sleep").arg("60"),
        "synthetic blocked child",
        Duration::from_millis(50),
    );
}
