//! Real Windows PowerShell processes behind a real pseudoconsole; no emulator is attached.
use super::*;
use std::time::Instant;

fn launch(arguments: &[&str]) -> HostLaunch {
    let root = PathBuf::from(std::env::var_os("SystemRoot").expect("SystemRoot"));
    let mut launch = HostLaunch {
        executable: root.join(r"System32\WindowsPowerShell\v1.0\powershell.exe"),
        ..Default::default()
    };
    for argument in ["-NoLogo", "-NoProfile"].iter().chain(arguments) {
        launch.arg(*argument);
    }
    for key in [
        "SystemRoot",
        "SystemDrive",
        "windir",
        "ComSpec",
        "PATH",
        "PATHEXT",
        "TEMP",
        "TMP",
        "USERPROFILE",
        "APPDATA",
        "LOCALAPPDATA",
        "ProgramData",
        "ProgramFiles",
    ] {
        if let Some(value) = std::env::var_os(key) {
            launch.env(key, value);
        }
    }
    launch
}
fn open(launch: HostLaunch) -> Box<dyn TerminalIo> {
    pty::open(
        launch,
        TerminalSize {
            cols: 120,
            rows: 30,
        },
        false,
        &CancellationToken::new(),
    )
    .unwrap()
}
/// What an emulator would show: control sequences removed, text kept.
fn text(bytes: &[u8]) -> String {
    let source = String::from_utf8_lossy(bytes);
    let mut shown = String::new();
    let mut characters = source.chars().peekable();
    while let Some(character) = characters.next() {
        if character != '\u{1b}' {
            shown.push(character);
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
    shown
}
fn collect(endpoint: &mut dyn TerminalIo, seen: &mut Vec<u8>) {
    let mut buffer = [0u8; 16384];
    loop {
        match endpoint.read(&mut buffer) {
            Ok(0) => break,
            Ok(n) => seen.extend_from_slice(&buffer[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
            Err(error) => panic!("terminal read: {error}"),
        }
    }
}
fn read_until(endpoint: &mut dyn TerminalIo, seen: &mut Vec<u8>, wanted: &str) -> String {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        collect(endpoint, seen);
        let shown = text(seen);
        if shown.contains(wanted) {
            return shown;
        }
        assert!(Instant::now() < deadline, "no {wanted:?} in {shown:?}");
        assert!(
            endpoint.try_exit().unwrap().is_none(),
            "exited without {wanted:?}: {shown:?}"
        );
        endpoint
            .wait_ready(false, Duration::from_millis(10))
            .unwrap();
    }
}
fn send(endpoint: &mut dyn TerminalIo, text: &str) {
    let mut bytes = text.as_bytes();
    let deadline = Instant::now() + Duration::from_secs(10);
    while !bytes.is_empty() {
        match endpoint.write(bytes) {
            Ok(n) => bytes = &bytes[n..],
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                assert!(Instant::now() < deadline, "input queue stayed full");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("terminal write: {error}"),
        }
    }
}
fn running(process: u32) -> bool {
    let listed = std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {process}"), "/NH", "/FO", "CSV"])
        .output()
        .unwrap();
    String::from_utf8_lossy(&listed.stdout).contains(&format!("\"{process}\""))
}

#[test]
fn output_written_before_exit_is_delivered_with_the_exit_code() {
    let mut endpoint = open(launch(&[
        "-Command",
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Write-Output 'ĞğİıŞşÇçÖöÜü'; exit 7",
    ]));
    let mut seen = vec![];
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        collect(endpoint.as_mut(), &mut seen);
        if let Some(exit) = endpoint.try_exit().unwrap() {
            break exit;
        }
        assert!(Instant::now() < deadline, "no exit: {:?}", text(&seen));
        endpoint
            .wait_ready(false, Duration::from_millis(10))
            .unwrap();
    };
    assert_eq!(exit.code, 7);
    assert!(!exit.uncertain);
    assert!(text(&seen).contains("ĞğİıŞşÇçÖöÜü"), "{:?}", text(&seen));
    // The request for the cursor position was answered here, not passed to the display.
    assert!(!seen.windows(4).any(|window| window == b"\x1b[6n"));
    assert_eq!(endpoint.shutdown().unwrap().code, 7);
}

#[test]
fn spaced_directories_and_quoted_arguments_reach_the_shell_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let directory = root.path().join("boş luk dizini");
    std::fs::create_dir(&directory).unwrap();
    let mut launch = launch(&[
        "-Command",
        "[Console]::OutputEncoding = [Text.Encoding]::UTF8; (Get-Location).Path; 'a \"q\" b'; 'DONE'",
    ]);
    launch.cwd = Some(directory);
    let mut endpoint = open(launch);
    let mut seen = vec![];
    let shown = read_until(endpoint.as_mut(), &mut seen, "DONE");
    assert!(shown.contains("boş luk dizini"), "{shown:?}");
    assert!(shown.contains("a \"q\" b"), "{shown:?}");
}

#[test]
fn interrupt_stops_a_long_command_and_the_shell_keeps_working() {
    // A launcher that disables Ctrl+C disables it for every process below it, this shell
    // included. The shell accepts it again first, as the pane's own bootstrap does, so the test
    // observes the endpoint's delivery whatever started the test run.
    let mut endpoint = open(launch(&[
        "-NoExit",
        "-Command",
        "$t = [AppDomain]::CurrentDomain.DefineDynamicAssembly((New-Object Reflection.AssemblyName 'T'), 'Run').DefineDynamicModule('T').DefineType('T.N', 'Public,Class'); $m = $t.DefinePInvokeMethod('SetConsoleCtrlHandler', 'kernel32.dll', 'Public,Static,PinvokeImpl', 'Standard', [bool], @([IntPtr], [bool]), 'Winapi', 'Auto'); $m.SetImplementationFlags('PreserveSig'); [void]$t.CreateType()::SetConsoleCtrlHandler([IntPtr]::Zero, $false); function prompt { 'READY' + '> ' }",
    ]));
    let mut seen = vec![];
    read_until(endpoint.as_mut(), &mut seen, "READY> ");
    send(endpoint.as_mut(), "Start-Sleep 120; 'SLEPT_' + 'FULLY'\r");
    let started = Instant::now();
    // The command is running once its echoed line has been shown.
    read_until(endpoint.as_mut(), &mut seen, "'FULLY'");
    std::thread::sleep(Duration::from_millis(500));
    seen.clear();
    send(endpoint.as_mut(), "\u{3}");
    // An interrupt discards pending input, so the next line waits for the returned prompt.
    read_until(endpoint.as_mut(), &mut seen, "READY> ");
    send(endpoint.as_mut(), "'AFTER_' + (1 + 1)\r");
    let shown = read_until(endpoint.as_mut(), &mut seen, "AFTER_2");
    assert!(started.elapsed() < Duration::from_secs(60));
    assert!(!shown.contains("SLEPT_FULLY"), "{shown:?}");
    assert!(endpoint.try_exit().unwrap().is_none());
}

#[test]
fn closing_during_unread_output_and_resizes_returns_promptly() {
    let mut endpoint = open(launch(&[
        "-Command",
        "'STARTED'; while ($true) { 'x' * 200 }",
    ]));
    let mut seen = vec![];
    read_until(endpoint.as_mut(), &mut seen, "STARTED");
    // Nothing is read from here on: both queues and the console's pipe fill up.
    for (cols, rows) in [(40, 10), (200, 60), (2, 2), (120, 30)] {
        endpoint.resize(TerminalSize { cols, rows }).unwrap();
        std::thread::sleep(Duration::from_millis(100));
    }
    let closing = Instant::now();
    let exit = endpoint.shutdown().unwrap();
    assert!(closing.elapsed() < Duration::from_secs(10));
    // A second shutdown, as the destructor performs, reports the same outcome.
    assert_eq!(endpoint.shutdown().unwrap().code, exit.code);
}

#[test]
fn closing_the_pane_leaves_no_process_of_the_shell_behind() {
    let mut endpoint = open(launch(&["-NoExit", "-Command", "'READY'"]));
    let mut seen = vec![];
    read_until(endpoint.as_mut(), &mut seen, "READY");
    send(
        endpoint.as_mut(),
        "$p = Start-Process ping.exe -ArgumentList '-n','600','127.0.0.1' -NoNewWindow -PassThru; 'CHILD_' + $p.Id + '_END'\r",
    );
    let deadline = Instant::now() + Duration::from_secs(30);
    let process = loop {
        let shown = read_until(endpoint.as_mut(), &mut seen, "_END");
        let found = shown.match_indices("CHILD_").find_map(|(start, marker)| {
            let digits: String = shown[start + marker.len()..]
                .chars()
                .take_while(char::is_ascii_digit)
                .collect();
            let rest = &shown[start + marker.len() + digits.len()..];
            (rest.starts_with("_END")).then(|| digits.parse::<u32>().ok())?
        });
        if let Some(process) = found {
            break process;
        }
        assert!(Instant::now() < deadline, "{shown:?}");
        endpoint
            .wait_ready(false, Duration::from_millis(10))
            .unwrap();
    };
    assert!(running(process));
    endpoint.shutdown().unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    while running(process) {
        assert!(
            Instant::now() < deadline,
            "process {process} outlived its pane"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn reserved_variables_are_matched_as_windows_matches_names() {
    for name in ["WES_BRIDGE_TOKEN", "wes_bridge_token", "Wes_Workspace"] {
        assert!(reserved_variable(name));
    }
    assert!(!reserved_variable("WESTERN"));
}
