use indexmap::IndexMap;
use std::{
    fs::OpenOptions,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};
use tempfile::TempDir;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use wes_adapters::process::{self, ProcessConfig, ProcessInvoker};
use wes_core::{
    Data, ErrorValue, Provenance, Shape, Value,
    capability::{Capability, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationError, Invoker},
    runtime::{Effect, ExecutionTraits, Runtime},
};
#[path = "process/interactive.rs"]
mod interactive;

struct Fixture {
    _directory: TempDir,
    executable: String,
    lock: PathBuf,
}
impl Fixture {
    async fn new() -> Self {
        tokio::task::spawn_blocking(|| {
            static BUILT: OnceLock<(TempDir, PathBuf)> = OnceLock::new();
            let (_, compiled) = BUILT.get_or_init(|| {
                let directory = tempfile::tempdir().unwrap();
                let executable = directory.path().join(if cfg!(windows) {
                    "fixture.exe"
                } else {
                    "fixture"
                });
                let rustc = Path::new(env!("CARGO")).with_file_name(if cfg!(windows) {
                    "rustc.exe"
                } else {
                    "rustc"
                });
                let output = std::process::Command::new(rustc)
                    .arg("--edition=2024")
                    .arg(
                        Path::new(env!("CARGO_MANIFEST_DIR"))
                            .join("tests/support/process_fixture.rs"),
                    )
                    .arg("-o")
                    .arg(&executable)
                    .output()
                    .unwrap();
                assert!(
                    output.status.success(),
                    "fixture compilation: {}",
                    String::from_utf8_lossy(&output.stderr)
                );
                // Prepare the immutable executable before a test starts measuring invocation
                // budgets. Native first-launch validation is part of fixture setup, like rustc.
                let ready = std::process::Command::new(&executable)
                    .args(["stdin", "", ""])
                    .output()
                    .unwrap();
                assert!(ready.status.success(), "fixture preparation: {ready:?}");
                (directory, executable)
            });
            // Share only the immutable program; each invocation owns separate files and locks.
            let directory = tempfile::tempdir().unwrap();
            let lock = directory.path().join("child.lock");
            Self {
                executable: compiled.to_str().unwrap().into(),
                lock,
                _directory: directory,
            }
        })
        .await
        .unwrap()
    }
    fn invoker(
        &self,
        mode: &str,
        endpoint: &str,
        config: ProcessConfig,
    ) -> (Arc<Capability>, ProcessInvoker) {
        let command = vec![
            self.executable.clone(),
            mode.into(),
            self.lock.to_str().unwrap().into(),
            endpoint.into(),
        ];
        let (description, invoker) = process::provider("fixture", command, "arg", config).unwrap();
        (description.capabilities().next().unwrap().clone(), invoker)
    }
    fn lock_is_available(&self) -> bool {
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.lock)
            .unwrap()
            .try_lock()
            .is_ok()
    }
}
async fn assert_control_closed(control: &mut tokio::net::TcpStream) {
    let ended = tokio::time::timeout(Duration::from_secs(1), control.read(&mut [0; 1]))
        .await
        .unwrap();
    assert!(
        matches!(ended, Ok(0))
            || matches!(ended, Err(ref error) if matches!(error.kind(), std::io::ErrorKind::ConnectionReset | std::io::ErrorKind::ConnectionAborted)),
        "descendant connection still open: {ended:?}"
    );
}
fn call(capability: Arc<Capability>, arguments: IndexMap<String, Value>) -> Call {
    let mut runtime = Runtime::new();
    runtime
        .add(
            (),
            [],
            ExecutionTraits {
                pure: false,
                repeatable: false,
                bounded: true,
            },
        )
        .unwrap();
    let run = runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| match e {
            Effect::Spawn(t) => Some(t.run),
            _ => None,
        })
        .unwrap();
    Call {
        authority: Default::default(),
        run,
        capability,
        arguments,
    }
}
fn args(values: impl IntoIterator<Item = (&'static str, Data)>) -> IndexMap<String, Value> {
    values
        .into_iter()
        .map(|(key, data)| {
            (
                key.into(),
                Value::new(Shape::Unknown, data, Provenance::default()).unwrap(),
            )
        })
        .collect()
}
async fn invoke(
    cap: Arc<Capability>,
    invoker: &ProcessInvoker,
    arguments: IndexMap<String, Value>,
) -> Result<Value, InvocationError> {
    tokio::time::timeout(
        Duration::from_secs(4),
        invoker.invoke(call(cap, arguments), CancellationToken::new()),
    )
    .await
    .unwrap()
}
fn fields(value: &Value) -> &IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("process record")
    };
    fields
}
fn failure(result: Result<Value, InvocationError>) -> ErrorValue {
    match result {
        Err(InvocationError::Failed(error)) => error,
        other => panic!("expected failure: {other:?}"),
    }
}

#[tokio::test]
async fn binary_streams_and_nonzero_exit_are_preserved_as_a_typed_result() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("bytes", "", ProcessConfig::default());
    assert_eq!(cap.safety, Safety::Unsafe);
    assert_eq!(cap.result, process::output_shape());
    let value = invoke(cap, &invoker, IndexMap::new()).await.unwrap();
    assert_eq!(value.shape(), &process::output_shape());
    assert_eq!(fields(&value)["exitCode"], Data::Int(7));
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes(vec![0, 255, 1, 10].into())
    );
    assert_eq!(
        fields(&value)["stderr"],
        Data::Bytes(vec![254, 0, 2].into())
    );
    assert!(
        value
            .provenance()
            .fact("ranLocally")
            .unwrap()
            .starts_with(&fixture.executable)
    );
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn direct_arguments_remain_one_literal_os_argument_and_are_never_shell_programs() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("arg", "", ProcessConfig::default());
    let text = "two words ; echo NOT_EXECUTED > missing-file $(echo nope) \"quoted\"\nnext";
    let value = invoke(
        cap.clone(),
        &invoker,
        args([("arg", Data::Text(text.into()))]),
    )
    .await
    .unwrap();
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes(text.as_bytes().to_vec().into())
    );
    let absent = invoke(cap.clone(), &invoker, IndexMap::new())
        .await
        .unwrap();
    assert_eq!(
        fields(&absent)["stdout"],
        Data::Bytes(b"absent".to_vec().into())
    );
    let empty = invoke(
        cap,
        &invoker,
        args([("arg", Data::Text(String::new().into()))]),
    )
    .await
    .unwrap();
    assert_eq!(fields(&empty)["stdout"], Data::Bytes(vec![].into()));
}

#[tokio::test]
async fn stdout_and_stderr_are_drained_concurrently_and_share_one_budget() {
    let fixture = Fixture::new().await;
    let config = ProcessConfig {
        output_bytes: 256 * 1024,
        ..ProcessConfig::default()
    };
    let (cap, invoker) = fixture.invoker("flood", "", config);
    let value = invoke(cap, &invoker, IndexMap::new()).await.unwrap();
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes(vec![b'o'; 128 * 1024].into())
    );
    assert_eq!(
        fields(&value)["stderr"],
        Data::Bytes(vec![b'e'; 128 * 1024].into())
    );
    let (cap, invoker) = fixture.invoker(
        "flood",
        "",
        ProcessConfig {
            output_bytes: 200 * 1024,
            ..config
        },
    );
    assert_eq!(
        failure(invoke(cap, &invoker, IndexMap::new()).await).code(),
        "PROC004"
    );
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn finite_stdin_is_eof_instead_of_an_unowned_open_pipe() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("stdin", "", ProcessConfig::default());
    let value = invoke(cap, &invoker, IndexMap::new()).await.unwrap();
    assert_eq!(fields(&value)["stdout"], Data::Bytes(b"0".to_vec().into()));
}

#[tokio::test]
async fn cancellation_waits_for_direct_child_exit_and_releases_its_os_resources() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (cap, invoker) = fixture.invoker(
        "wait",
        &listener.local_addr().unwrap().to_string(),
        ProcessConfig::default(),
    );
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        invoker
            .invoke(call(cap.clone(), IndexMap::new()), cancelled)
            .await,
        Err(InvocationError::Cancelled)
    ));
    assert!(!fixture.lock.exists());
    let token = CancellationToken::new();
    let running = tokio::spawn(invoker.invoke(call(cap, IndexMap::new()), token.clone()));
    let (mut ready, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut message = [0; 5];
    ready.read_exact(&mut message).await.unwrap();
    assert_eq!(&message, b"ready");
    assert!(!fixture.lock_is_available());
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), running)
            .await
            .unwrap()
            .unwrap(),
        Err(InvocationError::Cancelled)
    ));
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn timeout_and_output_limit_terminate_and_reap_before_returning_failure() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("wait", "", ProcessConfig::default());
    let error = failure(
        invoke(
            cap,
            &invoker,
            args([("timeout", Data::Duration("PT0.1S".parse().unwrap()))]),
        )
        .await,
    );
    assert_eq!(error.code(), "PROC003");
    // The budget includes startup: a loaded CI host may cancel before the fixture opens its lock.
    // If it acquired the resource, cleanup must have released it before this result.
    if fixture.lock.exists() {
        assert!(fixture.lock_is_available());
    }
    let (cap, invoker) = fixture.invoker(
        "limit",
        "",
        ProcessConfig {
            output_bytes: 1024,
            ..ProcessConfig::default()
        },
    );
    assert_eq!(
        failure(invoke(cap, &invoker, IndexMap::new()).await).code(),
        "PROC004"
    );
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn invalid_arguments_and_duration_fail_before_launch_and_do_not_echo_input() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("arg", "", ProcessConfig::default());
    for arguments in [
        args([("arg", Data::Text("fixture-private\0bad".into()))]),
        args([("arg", Data::Int(1))]),
        args([("timeout", Data::Text("fixture-private".into()))]),
        args([("timeout", Data::Duration("PT0S".parse().unwrap()))]),
        args([("timeout", Data::Duration("-PT1S".parse().unwrap()))]),
        args([(
            "timeout",
            Data::Duration("PT9223372036854775807S".parse().unwrap()),
        )]),
    ] {
        let error = failure(invoke(cap.clone(), &invoker, arguments).await);
        assert_eq!(error.code(), "PROC001");
        assert!(!format!("{error:?}").contains("fixture-private"));
    }
    assert!(!fixture.lock.exists());
    assert!(!format!("{invoker:?}").contains(&fixture.executable));
}

#[tokio::test]
async fn failed_spawn_has_no_command_line_or_host_error_payload() {
    let directory = tempfile::tempdir().unwrap();
    let executable = directory.path().join("fixture-private-nonexistent");
    let (description, invoker) = process::wrapping(
        "fixture",
        executable.to_str().unwrap(),
        ProcessConfig::default(),
    )
    .unwrap();
    let error = failure(
        invoke(
            description.capabilities().next().unwrap().clone(),
            &invoker,
            IndexMap::new(),
        )
        .await,
    );
    assert_eq!(error.code(), "PROC002");
    assert!(!format!("{error:?}").contains("fixture-private"));
}

#[test]
fn configuration_limits_and_native_shell_metadata_are_explicit() {
    assert!(process::provider("fixture", vec![], "arg", ProcessConfig::default()).is_err());
    assert!(
        process::provider(
            "fixture",
            vec!["tool".into()],
            "timeout",
            ProcessConfig::default()
        )
        .is_err()
    );
    assert!(
        process::wrapping(
            "fixture",
            "tool",
            ProcessConfig {
                argument_bytes: 3,
                ..ProcessConfig::default()
            }
        )
        .is_err()
    );
    let (description, _) = process::shell(ProcessConfig::default()).unwrap();
    let cap = description.capabilities().next().unwrap();
    assert_eq!(cap.safety, Safety::Unsafe);
    assert!(cap.parameter("cmd").unwrap().content.is_some());
    let (direct, _) = process::wrapping("fixture", "tool", ProcessConfig::default()).unwrap();
    assert!(
        direct
            .capabilities()
            .next()
            .unwrap()
            .parameter("args")
            .unwrap()
            .content
            .is_none()
    );
}

#[tokio::test]
async fn cancelling_after_parent_exit_does_not_wait_for_a_descendants_inherited_pipe() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (cap, invoker) = fixture.invoker(
        "descendant",
        &listener.local_addr().unwrap().to_string(),
        ProcessConfig::default(),
    );
    let token = CancellationToken::new();
    let running = tokio::spawn(invoker.invoke(call(cap, IndexMap::new()), token.clone()));
    let (mut control, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
        .await
        .unwrap()
        .unwrap();
    let mut ready = [0; 5];
    control.read_exact(&mut ready).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), async {
        while !fixture.lock_is_available() {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert!(!running.is_finished()); // Parent exited, but the inherited output pipe is still open.
    token.cancel();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), running)
            .await
            .unwrap()
            .unwrap(),
        Err(InvocationError::Cancelled)
    ));
    let descendant_lock = fixture.lock.with_extension("desc.lock");
    assert_control_closed(&mut control).await;
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            if OpenOptions::new()
                .read(true)
                .write(true)
                .open(&descendant_lock)
                .unwrap()
                .try_lock()
                .is_ok()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn only_the_explicit_native_shell_interprets_shell_source() {
    let (description, invoker) = process::shell(ProcessConfig::default()).unwrap();
    #[cfg(unix)]
    let source = "printf fixture-shell";
    #[cfg(windows)]
    let source = "echo fixture-shell";
    let value = invoke(
        description.capabilities().next().unwrap().clone(),
        &invoker,
        args([("cmd", Data::Text(source.into()))]),
    )
    .await
    .unwrap();
    assert_eq!(fields(&value)["exitCode"], Data::Int(0));
    let Data::Bytes(output) = &fields(&value)["stdout"] else {
        panic!("stdout bytes")
    };
    assert_eq!(String::from_utf8_lossy(output).trim(), "fixture-shell");
}

/// Deadline/output failures terminate an already-started descendant, not just its exited parent.
#[tokio::test]
async fn failed_capture_ends_the_owned_descendant_without_a_release_command() {
    for output_limit in [false, true] {
        let fixture = Fixture::new().await;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (cap, invoker) = fixture.invoker(
            if output_limit {
                "descendant-flood"
            } else {
                "descendant"
            },
            &listener.local_addr().unwrap().to_string(),
            ProcessConfig {
                timeout: Duration::from_secs(60),
                output_bytes: if output_limit {
                    16
                } else {
                    ProcessConfig::default().output_bytes
                },
                ..ProcessConfig::default()
            },
        );
        let running =
            tokio::spawn(invoker.invoke(call(cap, IndexMap::new()), CancellationToken::new()));
        let (mut control, _) = tokio::time::timeout(Duration::from_secs(2), listener.accept())
            .await
            .unwrap()
            .unwrap();
        control.read_exact(&mut [0; 5]).await.unwrap();
        if !output_limit {
            // Advance only after the real descendant's readiness, then use real time for cleanup.
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(61)).await;
            let cleanup = std::time::Instant::now() + Duration::from_secs(5);
            while !running.is_finished() {
                assert!(
                    std::time::Instant::now() < cleanup,
                    "native cleanup did not finish"
                );
                tokio::task::yield_now().await;
            }
            tokio::time::resume();
        }
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(2), running)
                .await
                .unwrap()
                .unwrap(),
            Err(InvocationError::Failed(_))
        ));
        assert_control_closed(&mut control).await;
    }
}

/// The program reaches the native shell as written: its quotes are the shell's to read, not
/// characters escaped on the way for some other program's argument rules.
#[tokio::test]
async fn quotes_in_shell_source_reach_the_native_shell_as_written() {
    let (description, invoker) = process::shell(ProcessConfig::default()).unwrap();
    #[cfg(unix)]
    let source = r#"if [ "a  b" = "a  b" ]; then printf '%s' "same  words"; fi"#;
    #[cfg(windows)]
    let source = r#"if "a  b"=="a  b" echo "same  words""#;
    let value = invoke(
        description.capabilities().next().unwrap().clone(),
        &invoker,
        args([("cmd", Data::Text(source.into()))]),
    )
    .await
    .unwrap();
    assert_eq!(fields(&value)["exitCode"], Data::Int(0));
    let Data::Bytes(output) = &fields(&value)["stdout"] else {
        panic!("stdout bytes")
    };
    // cmd's echo prints its quotes; two spaces survive in both shells.
    assert_eq!(
        String::from_utf8_lossy(output).trim().trim_matches('"'),
        "same  words"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn native_signal_exit_remains_an_exit_code_not_a_false_user_cancellation() {
    let (description, invoker) = process::shell(ProcessConfig::default()).unwrap();
    // Only the newly spawned test shell signals itself; no parent or unrelated PID is addressed.
    let value = invoke(
        description.capabilities().next().unwrap().clone(),
        &invoker,
        args([("cmd", Data::Text("kill -TERM $$".into()))]),
    )
    .await
    .unwrap();
    assert_eq!(fields(&value)["exitCode"], Data::Int(143));
}
