//! The terminal's Windows plumbing with a native program standing in for the SSH client:
//! `cmd.exe` ignores the client's options and stays interactive behind the pseudoconsole.
use super::*;
use std::time::{Duration, Instant};
use wes_engine::{
    environments::{EnvironmentLoader, Registry},
    execution::{TerminalIo, TerminalSize},
};

struct Fixture {
    root: tempfile::TempDir,
    target: Target,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::Builder::new()
            .prefix("ssh boş luk ")
            .tempdir()
            .unwrap();
        let system = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap());
        std::fs::write(root.path().join("key"), b"synthetic key").unwrap();
        std::fs::write(root.path().join("hosts"), b"synthetic host").unwrap();
        let yaml = serde_json::json!({"version":1,"targets":{"remote":{
            "kind":"ssh","client":system.join(r"System32\cmd.exe"),"host":"127.0.0.1","port":2222,
            "user":"synthetic","identity_file":root.path().join("key"),
            "known_hosts":root.path().join("hosts"),"shell":"posix","inherit":"remote"
        }},"environments":{"qa":{"imports":{"tool":{"source":{"kind":"process","bin":"/remote/tool"},
            "bind":{"target":"remote"}}}}}});
        let loader = crate::environments::LocalEnvironments::new(root.path()).unwrap();
        let loaded = loader.capture_text(&yaml.to_string(), None, None).unwrap();
        let mut registry = Registry::default();
        let plan = registry
            .plan(
                &wes_core::environments::Package::parse(&loaded.yaml).unwrap(),
                &loaded.sources,
            )
            .unwrap();
        registry.apply(plan).unwrap();
        let binding = registry.inspect("qa").unwrap().bind("tool").unwrap();
        Self {
            root,
            target: binding.import().target().clone(),
        }
    }
    fn plan(&self) -> crate::execution_targets::TerminalPlan {
        crate::execution_targets::TerminalPlan::for_target(&self.target).unwrap()
    }
}
const SIZE: TerminalSize = TerminalSize {
    cols: 120,
    rows: 30,
};

fn read_until(endpoint: &mut dyn TerminalIo, seen: &mut Vec<u8>, wanted: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut buffer = [0u8; 16384];
    loop {
        loop {
            match std::io::Read::read(endpoint, &mut buffer) {
                Ok(0) => break,
                Ok(n) => seen.extend_from_slice(&buffer[..n]),
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => panic!("terminal read: {error}"),
            }
        }
        let shown = String::from_utf8_lossy(seen);
        if shown.contains(wanted) {
            return;
        }
        assert!(Instant::now() < deadline, "no {wanted:?} in {shown:?}");
        endpoint
            .wait_ready(false, Duration::from_millis(10))
            .unwrap();
    }
}

#[test]
fn a_remote_terminal_runs_its_client_behind_the_console_without_the_hosts_environment() {
    let fixture = Fixture::new();
    let plan = fixture.plan();
    assert_eq!(plan.support().label, "SSH");
    assert!(!plan.support().workspace_tools);
    let mut endpoint = plan.open(None, SIZE, &CancellationToken::new()).unwrap();
    let mut seen = vec![];
    // The client was given the terminal type; nothing else of the host's environment reached it.
    std::io::Write::write_all(&mut endpoint, b"echo MARK_%TERM%_%USERPROFILE%_END\r").unwrap();
    read_until(
        endpoint.as_mut(),
        &mut seen,
        "MARK_xterm-256color_%USERPROFILE%_END",
    );
    // The Windows client cannot start without these two; they are all it is given.
    std::io::Write::write_all(
        &mut endpoint,
        b"if defined SystemRoot if defined ProgramData echo BOTH_%TERM:~0,5%\r",
    )
    .unwrap();
    read_until(endpoint.as_mut(), &mut seen, "BOTH_xterm");
    assert_eq!(
        client_environment()
            .into_iter()
            .map(|(name, _)| name)
            .collect::<Vec<_>>(),
        ["SystemRoot", "ProgramData"]
    );
    // The client's own exit code for a lost connection leaves the remote outcome unknown.
    std::io::Write::write_all(&mut endpoint, b"exit 255\r").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let exit = loop {
        read_until(endpoint.as_mut(), &mut seen, "MARK_");
        if let Some(exit) = endpoint.try_exit().unwrap() {
            break exit;
        }
        assert!(Instant::now() < deadline);
        endpoint
            .wait_ready(false, Duration::from_millis(10))
            .unwrap();
    };
    assert_eq!(exit.code, 255);
    assert!(exit.uncertain);
}

#[test]
fn a_remote_terminal_takes_no_workspace_material_and_rechecks_its_inputs() {
    let fixture = Fixture::new();
    let refused = fixture
        .plan()
        .open(
            Some(crate::execution_targets::HostLaunch::default()),
            SIZE,
            &CancellationToken::new(),
        )
        .err()
        .unwrap();
    assert!(refused.to_string().contains("workspace bootstrap"));
    let plan = fixture.plan();
    std::fs::remove_file(fixture.root.path().join("key")).unwrap();
    let missing = plan
        .open(None, SIZE, &CancellationToken::new())
        .err()
        .unwrap();
    assert!(missing.to_string().contains("identity file was not found"));
    assert!(missing.to_string().contains("SSH client was not started"));
}
