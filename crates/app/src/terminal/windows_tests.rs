use super::*;
use crate::runtime::{RuntimeOptions, launch};

// A failed assertion must not leave blocking console workers alive during runtime teardown.
struct Cleanup(Manager);
impl Drop for Cleanup {
    fn drop(&mut self) {
        for terminal in self.0.sessions.lock().unwrap().values() {
            terminal.stopped.cancel();
        }
    }
}

fn shown(terminal: &TerminalSession) -> String {
    let bytes: Vec<u8> = terminal
        .output
        .lock()
        .unwrap()
        .bytes
        .iter()
        .copied()
        .collect();
    visible(&bytes)
}
/// What an emulator would show: control sequences removed, text kept.
fn visible(bytes: &[u8]) -> String {
    let source = String::from_utf8_lossy(bytes);
    let mut text = String::new();
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            text.push(character);
            continue;
        }
        match characters.next() {
            Some('[') => {
                for parameter in characters.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&parameter) {
                        break;
                    }
                }
            }
            Some(']') => {
                while let Some(parameter) = characters.next() {
                    if parameter == '\u{7}'
                        || (parameter == '\u{1b}' && characters.next_if_eq(&'\\').is_some())
                    {
                        break;
                    }
                }
            }
            _ => {}
        }
    }
    text
}
async fn wait_for(terminal: &TerminalSession, wanted: &str) -> String {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let text = shown(terminal);
            if text.contains(wanted) {
                return text;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no {wanted:?} in {:?}", shown(terminal)))
}

#[tokio::test(flavor = "multi_thread")]
async fn a_local_pane_is_the_systems_powershell_with_an_owned_prompt_and_utf8() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("çalışma alanı");
    std::fs::create_dir(&directory).unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let current = runtime.handle.current().unwrap();
    let client = "synthetic-windows-client";
    let config = Config {
        idle_timeout: None,
        cwd: directory,
        ..crate::runtime::browser_services(root.path().join("home"), root.path().into())
            .unwrap()
            .terminal
            .unwrap()
    };
    let id = manager
        .start(
            config,
            current.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let terminal = manager.owned(&id, &current.generation, client).unwrap();
    let write = |text: &str| {
        manager
            .write(&id, &current.generation, client, text.into())
            .unwrap()
    };

    // The owned prompt: selected environment, the spaced non-ASCII directory, then the marker.
    let prompt = wait_for(&terminal, "çalışma alanı > ").await;
    assert!(prompt.contains("@default "), "{prompt:?}");

    write("'ÇIKTI_' + 'ğüşiöç'\r");
    wait_for(&terminal, "ÇIKTI_ğüşiöç").await;

    // Neither the process policy nor the bootstrap's own variables are left behind.
    write(
        "'POLICY_' + (Get-ExecutionPolicy -Scope Process) + '_' + [string]$env:WES_BOOTSTRAP + '_END'\r",
    );
    wait_for(&terminal, "POLICY_Undefined__END").await;
    write("'CODEPAGE_' + [Console]::OutputEncoding.CodePage\r");
    wait_for(&terminal, "CODEPAGE_65001").await;
    // A secret of the hosting process is not part of the pane's environment.
    write("'HOST_' + [string]$env:CARGO_MANIFEST_DIR + '_END'\r");
    wait_for(&terminal, "HOST__END").await;

    manager
        .resize(&id, &current.generation, client, 100, 40)
        .unwrap();
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            write("'COLS_' + $Host.UI.RawUI.WindowSize.Width\r");
            tokio::time::sleep(Duration::from_millis(300)).await;
            if shown(&terminal).contains("COLS_100") {
                break;
            }
        }
    })
    .await
    .unwrap();

    write("exit 5\r");
    tokio::time::timeout(Duration::from_secs(30), terminal.finished.cancelled())
        .await
        .unwrap();
    let frame = manager.poll(&id, &current.generation, client, 0).unwrap();
    assert_eq!(frame.exit, Some(5));
    assert!(frame.closed && frame.problem.is_none());
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_a_pane_ends_a_running_command_without_a_reported_problem() {
    let root = tempfile::tempdir().unwrap();
    let runtime = launch(RuntimeOptions::new(
        root.path().join("home"),
        root.path().into(),
    ))
    .await
    .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let current = runtime.handle.current().unwrap();
    let client = "synthetic-windows-client";
    let config = Config {
        idle_timeout: None,
        ..crate::runtime::browser_services(root.path().join("home"), root.path().into())
            .unwrap()
            .terminal
            .unwrap()
    };
    let id = manager
        .start(
            config,
            current.clone(),
            client.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let terminal = manager.owned(&id, &current.generation, client).unwrap();
    wait_for(&terminal, " > ").await;
    manager
        .write(
            &id,
            &current.generation,
            client,
            "'RUN' + 'NING'; Start-Sleep 600\r".into(),
        )
        .unwrap();
    wait_for(&terminal, "RUNNING").await;
    let problem = tokio::time::timeout(
        Duration::from_secs(15),
        manager.close_joined(&id, &current.generation, client),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(problem, None);
    assert!(manager.owned(&id, &current.generation, client).is_err());
    assert!(!manager.has_active_sessions());
    manager.shutdown().await;
}

const CLIENT: &str = "synthetic-windows-client";
async fn started(
    root: &std::path::Path,
) -> (
    crate::runtime::LaunchedRuntime,
    Manager,
    CurrentSession,
    String,
    Arc<TerminalSession>,
) {
    let runtime = launch(RuntimeOptions::new(root.join("home"), root.into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let current = runtime.handle.current().unwrap();
    let config = Config {
        idle_timeout: None,
        ..crate::runtime::browser_services(root.join("home"), root.into())
            .unwrap()
            .terminal
            .unwrap()
    };
    let id = manager
        .start(
            config,
            current.clone(),
            CLIENT.into(),
            0,
            runtime.handle.clone(),
        )
        .await
        .unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    (runtime, manager, current, id, terminal)
}
async fn wait_after(terminal: &TerminalSession, from: usize, wanted: &str) {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !shown(terminal)
            .split_at_checked(from)
            .is_some_and(|(_, later)| later.contains(wanted))
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap_or_else(|_| panic!("no later {wanted:?} in {:?}", shown(terminal)))
}

#[tokio::test(flavor = "multi_thread")]
async fn an_interrupt_stops_a_long_command_and_the_pane_keeps_working() {
    let root = tempfile::tempdir().unwrap();
    let (_runtime, manager, current, id, terminal) = started(root.path()).await;
    let _cleanup = Cleanup(manager.clone());
    let write = |text: &str| {
        manager
            .write(&id, &current.generation, CLIENT, text.into())
            .unwrap()
    };
    write("Start-Sleep 120; 'SLEPT_' + 'FULLY'\r");
    let began = std::time::Instant::now();
    wait_for(&terminal, "'FULLY'").await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let interrupted = shown(&terminal).len();
    write("\u{3}");
    // An interrupt discards pending input, so the next line waits for the returned prompt.
    wait_after(&terminal, interrupted, " > ").await;
    write("'AFTER_' + (1 + 1)\r");
    let text = wait_for(&terminal, "AFTER_2").await;
    assert!(began.elapsed() < Duration::from_secs(60));
    assert!(!text.contains("SLEPT_FULLY"), "{text:?}");
    assert!(!terminal.stopped.is_cancelled());
    manager.shutdown().await;
}

/// Windows hands "Ctrl+C is disabled" from a process to everything it starts. A host launched
/// that way must still give its panes a working interrupt.
#[test]
fn the_interrupt_works_in_a_host_that_was_started_with_ctrl_c_disabled() {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x200;
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "terminal::windows_tests::an_interrupt_stops_a_long_command_and_the_pane_keeps_working",
        ])
        .creation_flags(CREATE_NEW_PROCESS_GROUP)
        .output()
        .unwrap();
    let report = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && report.contains("1 passed"),
        "{report}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_line_editor_edits_with_arrow_keys_and_recalls_the_panes_own_commands() {
    let root = tempfile::tempdir().unwrap();
    let (_runtime, manager, current, id, terminal) = started(root.path()).await;
    let _cleanup = Cleanup(manager.clone());
    let write = |text: &str| {
        manager
            .write(&id, &current.generation, CLIENT, text.into())
            .unwrap()
    };
    write(
        "'EDITOR_' + [bool](Get-Module PSReadLine) + '_' + (Get-PSReadLineOption).HistorySaveStyle\r",
    );
    wait_for(&terminal, "EDITOR_True_SaveNothing").await;
    // Two cursor-left keys put the missing letter inside the quoted text.
    write("'EDIT_' + 'AC'\u{1b}[D\u{1b}[DB\r");
    wait_for(&terminal, "EDIT_ABC").await;
    write("'RECALL_' + (20 + 3)\r");
    wait_for(&terminal, "RECALL_23").await;
    let recalled = shown(&terminal).len();
    // Cursor-up recalls the previous line from this pane's in-memory history.
    write("\u{1b}[A\r");
    wait_after(&terminal, recalled, "RECALL_23").await;
    manager.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panes_history_survives_its_shell_and_is_recalled_by_the_next_one() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let current = runtime.handle.current().unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let config = Config {
        idle_timeout: None,
        history: Some(key.clone()),
        ..crate::runtime::browser_services(home.clone(), root.path().into())
            .unwrap()
            .terminal
            .unwrap()
    };
    let start = || {
        manager.start(
            config.clone(),
            current.clone(),
            CLIENT.into(),
            0,
            runtime.handle.clone(),
        )
    };
    let file = home.join("terminal-history").join(&key).join("powershell");

    let id = start().await.unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    for line in ["'İLK_' + 'ğüşiöç'\r", "'SON_' + (40 + 2)\r"] {
        manager
            .write(&id, &current.generation, CLIENT, line.into())
            .unwrap();
    }
    wait_for(&terminal, "SON_42").await;
    // Each command is recorded when the prompt returns, before the pane is closed.
    tokio::time::timeout(Duration::from_secs(30), async {
        while std::fs::read(&file).is_ok_and(|bytes| !bytes.ends_with(b"(40 + 2)\0")) {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        String::from_utf8(std::fs::read(&file).unwrap()).unwrap(),
        "'İLK_' + 'ğüşiöç'\0'SON_' + (40 + 2)\0"
    );
    assert!(!file.with_extension("next").exists());
    // The same pane is one writer at a time.
    assert!(start().await.is_err());
    assert_eq!(
        manager
            .close_joined(&id, &current.generation, CLIENT)
            .await
            .unwrap(),
        None
    );

    // A new shell for the same pane: nothing was typed here, yet cursor-up finds the command.
    let id = start().await.unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    assert!(!shown(&terminal).contains("SON_42"));
    manager
        .write(&id, &current.generation, CLIENT, "\u{1b}[A\r".into())
        .unwrap();
    wait_for(&terminal, "SON_42").await;
    manager
        .close_joined(&id, &current.generation, CLIENT)
        .await
        .unwrap();

    // Forgetting the pane removes its history and refuses to start it again.
    manager.forget(&home, &key, CLIENT).await.unwrap();
    assert!(!file.parent().unwrap().exists());
    assert!(start().await.is_err());
    manager.shutdown().await;
}

/// A pane's shell opened without the manager, so its history budget can be the smallest the
/// catalogue supports instead of the process-wide default.
struct Shell {
    endpoint: Box<dyn TerminalIo>,
    seen: Vec<u8>,
    _directory: tempfile::TempDir,
}
impl Shell {
    fn open(history: &std::path::Path, bytes: u64) -> Self {
        Self::open_with(history, bytes, None)
    }
    /// With `modules`, the shell searches only that directory for modules. An empty one is a
    /// host whose line editor is not available.
    fn open_with(history: &std::path::Path, bytes: u64, modules: Option<&std::path::Path>) -> Self {
        let directory = tempfile::tempdir().unwrap();
        let mut command = powershell::launch(directory.path()).unwrap();
        for (key, value) in powershell::environment(std::env::vars_os()) {
            command.env(key, value);
        }
        command.env("WES_HISTORY_FILE", history);
        command.env("WES_HISTORY_BYTES", bytes.to_string());
        if let Some(modules) = modules {
            command.env("PSModulePath", modules);
        }
        let endpoint = TerminalPlan::workspace()
            .open(
                Some(command),
                TerminalSize {
                    cols: 120,
                    rows: 30,
                },
                &CancellationToken::new(),
            )
            .unwrap();
        let mut shell = Self {
            endpoint,
            seen: vec![],
            _directory: directory,
        };
        shell.wait(" > ");
        shell
    }
    fn wait(&mut self, wanted: &str) {
        let deadline = std::time::Instant::now() + Duration::from_secs(30);
        let mut buffer = [0u8; 16384];
        loop {
            loop {
                match self.endpoint.read(&mut buffer) {
                    Ok(0) => break,
                    Ok(n) => self.seen.extend_from_slice(&buffer[..n]),
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Err(error) => panic!("terminal read: {error}"),
                }
            }
            if visible(&self.seen).contains(wanted) {
                return;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "no {wanted:?} in {:?}",
                visible(&self.seen)
            );
            self.endpoint
                .wait_ready(false, Duration::from_millis(10))
                .unwrap();
        }
    }
    fn send(&mut self, text: &str) {
        let mut bytes = text.as_bytes();
        while !bytes.is_empty() {
            match self.endpoint.write(bytes) {
                Ok(n) => bytes = &bytes[n..],
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    std::thread::sleep(Duration::from_millis(5))
                }
                Err(error) => panic!("terminal write: {error}"),
            }
        }
    }
}
fn recorded(file: &std::path::Path, expected: &[u8]) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::fs::read(file).unwrap() != expected {
        assert!(
            std::time::Instant::now() < deadline,
            "{:?}",
            String::from_utf8_lossy(&std::fs::read(file).unwrap())
        );
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn a_command_over_the_history_budget_leaves_an_empty_history_the_pane_reopens() {
    let home = tempfile::tempdir().unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let file = history::prepare(home.path(), &key, "powershell").unwrap();
    std::fs::write(&file, "'earlier'\0").unwrap();
    // The smallest supported budget; one accepted command is larger than all of it.
    let budget = wes_budgets::catalogue()["terminal.history.bytes"].min;
    let mut shell = Shell::open(&file, budget);
    shell.send(&format!(
        "'LONG_' + '{}'.Length\r",
        "x".repeat(budget as usize + 200)
    ));
    shell.wait(&format!("LONG_{}", budget + 200));
    // Nothing fits, so nothing is kept: an empty file, not a separator without a record.
    recorded(&file, b"");
    shell.endpoint.shutdown().unwrap();
    drop(shell);

    // The same pane opens again: its history is valid, empty, and records what follows.
    history::prepare(home.path(), &key, "powershell").unwrap();
    let mut shell = Shell::open(&file, budget);
    shell.send("'AGAIN_' + (1 + 1)\r");
    shell.wait("AGAIN_2");
    recorded(&file, b"'AGAIN_' + (1 + 1)\0");
    shell.endpoint.shutdown().unwrap();
    history::prepare(home.path(), &key, "powershell").unwrap();
}

/// A host without the line editor: a command is still recorded when it starts, so closing the
/// pane during it keeps it; a line that starts no command is recorded when it returns.
#[test]
fn without_the_line_editor_a_running_command_is_recorded_before_its_pane_closes() {
    let home = tempfile::tempdir().unwrap();
    let modules = tempfile::tempdir().unwrap();
    let key = uuid::Uuid::new_v4().to_string();
    let file = history::prepare(home.path(), &key, "powershell").unwrap();
    let budget = wes_budgets::catalogue()["terminal.history.bytes"].default;
    let mut shell = Shell::open_with(&file, budget, Some(modules.path()));
    shell.send("'EDITOR_' + [bool](Get-Module PSReadLine)\r");
    shell.wait("EDITOR_False");
    recorded(&file, b"'EDITOR_' + [bool](Get-Module PSReadLine)\0");
    // A bare expression starts no command; it is recorded once, when its prompt returns.
    shell.send("'İFADE_' + (3 + 4)\r");
    shell.wait("İFADE_7");
    // The identical line entered twice is two records, and each is one record however many
    // commands the line runs. Only its output tells the two entries apart.
    let twice = "$tur += 1; Write-Output \"TUR_$tur\" | Out-String | Write-Output";
    for round in ["TUR_1", "TUR_2"] {
        shell.send(&format!("{twice}\r"));
        shell.wait(round);
    }
    let running = "'ÇALIŞIYOR_' + 'ğ' | Write-Output; Start-Sleep 600";
    shell.send(&format!("{running}\r"));
    shell.wait("ÇALIŞIYOR_ğ");
    let expected = format!(
        "'EDITOR_' + [bool](Get-Module PSReadLine)\0'İFADE_' + (3 + 4)\0{twice}\0{twice}\0{running}\0"
    );
    // Recorded while it runs: it was started, it has not returned.
    recorded(&file, expected.as_bytes());
    shell.endpoint.shutdown().unwrap();
    drop(shell);
    recorded(&file, expected.as_bytes());

    // The next shell of that pane has the command in its session history, did not run it, and
    // records what follows after it.
    history::prepare(home.path(), &key, "powershell").unwrap();
    let mut shell = Shell::open_with(&file, budget, Some(modules.path()));
    assert!(!visible(&shell.seen).contains("ÇALIŞIYOR_ğ"));
    shell.send("'ÖNCEKİ_' + (Get-History)[-1].CommandLine.Length\r");
    shell.wait(&format!("ÖNCEKİ_{}", running.chars().count()));
    let extended = format!("{expected}'ÖNCEKİ_' + (Get-History)[-1].CommandLine.Length\0");
    recorded(&file, extended.as_bytes());
    shell.endpoint.shutdown().unwrap();
}

/// The user's own line-editor history, which no pane may write to.
fn global_history() -> String {
    std::env::var_os("APPDATA")
        .map(std::path::PathBuf::from)
        .map(|data| data.join(r"Microsoft\Windows\PowerShell\PSReadLine\ConsoleHost_history.txt"))
        .and_then(|file| std::fs::read(file).ok())
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
        .unwrap_or_default()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_command_still_running_when_its_pane_or_the_application_closes_is_remembered() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let _cleanup = Cleanup(manager.clone());
    let current = runtime.handle.current().unwrap();
    let base = crate::runtime::browser_services(home.clone(), root.path().into())
        .unwrap()
        .terminal
        .unwrap();
    let marker = uuid::Uuid::new_v4().simple().to_string();
    let pane = |key: &str| {
        let config = Config {
            idle_timeout: None,
            history: Some(key.to_owned()),
            ..base.clone()
        };
        manager.start(
            config,
            current.clone(),
            CLIENT.into(),
            0,
            runtime.handle.clone(),
        )
    };
    let file = |key: &str| home.join("terminal-history").join(key).join("powershell");

    // A pane closed while its command runs.
    let closed = uuid::Uuid::new_v4().to_string();
    let id = pane(&closed).await.unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    let multiline = format!("'ÇOK_' +\r'SATIR_{marker}'\r");
    let running = format!("'ÇALIŞIYOR_' + 'ğ'; Start-Sleep 600 # {marker}");
    manager
        .write(&id, &current.generation, CLIENT, multiline)
        .unwrap();
    wait_for(&terminal, &format!("ÇOK_SATIR_{marker}")).await;
    manager
        .write(&id, &current.generation, CLIENT, format!("{running}\r"))
        .unwrap();
    wait_for(&terminal, "ÇALIŞIYOR_ğ").await;
    // The running command is already recorded: it was accepted, it has not returned.
    let expected = format!("'ÇOK_' +\n'SATIR_{marker}'\0{running}\0");
    recorded(&file(&closed), expected.as_bytes());
    assert!(!terminal.stopped.is_cancelled());
    assert_eq!(
        manager
            .close_joined(&id, &current.generation, CLIENT)
            .await
            .unwrap(),
        None
    );
    recorded(&file(&closed), expected.as_bytes());

    // The next shell of that pane offers both, the multiline one as one command, and records
    // neither a second time when the line editor reads them back.
    let id = pane(&closed).await.unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    manager
        .write(&id, &current.generation, CLIENT, "\u{1b}[A".into())
        .unwrap();
    wait_for(&terminal, &format!("Start-Sleep 600 # {marker}")).await;
    manager
        .write(&id, &current.generation, CLIENT, "\u{3}".into())
        .unwrap();
    let cleared = shown(&terminal).len();
    wait_after(&terminal, cleared, " > ").await;
    manager
        .write(
            &id,
            &current.generation,
            CLIENT,
            "'SONRA_' + (2 + 2)\r".into(),
        )
        .unwrap();
    wait_for(&terminal, "SONRA_4").await;
    let extended = format!("{expected}'SONRA_' + (2 + 2)\0");
    recorded(&file(&closed), extended.as_bytes());
    manager
        .close_joined(&id, &current.generation, CLIENT)
        .await
        .unwrap();

    // The application shut down while a command runs in another pane.
    let quit = uuid::Uuid::new_v4().to_string();
    let id = pane(&quit).await.unwrap();
    let terminal = manager.owned(&id, &current.generation, CLIENT).unwrap();
    wait_for(&terminal, " > ").await;
    let leaving = format!("'KAPANIRKEN_' + 'ş'; Start-Sleep 600 # {marker}");
    manager
        .write(&id, &current.generation, CLIENT, format!("{leaving}\r"))
        .unwrap();
    wait_for(&terminal, "KAPANIRKEN_ş").await;
    manager.shutdown().await;
    assert!(terminal.finished.is_cancelled());
    assert_eq!(
        std::fs::read(file(&quit)).unwrap(),
        format!("{leaving}\0").into_bytes()
    );
    // None of it reached the user's own history.
    assert!(!global_history().contains(&marker));
}
