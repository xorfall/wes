//! The host's finite shell provider, and commands written in that shell's own language.
//!
//! The provider is `sh` on Unix and `cmd` on Windows. Neither name is an alias of the other
//! and no command is translated: a test asks for what it needs and gets source for the shell
//! this host really has.
use std::{
    path::Path,
    time::{Duration, Instant},
};

#[allow(dead_code)]
pub const NAME: &str = if cfg!(windows) { "cmd" } else { "sh" };

fn run(command: &str) -> String {
    format!("{NAME} run cmd:{}", wes_language::quote_text(command))
}

/// Source that writes exactly `text` to standard output, with no line ending. The text is a
/// plain word: letters, digits and hyphens.
#[allow(dead_code)]
pub fn print(text: &str) -> String {
    assert!(
        text.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
        "{text:?}"
    );
    if cfg!(windows) {
        // `set /p` with no input reports failure; the command as a whole succeeds.
        run(&format!("(<nul set /p={text}) & cd ."))
    } else {
        run(&format!("printf {text}"))
    }
}

/// Source for a command that creates `marker` once it is running and then stays running for
/// about `seconds`. The marker's path has no quote and no shell-special character.
#[allow(dead_code)]
pub fn hold(marker: &Path, seconds: u32) -> String {
    let marker = marker.to_str().unwrap();
    assert!(
        !marker.contains(['"', '\'', '%', '&', '^', '$', '`']),
        "{marker:?}"
    );
    if cfg!(windows) {
        // One echo request a second; the first is answered at once.
        run(&format!(
            "type nul > \"{marker}\" & ping -n {} 127.0.0.1 > nul",
            seconds + 1
        ))
    } else {
        // The shell becomes the sleep, so ending the command ends it too.
        run(&format!(": > '{marker}'; exec sleep {seconds}"))
    }
}

/// Waits until a held command has created its marker: the command is then running.
#[allow(dead_code)]
pub fn started(marker: &Path) {
    let deadline = Instant::now() + Duration::from_secs(60);
    while !marker.exists() {
        assert!(Instant::now() < deadline, "the command did not start");
        std::thread::sleep(Duration::from_millis(20));
    }
}
