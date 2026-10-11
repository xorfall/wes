#[cfg(unix)]
#[path = "support/command_output.rs"]
mod command_output;

#[cfg(unix)]
use std::{process::Command, time::Duration};

#[cfg(unix)]
#[test]
fn successful_command_retains_both_channels_without_a_false_timeout() {
    let result = command_output::capture(
        Command::new("/bin/sh").args(["-c", "printf 'out'; printf 'err' >&2"]),
        Duration::from_secs(2),
        4096,
    )
    .unwrap();
    assert!(result.output.status.success());
    assert!(!result.timed_out);
    assert_eq!(result.output.stdout, b"out");
    assert_eq!(result.output.stderr, b"err");
    assert_eq!((result.stdout_omitted, result.stderr_omitted), (0, 0));
}

#[cfg(unix)]
#[test]
fn a_stalled_sampler_is_killed_and_reaped_before_capture_returns() {
    let result = command_output::capture(
        Command::new("/bin/sleep").arg("60"),
        Duration::from_millis(50),
        4096,
    )
    .unwrap();
    assert!(result.timed_out);
    assert!(!result.output.status.success());
    assert!(result.output.stdout.is_empty() && result.output.stderr.is_empty());
    // Only this test's reaped PID is inspected; no user process is targeted.
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &result.pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .unwrap()
            .success()
    );
}

#[cfg(unix)]
#[test]
fn both_full_channels_are_drained_after_the_retained_prefix_limit() {
    let result = command_output::capture(
        Command::new("/bin/sh").args([
            "-c",
            "i=0; while [ $i -lt 100 ]; do printf '%04096d' 0; printf '%04096d' 0 >&2; i=$((i+1)); done",
        ]),
        Duration::from_secs(2),
        4096,
    )
    .unwrap();
    assert!(result.output.status.success());
    assert!(!result.timed_out);
    assert_eq!(result.output.stdout.len(), 4096);
    assert_eq!(result.output.stderr.len(), 4096);
    assert!(result.stdout_omitted > 0 && result.stderr_omitted > 0);
}
