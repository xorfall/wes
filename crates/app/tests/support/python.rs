//! The Python interpreter the example runners need, found once for a test binary.
//!
//! Unix systems name it `python3`. A Windows installation provides `python` and the `py`
//! launcher, and may leave a `python3` on the search path that only offers to install one.
//! Each candidate is therefore started and asked what it is; the interpreter it reports is
//! the one used, by its own absolute path. Nothing is downloaded, aliased or added to the
//! search path, and a host without a usable interpreter fails the test with what to do.
use std::{
    io::Read,
    path::PathBuf,
    process::{Command, Stdio},
    sync::OnceLock,
    time::{Duration, Instant},
};

const REQUIRED: (u64, u64) = (3, 11);
/// A candidate that has not answered by then is not an interpreter worth waiting for.
const ANSWER: Duration = Duration::from_secs(20);
#[cfg(not(windows))]
const CANDIDATES: &[&[&str]] = &[&["python3"], &["python"]];
#[cfg(windows)]
const CANDIDATES: &[&[&str]] = &[&["python"], &["py", "-3"], &["python3"]];
/// The answer is JSON with non-ASCII characters escaped, so it reads the same whatever
/// encoding the interpreter gives its output on this host.
const QUESTION: &str = "import json, sys; print(json.dumps({'executable': sys.executable, 'version': list(sys.version_info[:2])}))";

/// The interpreter a candidate starts, when it answers in time and is new enough.
///
/// The deadline covers the whole exchange: the candidate's exit and the end of its output.
/// A candidate still running then is ended and waited for. Output that is still open then,
/// because something the candidate started holds it, is abandoned: its reader ends with
/// that process and is not waited for.
#[allow(dead_code)]
pub fn identify(
    candidate: &[&str],
    environment: &[(&str, &str)],
    deadline: Duration,
) -> Option<PathBuf> {
    let mut child = Command::new(candidate[0])
        .args(&candidate[1..])
        .args(["-c", QUESTION])
        .envs(environment.iter().copied())
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let output = child.stdout.take();
    let (sender, received) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut answer = Vec::new();
        // The answer is one short line; anything longer is not an answer.
        let read = output.map(|output| output.take(64 * 1024).read_to_end(&mut answer));
        let _ = sender.send(matches!(read, Some(Ok(_))).then_some(answer));
    });
    let started = Instant::now();
    let (mut status, mut answer) = (None, None);
    let complete = loop {
        if status.is_none() {
            match child.try_wait() {
                Ok(exited) => status = exited,
                Err(_) => break false,
            }
        }
        if answer.is_none() {
            match received.try_recv() {
                Ok(Some(bytes)) => answer = Some(bytes),
                Ok(None) | Err(std::sync::mpsc::TryRecvError::Disconnected) => break false,
                Err(std::sync::mpsc::TryRecvError::Empty) => {}
            }
        }
        if status.is_some() && answer.is_some() {
            break true;
        }
        if started.elapsed() >= deadline {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if !complete {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let answer: serde_json::Value = serde_json::from_slice(&answer?).ok()?;
    let version = (
        answer["version"][0].as_u64()?,
        answer["version"][1].as_u64()?,
    );
    let executable = PathBuf::from(answer["executable"].as_str()?);
    (status?.success() && version >= REQUIRED && executable.is_absolute() && executable.is_file())
        .then_some(executable)
}

/// A command for the verified interpreter; arguments are added by the caller as usual.
#[allow(dead_code)]
pub fn command() -> Command {
    static INTERPRETER: OnceLock<PathBuf> = OnceLock::new();
    Command::new(INTERPRETER.get_or_init(|| {
        CANDIDATES
            .iter()
            .find_map(|candidate| identify(candidate, &[], ANSWER))
            .unwrap_or_else(|| {
                let tried: Vec<_> = CANDIDATES.iter().map(|c| c.join(" ")).collect();
                panic!(
                    "Python {}.{} or newer is required to run the example checks, and none of [{}] started one. Install it and make one of these commands available; nothing is installed or skipped for you.",
                    REQUIRED.0,
                    REQUIRED.1,
                    tried.join(", ")
                )
            })
    }))
}
