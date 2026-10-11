//! Bounded capture for owned test commands whose output pipes are not inherited by descendants.
use std::{
    io::{self, Read},
    process::{Command, Output, Stdio},
    thread,
    time::{Duration, Instant},
};

pub struct Capture {
    pub pid: u32,
    pub output: Output,
    pub timed_out: bool,
    pub stdout_omitted: u64,
    pub stderr_omitted: u64,
}

fn drain(mut input: impl Read, limit: usize) -> io::Result<(Vec<u8>, u64)> {
    let mut bytes = Vec::with_capacity(limit);
    let mut omitted = 0u64;
    let mut buffer = [0; 4096];
    loop {
        let count = input.read(&mut buffer)?;
        if count == 0 {
            return Ok((bytes, omitted));
        }
        let keep = count.min(limit - bytes.len());
        bytes.extend_from_slice(&buffer[..keep]);
        omitted = omitted.saturating_add((count - keep) as u64);
    }
}

pub fn capture(command: &mut Command, budget: Duration, limit: usize) -> io::Result<Capture> {
    assert!(
        limit <= 1024 * 1024,
        "test output capture must remain bounded"
    );
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let pid = child.id();
    let stdout = child.stdout.take().expect("piped stdout");
    let stderr = child.stderr.take().expect("piped stderr");
    // Continue draining after the retained prefix fills, so neither channel can stall the child.
    let out = thread::spawn(move || drain(stdout, limit));
    let err = thread::spawn(move || drain(stderr, limit));
    let deadline = Instant::now() + budget;
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Ok(status),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(error);
            }
            Ok(None) if Instant::now() >= deadline => {
                timed_out = true;
                let _ = child.kill(); // It may have exited between try_wait and kill.
                break child.wait();
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
        }
    };
    // Join both readers even when waiting failed. No timed-out sampler or reader is detached.
    let stdout = out.join();
    let stderr = err.join();
    let reader_failed = || io::Error::other("test output reader terminated unexpectedly");
    let (stdout, stdout_omitted) = stdout.map_err(|_| reader_failed())??;
    let (stderr, stderr_omitted) = stderr.map_err(|_| reader_failed())??;
    Ok(Capture {
        pid,
        output: Output {
            status: status?,
            stdout,
            stderr,
        },
        timed_out,
        stdout_omitted,
        stderr_omitted,
    })
}
