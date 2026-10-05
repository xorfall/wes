use super::*;
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;
use wes_core::{Primitive, Shape};
use wes_engine::environments::{EnvironmentLoader, Registry};

struct Fixture {
    root: tempfile::TempDir,
    loader: crate::environments::LocalEnvironments,
    binding: Binding,
    product: ImportProduct,
    yaml: serde_json::Value,
}
impl Fixture {
    fn new(body: &str, timeout: i64, remote: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("ssh fixture '")
            .tempdir()
            .unwrap();
        let client = root.path().join("client");
        std::fs::write(&client, format!("#!/bin/sh\nprintf '%s\\0' \"$@\" > {}\nprintf '%s' \"${{SSH_AUTH_SOCK-unset}}\" > {}\nprintf '%s' \"$$\" > {}\n/bin/mv {} {}\n{body}\n",
            quote(root.path().join("argv").to_str().unwrap()), quote(root.path().join("ambient").to_str().unwrap()),
            quote(root.path().join("pid.pending").to_str().unwrap()),
            quote(root.path().join("pid.pending").to_str().unwrap()),
            quote(root.path().join("pid").to_str().unwrap()))).unwrap();
        std::fs::set_permissions(&client, std::fs::Permissions::from_mode(0o700)).unwrap();
        std::fs::write(root.path().join("key"), b"synthetic key").unwrap();
        std::fs::write(root.path().join("hosts"), b"synthetic host").unwrap();
        let yaml = serde_json::json!({"version":1,"targets":{"remote":{
            "kind":"ssh","client":client,"host":"127.0.0.1","port":2222,"user":"synthetic",
            "identity_file":root.path().join("key"),"known_hosts":root.path().join("hosts"),
            "shell":"posix","inherit":"remote","cwd":root.path(),"env":{"SSH_QA":"literal '$HOME; value"}
        }},"environments":{"qa":{"imports":{"tool":{"source":{"kind":"process","bin":remote},
            "bind":{"target":"remote","timeout_ms":timeout}}}}}});
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
        let product = loader.build("tool", &binding).unwrap();
        assert!(
            !root.path().join("argv").exists(),
            "capture/build must not run the client"
        );
        Self {
            root,
            loader,
            binding,
            product,
            yaml,
        }
    }
    fn call(&self, argument: Option<Value>) -> Call {
        use wes_engine::runtime::{Effect, ExecutionTraits, Runtime};
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
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        Call {
            authority: Default::default(),
            run,
            capability: self
                .product
                .description()
                .capabilities()
                .next()
                .unwrap()
                .clone(),
            arguments: argument.into_iter().map(|v| ("args".into(), v)).collect(),
        }
    }
    fn text_call(&self, text: &str) -> Call {
        self.call(Some(
            Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text(text.into()),
                Provenance::default(),
            )
            .unwrap(),
        ))
    }
    async fn started(&self) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while !self.root.path().join("pid").exists() {
                tokio::time::sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
    }
    fn client_is_reaped(&self) {
        let pid: i32 = std::fs::read_to_string(self.root.path().join("pid"))
            .unwrap()
            .parse()
            .unwrap();
        let result = std::process::Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap();
        assert!(!result.status.success(), "local client must be joined");
    }
}
fn fields(value: &Value) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(fields) = value.data() else {
        panic!("record")
    };
    fields
}

#[tokio::test]
async fn literal_arguments_explicit_environment_and_stable_target_binding() {
    let f = Fixture::new(
        "for last; do :; done\nexec /bin/sh -c \"$last\"",
        5000,
        "/bin/echo",
    );
    let argument = "a' ; $(touch SHOULD_NOT_EXIST) `echo no` $HOME\nlast";
    let value = f
        .product
        .invoker()
        .invoke(f.text_call(argument), CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes(format!("{argument}\n").into_bytes().into())
    );
    assert_eq!(fields(&value)["exitCode"], Data::Int(0));
    assert!(!f.root.path().join("SHOULD_NOT_EXIST").exists());
    assert_eq!(
        std::fs::read_to_string(f.root.path().join("ambient")).unwrap(),
        "unset"
    );
    let bytes = std::fs::read(f.root.path().join("argv")).unwrap();
    let argv: Vec<_> = bytes
        .split(|b| *b == 0)
        .filter(|v| !v.is_empty())
        .map(|v| std::str::from_utf8(v).unwrap())
        .collect();
    for expected in [
        "none",
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "IdentityAgent=none",
        "IdentitiesOnly=yes",
        "ControlPath=none",
        "ClearAllForwardings=yes",
        "ProxyCommand=none",
        "ConnectionAttempts=1",
        "2222",
    ] {
        assert!(argv.contains(&expected), "{expected}");
    }
    assert!(argv.iter().any(|s| s.starts_with("UserKnownHostsFile=\"")));
    assert_eq!(value.provenance().fact("ssh.host"), Some("127.0.0.1"));
    f.client_is_reaped();
    // Replay requires the captured binding, not any of the original files.
    std::fs::remove_file(f.root.path().join("client")).unwrap();
    assert!(f.loader.build("tool", &f.binding).is_ok());
}

#[tokio::test]
async fn nonzero_child_exit_is_data_but_255_is_unknown_without_retry() {
    for (code, uncertain) in [(17, false), (255, true)] {
        let f = Fixture::new(
            &format!("printf child-output\nprintf child-error >&2\nexit {code}"),
            5000,
            "/not/on/host/tool",
        );
        let result = f
            .product
            .invoker()
            .invoke(f.call(None), CancellationToken::new())
            .await;
        if uncertain {
            let error = reported(result.unwrap_err(), "ENV036");
            assert_eq!(error.issues()[0].message, "child-error");
            assert!(error.message().contains("may have run"));
        } else {
            let value = result.unwrap();
            assert_eq!(fields(&value)["exitCode"], Data::Int(code));
            assert_eq!(
                fields(&value)["stderr"],
                Data::Bytes(b"child-error".to_vec().into())
            );
        }
        f.client_is_reaped();
    }
}

#[tokio::test]
async fn cancellation_and_authority_loss_join_client_without_claiming_remote_termination() {
    for revoke in [false, true] {
        let f = Fixture::new("exec /bin/sleep 30", 5000, "/bin/echo");
        let cancellation = CancellationToken::new();
        let work = tokio::spawn(
            f.product
                .invoker()
                .invoke(f.call(None), cancellation.clone()),
        );
        f.started().await;
        if revoke {
            f.loader.authority().unwrap().disable("qa").unwrap();
        } else {
            cancellation.cancel();
        }
        let result = tokio::time::timeout(Duration::from_secs(5), work)
            .await
            .unwrap()
            .unwrap();
        reported(result.unwrap_err(), "ENV036");
        f.client_is_reaped();
    }
}

#[tokio::test]
async fn timeout_and_output_budget_join_client_and_report_uncertain_remote_effect() {
    for (body, reason) in [
        ("exec /bin/sleep 300", "time budget"),
        ("exec /usr/bin/yes ssh-output", "output budget"),
    ] {
        let f = Fixture::new(body, 30_000, "/bin/echo");
        let work = tokio::spawn(
            f.product
                .invoker()
                .invoke(f.call(None), CancellationToken::new()),
        );
        // OS startup remains real: prove the fixture entered before expiring
        // its budget. Otherwise a busy host can kill it before it writes pid.
        f.started().await;
        if reason == "time budget" {
            tokio::time::pause();
            tokio::time::advance(Duration::from_secs(31)).await;
        }
        let result = work.await.unwrap();
        if reason == "time budget" {
            tokio::time::resume();
        }
        let error = reported(result.unwrap_err(), "ENV036");
        assert!(error.message().contains(reason), "{}", error.message());
        f.client_is_reaped();
    }
}

#[tokio::test]
async fn private_args_disabled_authority_and_pre_cancel_refuse_without_launch() {
    let f = Fixture::new("exit 0", 5000, "/bin/echo");
    let value = Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text("PRIVATE_SENTINEL".into()),
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    )
    .unwrap();
    let error = f
        .product
        .invoker()
        .invoke(f.call(Some(value)), CancellationToken::new())
        .await
        .unwrap_err()
        .to_string();
    assert!(error.contains("private arguments") && !error.contains("PRIVATE_SENTINEL"));
    let cancellation = CancellationToken::new();
    cancellation.cancel();
    assert!(matches!(
        f.product.invoker().invoke(f.call(None), cancellation).await,
        Err(InvocationError::Cancelled)
    ));
    f.loader.authority().unwrap().disable("qa").unwrap();
    reported(
        f.product
            .invoker()
            .invoke(f.call(None), CancellationToken::new())
            .await
            .unwrap_err(),
        "ENV020",
    );
    assert!(!f.root.path().join("pid").exists());
}

#[test]
fn target_validation_and_namespace_checks_are_inert_and_actionable() {
    let f = Fixture::new("exit 0", 5000, "/remote-only/tool");
    for (field, invalid) in [
        ("host", "-oProxyCommand=bad"),
        ("user", "bad user"),
        ("client", "relative/ssh"),
        ("identity_file", "/tmp/%h"),
        ("known_hosts", "/tmp/$HOME"),
        ("cwd", "relative"),
    ] {
        let mut yaml = f.yaml.clone();
        yaml["targets"]["remote"][field] = invalid.into();
        assert!(
            f.loader
                .capture_text(&yaml.to_string(), None, None)
                .is_err(),
            "{field}"
        );
    }
    for (field, invalid) in [
        ("shell", "powershell"),
        ("inherit", "host"),
        ("typo", "value"),
    ] {
        let mut yaml = f.yaml.clone();
        yaml["targets"]["remote"][field] = invalid.into();
        assert!(
            wes_core::environments::Package::parse(&yaml.to_string()).is_err(),
            "{field}"
        );
    }
    let mut yaml = f.yaml.clone();
    yaml["targets"]["local"] = serde_json::json!({"kind":"local"});
    yaml["environments"]["qa"]["imports"]["other"] = serde_json::json!({"source":{"kind":"process","bin":"/remote-only/tool"},"bind":{"target":"local"}});
    assert!(
        f.loader
            .capture_text(&yaml.to_string(), None, None)
            .err()
            .unwrap()
            .message
            .contains("namespaces")
    );
    assert!(!f.root.path().join("pid").exists());
}

#[test]
fn explicit_binding_roundtrip_revisions_and_unsupported_provider_targets() {
    let f = Fixture::new("exit 0", 5000, "/remote-only/tool");
    let original = f.binding.environment().revision();
    std::fs::write(f.root.path().join("other-key"), b"synthetic").unwrap();
    std::fs::write(f.root.path().join("other-hosts"), b"synthetic").unwrap();
    for (field, value) in [
        ("host", serde_json::json!("another.invalid")),
        ("port", serde_json::json!(22)),
        ("user", serde_json::json!("other")),
        (
            "identity_file",
            serde_json::json!(f.root.path().join("other-key")),
        ),
        (
            "known_hosts",
            serde_json::json!(f.root.path().join("other-hosts")),
        ),
        ("client", serde_json::json!("/other/ssh")),
    ] {
        let mut yaml = f.yaml.clone();
        yaml["targets"]["remote"][field] = value;
        let loaded = f
            .loader
            .capture_text(&yaml.to_string(), None, None)
            .unwrap();
        let mut registry = Registry::default();
        registry
            .apply(
                registry
                    .plan(
                        &wes_core::environments::Package::parse(&loaded.yaml).unwrap(),
                        &loaded.sources,
                    )
                    .unwrap(),
            )
            .unwrap();
        assert_ne!(
            registry.inspect("qa").unwrap().revision(),
            original,
            "{field}"
        );
    }
    let mut yaml = f.yaml.clone();
    yaml["environments"]["qa"]["imports"]["observe"] = serde_json::json!({"source":{"kind":"docker","socket":"/not/contacted.sock"},"bind":{"target":"remote"}});
    let loaded = f
        .loader
        .capture_text(&yaml.to_string(), None, None)
        .unwrap();
    let mut registry = Registry::default();
    registry
        .apply(
            registry
                .plan(
                    &wes_core::environments::Package::parse(&loaded.yaml).unwrap(),
                    &loaded.sources,
                )
                .unwrap(),
        )
        .unwrap();
    let error = f
        .loader
        .build(
            "observe",
            &registry.inspect("qa").unwrap().bind("observe").unwrap(),
        )
        .err()
        .unwrap();
    assert!(error.message.contains("local target"));
}

fn reported(error: InvocationError, code: &str) -> ErrorValue {
    let InvocationError::Failed(error) = error else {
        panic!("expected failure")
    };
    assert_eq!(error.code(), code);
    error
}

#[test]
fn queued_launch_respects_expired_budget_and_original_authority_lifetime() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        for revoke in [false, true] {
            let f = Fixture::new("exit 0", if revoke { 5000 } else { 10 }, "/bin/echo");
            let (release, blocked) = std::sync::mpsc::channel();
            let (ready, started) = tokio::sync::oneshot::channel();
            let blocker = tokio::task::spawn_blocking(move || {
                ready.send(()).unwrap();
                blocked.recv().unwrap();
            });
            started.await.unwrap();
            let mut work = f
                .product
                .invoker()
                .invoke(f.call(None), CancellationToken::new());
            // Poll until the launch has been queued behind the occupied blocking worker.
            std::future::poll_fn(|cx| {
                assert!(work.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            if revoke {
                let authority = f.loader.authority().unwrap();
                authority.disable("qa").unwrap();
                authority.enable("qa").unwrap();
            } else {
                tokio::time::sleep(Duration::from_millis(30)).await;
            }
            release.send(()).unwrap();
            blocker.await.unwrap();
            reported(
                work.await.unwrap_err(),
                if revoke { "ENV020" } else { "SSH_START" },
            );
            assert!(!f.root.path().join("pid").exists());
        }
    });
}

#[test]
fn relative_ssh_paths_are_captured_from_package_base_without_reading_keys() {
    let f = Fixture::new("exit 0", 5000, "/remote/tool");
    let mut yaml = f.yaml.clone();
    yaml["targets"]["remote"]["client"] = serde_json::json!("./client");
    yaml["targets"]["remote"]["identity_file"] = serde_json::json!("identity key");
    yaml["targets"]["remote"]["known_hosts"] = serde_json::json!("hosts");
    std::fs::write(f.root.path().join("identity key"), b"synthetic key").unwrap();
    let text = yaml.to_string();
    let missing = f.loader.capture_text(&text, None, None).err().unwrap();
    assert!(missing.to_string().contains("explicit absolute base"));
    let base = f.root.path().to_str().unwrap();
    let loaded = f.loader.capture_text(&text, None, Some(base)).unwrap();
    let package = wes_core::environments::Package::parse(&loaded.yaml).unwrap();
    let TargetKind::Ssh(ssh) = package.targets()["remote"].kind() else {
        panic!()
    };
    let resolved_base = f.root.path().canonicalize().unwrap();
    assert_eq!(ssh.client, resolved_base.join("./client").to_str().unwrap());
    assert_eq!(
        ssh.identity_file,
        resolved_base.join("identity key").to_str().unwrap()
    );
    assert_eq!(
        ssh.known_hosts,
        resolved_base.join("hosts").to_str().unwrap()
    );
    assert!(f.root.path().join("identity key").exists());
    assert!(!f.root.path().join("argv").exists());
    let path = f.root.path().join("package.yaml");
    std::fs::write(&path, text).unwrap();
    let file = f.loader.capture(path.to_str().unwrap()).unwrap();
    assert_eq!(file.yaml, loaded.yaml);
}
#[test]
fn uncertain_ssh_stderr_is_bounded_and_cannot_control_the_terminal() {
    let error = reported(
        unknown_with_stderr("uncertain", &vec![0x1b; 10000]),
        "ENV036",
    );
    let text = &error.issues()[0].message;
    assert!(!text.contains('\u{1b}'));
    assert!(text.contains("truncated"));
    assert!(text.len() < 32768);
    let private = error.with_policy(&wes_core::flow::FlowPolicy::default().private());
    assert!(private.issues().is_empty());
}

#[test]
fn ssh_capture_rejects_implicit_home_expansion_and_path_lookup() {
    let f = Fixture::new("exit 0", 5000, "/remote/tool");
    for (field, path, expected) in [
        ("client", "ssh", "does not search PATH"),
        ("client", "~/bin/ssh", "do not expand"),
        ("identity_file", "~/.ssh/key", "do not expand"),
        ("known_hosts", "~/hosts", "do not expand"),
    ] {
        let mut yaml = f.yaml.clone();
        yaml["targets"]["remote"][field] = serde_json::json!(path);
        let error = f
            .loader
            .capture_text(
                &yaml.to_string(),
                None,
                Some(f.root.path().to_str().unwrap()),
            )
            .err()
            .unwrap();
        assert!(error.to_string().contains(expected), "{error}");
    }
    assert!(!f.root.path().join("argv").exists());
}

#[test]
fn ssh_plan_rejects_missing_and_non_file_inputs_without_dispatch() {
    for field in ["identity_file", "known_hosts"] {
        let f = Fixture::new("exit 0", 5000, "/remote/tool");
        let mut yaml = f.yaml.clone();
        yaml["targets"]["remote"][field] = serde_json::json!(f.root.path().join("absent"));
        let error = f
            .loader
            .capture_text(&yaml.to_string(), None, None)
            .err()
            .unwrap();
        assert!(error.message.contains("was not found"), "{error}");
        yaml["targets"]["remote"][field] = serde_json::json!(f.root.path());
        let error = f
            .loader
            .capture_text(&yaml.to_string(), None, None)
            .err()
            .unwrap();
        assert!(error.message.contains("regular file"), "{error}");
        assert!(!f.root.path().join("pid").exists());
    }
}

#[tokio::test]
async fn inputs_removed_after_plan_prevent_launch_but_not_binding_rebuild() {
    for field in ["identity_file", "known_hosts"] {
        let f = Fixture::new("exit 0", 5000, "/remote/tool");
        std::fs::remove_file(f.yaml["targets"]["remote"][field].as_str().unwrap()).unwrap();
        let rebuilt = f.loader.build("tool", &f.binding).unwrap();
        let error = reported(
            rebuilt
                .invoker()
                .invoke(f.call(None), CancellationToken::new())
                .await
                .unwrap_err(),
            "SSH_START",
        );
        assert!(error.message().contains("was not found"));
        assert!(error.message().contains("no remote command was submitted"));
        assert!(!f.root.path().join("pid").exists());
    }
}

#[test]
fn terminal_discovery_is_inert_but_open_rechecks_inputs() {
    let f = Fixture::new("exit 0", 5000, "/remote/tool");
    let target = f.binding.import().target();
    let plan = crate::execution_targets::TerminalPlan::for_target(target).unwrap();
    std::fs::remove_file(f.root.path().join("key")).unwrap();
    assert!(crate::execution_targets::terminal_support(target).is_ok());
    let error = plan
        .open(
            None,
            wes_engine::execution::TerminalSize { cols: 80, rows: 24 },
            &CancellationToken::new(),
        )
        .err()
        .unwrap();
    assert!(error.to_string().contains("identity file was not found"));
    assert!(!f.root.path().join("pid").exists());
}
