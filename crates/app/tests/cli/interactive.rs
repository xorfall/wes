use super::*;
use std::{path::Path, process::Stdio, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

async fn run(home: &Path, source: &str) -> std::process::Output {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_wes"))
        .args(["--home", home.to_str().unwrap(), "--command", source])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut prefix = Vec::new();
    tokio::time::timeout(Duration::from_secs(5), stdout.read_until(b':', &mut prefix))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(prefix, b"question:");
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(b"synthetic-private-answer\n")
        .await
        .unwrap();
    drop(stdin);
    let (mut result, tail) = tokio::time::timeout(Duration::from_secs(8), async {
        tokio::join!(child.wait_with_output(), async {
            let mut tail = Vec::new();
            stdout.read_to_end(&mut tail).await.unwrap();
            tail
        })
    })
    .await
    .unwrap();
    let result = result.as_mut().unwrap();
    prefix.extend(tail);
    result.stdout = prefix;
    result.clone()
}
fn no_private_bytes(path: &Path) {
    let needle = b"synthetic-private-answer";
    for entry in std::fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            no_private_bytes(&path);
        } else {
            assert!(
                !std::fs::read(&path)
                    .unwrap()
                    .windows(needle.len())
                    .any(|part| part == needle)
            );
        }
    }
}
fn assert_result(home: &Path, result: std::process::Output) {
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let text = String::from_utf8(result.stdout).unwrap();
    assert!(text.contains("synthetic-private-answer"));
    assert!(text.contains("\"exitCode\":7"));
    assert!(text.contains("\"stdout\":\"\""));
    assert!(text.contains("\"stderr\":\"\""));
    no_private_bytes(home);
    let reopened = Command::new(env!("CARGO_BIN_EXE_wes"))
        .args([
            "--home",
            home.to_str().unwrap(),
            "--command",
            ":workspace save \"native-copy\"",
        ])
        .output()
        .unwrap();
    assert!(
        reopened.status.success(),
        "{}",
        String::from_utf8_lossy(&reopened.stderr)
    );
    assert!(!String::from_utf8_lossy(&reopened.stdout).contains("question:"));
    no_private_bytes(home);
}

#[tokio::test]
async fn native_interactive_shell_uses_client_descriptors_and_never_captures_the_answer() {
    let root = tempfile::tempdir().unwrap();
    let source = r#"@interactive sh run cmd:"printf 'question:'; read answer; printf '%s' \"$answer\"; exit 7" > dialogue"#;
    let result = run(root.path(), source).await;
    assert_result(root.path(), result);
}

#[tokio::test]
async fn native_imported_process_uses_same_handover_and_reopens_without_the_executable() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let script = root.path().join("ask.sh");
    std::fs::write(
        &script,
        b"#!/bin/sh\nprintf 'question:'\nread answer\nprintf '%s' \"$answer\"\nexit 7\n",
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
    let home = root.path().join("home");
    let source = format!(
        ":import process bin:\"{}\" as:dialog\n@interactive dialog run > dialogue",
        script.display()
    );
    let result = run(&home, &source).await;
    std::fs::remove_file(script).unwrap();
    assert_result(&home, result);
}
