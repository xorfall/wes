//! The shared interpreter discovery itself, against real interpreters and synthetic answers.
#[path = "support/python.rs"]
mod python;
use std::time::{Duration, Instant};

const ANSWER: Duration = Duration::from_secs(60);

/// The interpreter the suite uses, as a candidate that runs the given program first: the
/// discovery's own question follows it on the command line and is ignored by Python.
fn answering(program: &str) -> Option<std::path::PathBuf> {
    let interpreter = python::command().get_program().to_owned();
    python::identify(&[interpreter.to_str().unwrap(), "-c", program], &[], ANSWER)
}

#[test]
fn an_interpreter_under_a_non_ascii_path_is_found_whatever_its_output_encoding() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("Ünïcode yol ğ");
    let created = python::command()
        .args(["-m", "venv", "--without-pip"])
        .arg(&home)
        .output()
        .unwrap();
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let interpreter = if cfg!(windows) {
        home.join("Scripts").join("python.exe")
    } else {
        home.join("bin").join("python")
    };
    let candidate = [interpreter.to_str().unwrap()];
    // A legacy code page for the interpreter's output, a UTF-8 one, and the host's default.
    for environment in [
        &[("PYTHONIOENCODING", "cp1254"), ("PYTHONUTF8", "0")][..],
        &[("PYTHONIOENCODING", "utf-8")],
        &[],
    ] {
        let found = python::identify(&candidate, environment, ANSWER)
            .unwrap_or_else(|| panic!("not found with {environment:?}"));
        assert!(found.is_absolute() && found.is_file(), "{found:?}");
        assert!(
            found.to_string_lossy().contains("Ünïcode yol ğ"),
            "{found:?} with {environment:?}"
        );
    }
}

#[test]
fn a_candidate_that_never_answers_is_ended_at_the_deadline_and_is_not_an_interpreter() {
    let interpreter = python::command().get_program().to_owned();
    let started = Instant::now();
    let found = python::identify(
        &[
            interpreter.to_str().unwrap(),
            "-c",
            "import time; time.sleep(600)",
        ],
        &[],
        Duration::from_millis(500),
    );
    assert_eq!(found, None);
    // Ended and waited for well before the candidate would have finished on its own.
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "{:?}",
        started.elapsed()
    );
}

/// A candidate that answers and exits, but leaves a process holding its output open: the
/// answer is not complete by the deadline, and the probe does not wait for that process.
#[test]
fn an_answer_whose_output_stays_open_past_the_deadline_is_not_waited_for() {
    let interpreter = python::command().get_program().to_owned();
    let real = serde_json::to_string(interpreter.to_str().unwrap()).unwrap();
    let program = format!(
        "import subprocess, sys; print(r'{{\"executable\": {real}, \"version\": [3, 13]}}'); sys.stdout.flush(); subprocess.Popen([sys.executable, '-c', 'import time; time.sleep(8)'], stdout=sys.stdout)"
    );
    let candidate = [interpreter.to_str().unwrap(), "-c", program.as_str()];
    let started = Instant::now();
    assert_eq!(
        python::identify(&candidate, &[], Duration::from_millis(500)),
        None
    );
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    // The same answer is accepted once its output has ended within the deadline.
    assert!(python::identify(&candidate, &[], ANSWER).is_some());
}

#[test]
fn only_a_complete_answer_from_a_new_enough_existing_interpreter_is_accepted() {
    let real = python::command().get_program().to_owned();
    let real = serde_json::to_string(real.to_str().unwrap()).unwrap();
    // The same interpreter, answering as itself.
    assert!(
        answering(&format!(
            "print(r'{{\"executable\": {real}, \"version\": [3, 11]}}')"
        ))
        .is_some()
    );
    for (why, program) in [
        (
            "too old",
            format!("print(r'{{\"executable\": {real}, \"version\": [3, 10]}}')"),
        ),
        (
            "another major",
            format!("print(r'{{\"executable\": {real}, \"version\": [2, 7]}}')"),
        ),
        (
            "failed",
            format!(
                "print(r'{{\"executable\": {real}, \"version\": [3, 13]}}'); raise SystemExit(3)"
            ),
        ),
        (
            "no such file",
            "print('{\"executable\": \"/no/such/python\", \"version\": [3, 13]}')".to_owned(),
        ),
        (
            "relative",
            "print('{\"executable\": \"python\", \"version\": [3, 13]}')".to_owned(),
        ),
        (
            "not an answer",
            "print('Python was not found; run without arguments to install')".to_owned(),
        ),
        ("nothing", "pass".to_owned()),
        ("incomplete", "print('{\"executable\": ')".to_owned()),
    ] {
        assert_eq!(answering(&program), None, "{why}");
    }
    assert_eq!(
        python::identify(&["wes-no-such-interpreter"], &[], ANSWER),
        None
    );
}
