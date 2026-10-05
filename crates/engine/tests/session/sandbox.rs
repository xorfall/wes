use super::*;
use wes_engine::session::sandbox::{Definition, DefinitionStore};

#[derive(Default)]
struct Definitions {
    saved: Mutex<Vec<Definition>>,
    writes: AtomicUsize,
    fail: bool,
}
impl DefinitionStore for Definitions {
    fn load(&self) -> Result<Vec<Definition>, String> {
        Ok(self.saved.lock().unwrap().clone())
    }
    fn save(&self, definitions: &[Definition]) -> Result<(), String> {
        if self.fail {
            return Err("synthetic store refusal".into());
        }
        *self.saved.lock().unwrap() = definitions.to_vec();
        self.writes.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }
}
async fn value(handle: &SessionHandle, name: &str) -> Data {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Ok(reply) = handle.read_sandbox(name, false).await {
                return reply.data;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}
#[tokio::test]
async fn definitions_restore_but_runs_cells_values_and_grants_do_not() {
    let store = Arc::new(Definitions::default());
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let made = submit(&handle,"sandbox-create",":sandbox {\n:calc { return call('catalog',['echo'],{value:7}); } > first\n$first | :calc { return input + 1; } > second\n} > preview").await;
    assert!(made.sandbox.is_some());
    assert!(made.nodes.is_empty());
    assert_eq!(value(&handle, "preview.second").await, Data::Int(8));
    assert!(handle.observe().await.unwrap().cells.is_empty());
    assert!(handle.snapshot().await.unwrap().execution.graph.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let inspected = submit(&handle, "inspect", ":inspect $preview").await;
    assert!(format!("{:?}", inspected.sandbox.as_ref().unwrap().data).contains("Int"));
    for i in 0..1000 {
        submit(&handle, &format!("refresh-{i}"), ":refresh $preview").await;
        assert_eq!(value(&handle, "preview.second").await, Data::Int(8));
    }
    assert_eq!(store.writes.load(Ordering::SeqCst), 1);
    assert!(handle.observe().await.unwrap().cells.is_empty());
    stop(handle, task).await;
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let inspected = handle.read_sandbox("preview", true).await.unwrap();
    assert!(format!("{:?}", inspected.data).contains("not run"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(handle.read_sandbox("preview.second", false).await.is_err());
    submit(&handle, "start-restored", ":refresh $preview").await;
    assert_eq!(value(&handle, "preview.second").await, Data::Int(8));
    stop(handle, task).await;
}
#[tokio::test]
async fn sandbox_cannot_overwrite_parent_names_or_persist_through_host_controls() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(&handle, "ordinary", ":calc { return 11; } > taken").await;
    assert!(
        handle
            .submit(input(
                "collision",
                ":sandbox { :calc { return 1; } > local } > taken"
            ))
            .await
            .is_err()
    );
    for (i, source) in [
        ":sandbox { :workspace save \"bad\" } > bad",
        ":sandbox { :env disable \"default\" } > bad",
        ":sandbox { :describe url:\"http://127.0.0.1\" } > bad",
    ]
    .iter()
    .enumerate()
    {
        assert!(
            handle
                .submit(input(&format!("bad-{i}"), source))
                .await
                .is_err()
        );
    }
    submit(
        &handle,
        "create",
        ":sandbox { :calc { return 42; } > taken } > preview",
    )
    .await;
    assert_eq!(value(&handle, "preview.taken").await, Data::Int(42));
    let collision = submit(&handle, "collision-parent", ":calc { return 2; } > preview").await;
    assert!(
        collision
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    let export = submit(&handle, "export", ":calc { return $preview.taken; } > copy").await;
    assert!(
        export
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    stop(handle, task).await;
}
#[tokio::test]
async fn failed_definition_write_never_enters_provider_and_duplicates_do_not_repeat_effects() {
    let store = Arc::new(Definitions {
        fail: true,
        ..Default::default()
    });
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    assert!(
        handle
            .submit(input(
                "failed",
                ":sandbox { catalog echo value:1 > result } > preview"
            ))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let source = ":sandbox { catalog echo value:1 > result } > preview";
    submit(&handle, "same", source).await;
    value(&handle, "preview.result").await;
    submit(&handle, "same", source).await;
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(matches!(
        handle.submit(input("same", ":refresh $preview")).await,
        Err(SessionError::Conflict)
    ));
    stop(handle, task).await;
}
#[tokio::test]
async fn cooperative_sandbox_authority_is_not_inherited_from_another_creator() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "mine",
        ":sandbox { :calc { return 1; } > value } > preview",
    )
    .await;
    let request = input("other", ":refresh $preview")
        .with_client("other-actor".into())
        .unwrap()
        .cooperative();
    assert!(matches!(
        handle.submit(request).await,
        Err(SessionError::Authority)
    ));
    stop(handle, task).await;
}
struct PrivateValue;
impl Invoker for PrivateValue {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        Box::pin(async {
            Ok(wes_core::Value::new(
                Shape::Primitive(wes_core::Primitive::Text),
                Data::Text("synthetic-private-marker".into()),
                wes_core::Provenance::default()
                    .with_policy(&wes_core::flow::FlowPolicy::default().private()),
            )
            .unwrap())
        })
    }
}
#[tokio::test]
async fn private_members_stay_withheld_from_cooperative_read_and_inspect() {
    let mut base = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    base.register_provider(
        ProviderDescription::new(
            "fixture",
            [Capability::new(
                ["read"],
                Shape::Primitive(wes_core::Primitive::Text),
                Safety::Safe,
            )],
            vec![],
        )
        .unwrap(),
        Arc::new(PrivateValue),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "private-create",
        ":sandbox { fixture read > value } > preview",
    )
    .await;
    assert_eq!(
        value(&handle, "preview.value").await,
        Data::Text("synthetic-private-marker".into())
    );
    let read = input("agent-read", ":read $preview.value")
        .with_client("agent".into())
        .unwrap()
        .cooperative();
    assert!(matches!(
        handle.submit(read).await,
        Err(SessionError::Authority)
    ));
    let inspect = input("agent-inspect", ":inspect $preview")
        .with_client("agent".into())
        .unwrap()
        .cooperative();
    let reply = handle.submit(inspect).await.unwrap();
    let shown = format!("{:?}", reply.sandbox.as_ref().unwrap().data);
    assert!(shown.contains("withheld"));
    assert!(!shown.contains("synthetic-private-marker"));
    let exported = handle.read_sandbox_export("preview", false).await.unwrap();
    assert!(format!("{:?}", exported.data).contains("withheld"));
    assert!(!format!("{:?}", exported.data).contains("synthetic-private-marker"));
    assert!(matches!(
        handle.read_sandbox_export("preview.value", false).await,
        Err(SessionError::Authority)
    ));
    stop(handle, task).await;
}
#[tokio::test]
async fn redefining_is_joined_and_mixed_scripts_do_not_partially_execute() {
    let (base, calls) = workspace(None, None);
    let store = Arc::new(Definitions::default());
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    assert!(
        handle
            .submit(input(
                "mixed",
                ":sandbox { catalog echo value:1 > result } > preview\ncatalog echo value:2"
            ))
            .await
            .is_err()
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    submit(
        &handle,
        "one",
        ":sandbox { :calc { return {amount:1}; } > value } > preview",
    )
    .await;
    assert_eq!(value(&handle, "preview.value.amount").await, Data::Int(1));
    submit(
        &handle,
        "two",
        ":sandbox { :calc { return {amount:2}; } > value } > preview",
    )
    .await;
    assert_eq!(value(&handle, "preview.value.amount").await, Data::Int(2));
    assert_eq!(store.saved.lock().unwrap().len(), 1);
    assert_eq!(store.writes.load(Ordering::SeqCst), 2);
    stop(handle, task).await;
}
#[tokio::test]
async fn actual_dashboard_example_and_annotation_expose_named_members() {
    let (handle, task) = session::spawn(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "example",
        include_str!("../../../../examples/sandbox/metrics.wes"),
    )
    .await;
    assert_eq!(value(&handle, "preview.requests").await, Data::Int(25));
    submit(
        &handle,
        "short",
        "@sandbox :calc { return 2; } | :calc { return input * 3; } > quick",
    )
    .await;
    assert_eq!(value(&handle, "quick.result").await, Data::Int(6));
    stop(handle, task).await;
}
#[tokio::test]
async fn recorded_parent_and_agent_request_ledger_receive_no_sandbox_execution_records() {
    let (base, calls) = workspace(None, None);
    let records = Arc::new(Mutex::new(vec![]));
    let (recorder, writer) = spawn_recorder(
        LogSink {
            records: records.clone(),
            gate: None,
            fail_observation: false,
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let result = handle
        .submit_request(
            "agent".into(),
            "one".into(),
            "context".into(),
            true,
            input(
                "create",
                ":sandbox { catalog echo value:5 > value } > preview",
            )
            .with_client("agent".into())
            .unwrap()
            .cooperative(),
        )
        .await
        .unwrap();
    assert!(result.submission.unwrap().unwrap().sandbox.is_some());
    value(&handle, "preview.value").await;
    let repeated = handle
        .submit_request(
            "agent".into(),
            "one".into(),
            "context".into(),
            true,
            input(
                "another-cell",
                ":sandbox { catalog echo value:5 > value } > preview",
            )
            .with_client("agent".into())
            .unwrap()
            .cooperative(),
        )
        .await
        .unwrap();
    assert!(!repeated.claim.fresh);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(records.lock().unwrap().is_empty());
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
    assert!(records.lock().unwrap().is_empty());
}

struct LiveEvents {
    active: Arc<AtomicUsize>,
    starts: Arc<AtomicUsize>,
}
impl wes_engine::streams::StreamingInvoker for LiveEvents {
    fn subscribe(
        &self,
        _: Call,
        sink: wes_engine::streams::StreamSink,
        cancel: CancellationToken,
    ) -> wes_engine::streams::StreamFuture {
        assert_eq!(
            self.active.fetch_add(1, Ordering::SeqCst),
            0,
            "subscriptions overlap"
        );
        let active = self.active.clone();
        let n = self.starts.fetch_add(1, Ordering::SeqCst) + 1;
        Box::pin(async move {
            struct Guard(Arc<AtomicUsize>);
            impl Drop for Guard {
                fn drop(&mut self) {
                    self.0.fetch_sub(1, Ordering::SeqCst);
                }
            }
            let _guard = Guard(active);
            sink.push(
                wes_core::Value::new(
                    Shape::Primitive(wes_core::Primitive::Int),
                    Data::Int(n as i64),
                    wes_core::Provenance::default(),
                )
                .unwrap(),
            )
            .unwrap();
            sink.opened().unwrap();
            cancel.cancelled().await;
            Ok(())
        })
    }
}
#[tokio::test]
async fn refresh_joins_stream_and_starts_a_fresh_accumulator_without_overlapping() {
    let (mut base, _) = workspace(None, None);
    let active = Arc::new(AtomicUsize::new(0));
    let starts = Arc::new(AtomicUsize::new(0));
    let mut cap = Capability::new(
        ["watch"],
        Shape::Primitive(wes_core::Primitive::Int),
        Safety::Safe,
    );
    cap.streaming = true;
    base.register_provider_ports(
        ProviderDescription::new("events", [cap], vec![]).unwrap(),
        Arc::new(Echo {
            calls: Arc::new(AtomicUsize::new(0)),
            gate: None,
            entered: Mutex::new(None),
        }),
        Some(Arc::new(LiveEvents {
            active: active.clone(),
            starts: starts.clone(),
        })),
    )
    .unwrap();
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "create",
        ":sandbox { events watch | :accumulate limit:5 > recent } > preview",
    )
    .await;
    for n in 1..=10 {
        let result = value(&handle, "preview.recent").await;
        let Data::Record(fields) = result else {
            panic!("accumulator");
        };
        assert_eq!(fields["items"], Data::List(vec![Data::Int(n)]));
        if n < 10 {
            submit(&handle, &format!("refresh-{n}"), ":refresh $preview").await;
        }
    }
    submit(&handle, "stop", ":cancel $preview").await;
    assert_eq!(active.load(Ordering::SeqCst), 0);
    assert_eq!(starts.load(Ordering::SeqCst), 10);
    stop(handle, task).await;
}

#[tokio::test]
async fn removal_is_owned_joined_durable_and_failed_definitions_release_names() {
    let store = Arc::new(Definitions::default());
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store.clone()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "create",
        ":sandbox { catalog echo value:7 > result } > preview",
    )
    .await;
    assert_eq!(
        value(&handle, "preview.result").await,
        Data::Text("7".into())
    );
    let foreign = input("foreign", ":remove $preview scope:downstream")
        .with_client("another".into())
        .unwrap()
        .cooperative();
    assert!(matches!(
        handle.submit(foreign).await,
        Err(SessionError::Authority)
    ));
    assert_eq!(store.saved.lock().unwrap().len(), 1);
    let missing_scope = handle
        .submit(input("scope", ":remove $preview"))
        .await
        .unwrap_err();
    assert!(missing_scope.to_string().contains("scope:downstream"));
    let removal = submit(&handle, "remove", ":remove $preview scope:downstream").await;
    assert!(format!("{:?}", removal.sandbox.as_ref().unwrap().data).contains("removed"));
    let duplicate = submit(&handle, "remove", ":remove $preview scope:downstream").await;
    assert_eq!(
        duplicate.sandbox.as_ref().unwrap().data,
        removal.sandbox.as_ref().unwrap().data
    );
    assert_eq!(store.writes.load(Ordering::SeqCst), 2);
    assert!(store.saved.lock().unwrap().is_empty());
    assert!(handle.sandbox_lifecycle().await.unwrap().running.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(handle.read_sandbox("preview", false).await.is_err());
    assert!(
        handle
            .submit(input(
                "bad",
                ":sandbox { :calc { return $outside; } > value } > reusable"
            ))
            .await
            .is_err()
    );
    let ordinary = submit(&handle, "ordinary", ":calc { return 1; } > reusable").await;
    assert!(
        !ordinary
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    stop(handle, task).await;
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base.with_sandbox_store(store),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let listed = submit(&handle, "list", ":list sandboxes").await;
    handle.wait_idle().await.unwrap();
    assert_eq!(
        handle.snapshot().await.unwrap().execution.values[&listed.nodes[0]].data(),
        &Data::List(vec![])
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    stop(handle, task).await;
}
#[tokio::test]
async fn sandbox_errors_have_readable_source_and_scope_without_parent_data() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let refusal = handle
        .submit(input(
            "undefined",
            ":sandbox { :calc { return $outside; } > value } > preview",
        ))
        .await
        .unwrap_err();
    assert!(matches!(
        refusal,
        session::SessionError::AdmissionRefused(_)
    ));
    let error = refusal.to_string();
    assert!(error.contains("CAL010"));
    assert!(error.contains("own declared members"));
    assert!(error.contains("$outside"));
    let local = handle
        .submit(input(
            "local-undefined",
            ":sandbox { :calc { return missing; } > value } > local_preview",
        ))
        .await
        .unwrap_err()
        .to_string();
    assert!(local.contains("CAL010"));
    assert!(!local.contains("parent workspace"));
    submit(
        &handle,
        "failing",
        ":sandbox { :calc { return 1 / 0; } > value } > preview",
    )
    .await;
    let shown = handle.read_sandbox_export("preview", false).await.unwrap();
    let text = format!("{:?}", shown.data);
    assert!(text.contains("sandbox $preview"), "{text}");
    assert!(!text.contains("cell "), "{text}");
    submit(&handle, "cancel", ":cancel $preview").await;
    assert!(
        format!(
            "{:?}",
            handle.read_sandbox("preview", true).await.unwrap().data
        )
        .contains("stopped")
    );
    stop(handle, task).await;
}

#[tokio::test]
async fn member_data_cannot_override_sandbox_control_state() {
    let (base, _) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    submit(
        &handle,
        "create-spoof",
        ":sandbox { :calc { return {kind:'Sandbox', state:'stopped'}; } > value } > preview",
    )
    .await;
    let observed = handle
        .read_sandbox_export("preview.value", false)
        .await
        .unwrap();
    assert_eq!(observed.state.as_str(), "active");
    assert!(format!("{:?}", observed.data).contains("stopped"));
    stop(handle, task).await;
}
