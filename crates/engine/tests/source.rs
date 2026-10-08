use indexmap::IndexMap;
use std::{
    collections::VecDeque,
    sync::{
        Arc, Mutex,
        atomic::{AtomicUsize, Ordering},
        mpsc,
    },
    time::Duration,
};
use tokio::sync::oneshot;
use wes_core::{
    Data, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::{CancellationToken, Executor},
    graph::NodeState,
    providers::{Call, InvocationFuture, Invoker},
    runtime::Effect,
    source::{PreparedSource, SourceError, SourceInput, SourcePreparation, prepare_declarations},
    tasks::TaskExecutor,
    type_sources::{TypeSourceCapture, TypeSourceError, TypeSourceReader},
    workspace::{Workspace, WorkspaceError},
};
use wes_language::{Severity, SourceText};
#[path = "source/documents.rs"]
mod documents;
#[path = "source/held.rs"]
mod held;
#[path = "source/imports.rs"]
mod imports;
#[path = "source/pipelines.rs"]
mod pipelines;
#[path = "source/replay.rs"]
mod replay;
#[path = "source/view_packages.rs"]
mod view_packages;

struct Echo(Arc<AtomicUsize>);
impl Invoker for Echo {
    fn invoke(&self, call: Call, _: CancellationToken) -> InvocationFuture {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move { Ok(call.arguments["value"].clone()) })
    }
}
fn workspace() -> (Workspace, Arc<AtomicUsize>) {
    let mut workspace =
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let calls = Arc::new(AtomicUsize::new(0));
    let mut echo = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
    echo.parameters = vec![Parameter::new("value", Shape::Unknown, true)];
    workspace
        .register_provider(
            ProviderDescription::new("catalog", [echo], vec![]).unwrap(),
            Arc::new(Echo(calls.clone())),
        )
        .unwrap();
    (workspace, calls)
}
struct Reader<F>(F);
impl<F: Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static> TypeSourceReader
    for Reader<F>
{
    fn read(&self, path: &str, max: usize) -> Result<String, TypeSourceError> {
        (self.0)(path, max)
    }
}
fn capture(
    f: impl Fn(&str, usize) -> Result<String, TypeSourceError> + Send + Sync + 'static,
) -> TypeSourceCapture {
    TypeSourceCapture::live(Arc::new(Reader(f)))
}
fn no_files() -> TypeSourceCapture {
    capture(|_, _| panic!("no file should be read"))
}
fn input(text: &str) -> SourceInput {
    SourceInput::new("cell-fixture".into(), text.into()).unwrap()
}
async fn plan(workspace: &Workspace, text: &str, types: TypeSourceCapture) -> PreparedSource {
    let prepared = prepare_declarations(
        input(text),
        workspace.draft().unwrap(),
        types,
        CancellationToken::new(),
    )
    .await
    .unwrap();
    let SourcePreparation::Declarations(prepared) = prepared else {
        panic!("expected declarations")
    };
    prepared
}
async fn run_all(workspace: &mut Workspace) {
    let mut effects = VecDeque::from(workspace.start(Duration::ZERO));
    while let Some(effect) = effects.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            assert!(report.notices.is_empty());
            effects.extend(workspace.complete(&run, report.outcome, Duration::ZERO));
        }
    }
    assert!(workspace.runtime().is_idle());
}

#[tokio::test]
async fn source_order_partial_acceptance_exact_utf8_slices_and_execution_are_distinct() {
    let (mut workspace, calls) = workspace();
    let text = "  :def echo as catalog echo value:?value\r\n echo value:\"é 😀\" > first\r\n missing call\r\n $first > alias\r\n catalog echo value:$alias > second  \r\n";
    let prepared = plan(&workspace, text, no_files()).await;
    assert_eq!(prepared.accepted().len(), 4);
    assert_eq!(
        prepared.nodes().map(|id| id.as_str()).collect::<Vec<_>>(),
        ["id1000", "id1001"]
    );
    assert_eq!(prepared.diagnostics().diagnostics.len(), 1);
    assert_eq!(
        prepared.diagnostics().diagnostics[0].severity,
        Severity::Error
    );
    assert!(
        prepared
            .diagnostics()
            .diagnostics
            .iter()
            .all(|d| d.code != "TMP000")
    );
    let record = prepared.record().unwrap();
    assert_eq!(record.text, text);
    assert_eq!(record.cell, "cell-fixture");
    assert_eq!(
        record.replay,
        ":def echo as catalog echo value:?value\necho value:\"é 😀\" > first\n$first > alias\ncatalog echo value:$alias > second"
    );
    let source = SourceText::new("fixture", text);
    for span in prepared.accepted() {
        assert!(source.slice(*span).is_ok());
        assert!(source.position(span.end()).is_ok());
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(workspace.runtime().graph().is_empty());
    let applied = prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert_eq!(applied.changes[0].diagnostics[0].code, "TMP000");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    assert!(
        workspace
            .runtime()
            .graph()
            .nodes()
            .all(|n| n.state() == NodeState::Pending)
    );
    run_all(&mut workspace).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    let second = workspace.resolve("second").unwrap();
    assert_eq!(
        workspace.runtime().value_of(&second.node).unwrap().data(),
        &Data::Text("é 😀".into())
    );
}

#[tokio::test]
async fn syntax_and_mixed_immediate_actions_reject_whole_source_before_type_reads() {
    let (workspace, calls) = workspace();
    for text in [
        ":package load path:test.yaml\ncatalog echo value:\"unclosed",
        ":package load path:test.yaml\n:wait $missing",
        ":cancel $missing\ncatalog echo value:ok",
        "catalog echo value:ok\nworkspace save \"name\"",
    ] {
        let SourcePreparation::Rejected {
            input: original,
            diagnostics,
        } = prepare_declarations(
            input(text),
            workspace.draft().unwrap(),
            no_files(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        else {
            panic!("reject before preparation")
        };
        assert_eq!(original.text(), text);
        assert!(!diagnostics.diagnostics.is_empty());
        if !text.contains("unclosed") {
            assert_eq!(diagnostics.diagnostics[0].code, "ENG005");
        }
    }
    assert!(workspace.runtime().graph().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn single_nonrecorded_actions_are_returned_for_live_dispatch_not_applied_to_a_draft() {
    let workspace = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    for text in [
        ":wait $later",
        ":refresh $later",
        ":cancel $later",
        ":workspace save \"named\"",
        ":workspace load \"named\"",
    ] {
        let SourcePreparation::Immediate {
            input: original,
            statement,
        } = prepare_declarations(
            input(text),
            workspace.draft().unwrap(),
            no_files(),
            CancellationToken::new(),
        )
        .await
        .unwrap()
        else {
            panic!("immediate")
        };
        assert_eq!(original.text(), text);
        assert_eq!(&text[statement.span.start()..statement.span.end()], text);
    }
}

#[tokio::test]
async fn successful_source_keeps_original_whitespace_and_empty_or_rejected_sources_have_no_record()
{
    let (workspace, _) = workspace();
    let text = "\r\n  catalog echo value:ok > result   \r\n\r\n";
    let prepared = plan(&workspace, text, no_files()).await;
    assert!(prepared.diagnostics().diagnostics.is_empty());
    assert_eq!(prepared.record().unwrap().replay, text);
    for text in ["", "  \r\n\n", "missing call"] {
        let prepared = plan(&workspace, text, no_files()).await;
        assert!(prepared.record().is_none());
        assert!(prepared.accepted().is_empty());
        assert_eq!(prepared.nodes().count(), 0);
    }
}

#[tokio::test]
async fn captured_type_loads_promote_only_successful_sources_and_reconstruct_without_live_files() {
    let (mut workspace, calls) = workspace();
    let reads = Arc::new(Mutex::new(Vec::new()));
    let observed = reads.clone();
    let yaml = "types: {Category: {base: Text, enum: [books]}}\n";
    let text = ":package load path:bad.yaml\n:package load path:good.yaml\n:package load path:good.yaml\n:def books-in(section: Category) as catalog echo value:?section\nbooks-in section:games > bad\nbooks-in section:books > chosen";
    let prepared = plan(
        &workspace,
        text,
        capture(move |path, _| {
            observed.lock().unwrap().push(path.to_owned());
            Ok(if path == "good.yaml" {
                yaml
            } else {
                "types: {Broken: {base: Absent}}"
            }
            .into())
        }),
    )
    .await;
    let record = prepared.record().unwrap().clone();
    assert_eq!(*reads.lock().unwrap(), ["bad.yaml", "good.yaml"]);
    assert_eq!(
        record.type_sources,
        IndexMap::from([("good.yaml".to_owned(), yaml.to_owned())])
    );
    assert!(!record.replay.contains("bad.yaml"));
    assert!(!record.replay.contains("games"));
    assert_eq!(prepared.diagnostics().issues.len(), 1);
    assert!(!prepared.diagnostics().issues[0].issues.is_empty());
    assert_eq!(record.nodes.len(), 1);
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert!(workspace.contracts().resolve("Category").is_ok());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let (mut restored, restored_calls) = self::workspace();
    let replay = plan(
        &restored,
        &record.replay,
        TypeSourceCapture::replay(record.type_sources).unwrap(),
    )
    .await;
    assert!(replay.diagnostics().diagnostics.is_empty());
    replay.commit(&mut restored, Duration::ZERO).unwrap();
    assert!(restored.contracts().resolve("Category").is_ok());
    assert_eq!(restored_calls.load(Ordering::SeqCst), 0);
    // This is effect-free declaration reconstruction, not full forced-ID/held session restoration.
    assert_eq!(workspace.resolve("chosen"), restored.resolve("chosen"));
}

#[tokio::test]
async fn read_failures_remain_statement_diagnostics_and_do_not_hide_later_success() {
    let workspace = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    for error in [
        TypeSourceError::Unavailable,
        TypeSourceError::InvalidText,
        TypeSourceError::TooLarge,
        TypeSourceError::MissingSnapshot,
    ] {
        let prepared = plan(
            &workspace,
            ":package load path:private.yaml\n:type check \"7\" as:Int > checked",
            capture(move |_, _| Err(error)),
        )
        .await;
        assert_eq!(prepared.diagnostics().diagnostics[0].code, "TYP007");
        assert!(
            !prepared.diagnostics().diagnostics[0]
                .message
                .contains("private.yaml")
        );
        assert_eq!(prepared.accepted().len(), 1);
        assert!(prepared.record().unwrap().type_sources.is_empty());
    }
}

#[tokio::test]
async fn cancellation_joins_entered_reading_and_discards_all_staged_declarations() {
    let (workspace, calls) = workspace();
    let (entered, entered_rx) = oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let capture = capture(move |_, _| {
        entered.lock().unwrap().take().unwrap().send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
        Ok("types: {}".into())
    });
    let token = CancellationToken::new();
    let pending = tokio::spawn(prepare_declarations(
        input("catalog echo value:ok > first\n:package load path:blocked.yaml"),
        workspace.draft().unwrap(),
        capture,
        token.clone(),
    ));
    tokio::time::timeout(Duration::from_secs(5), entered_rx)
        .await
        .unwrap()
        .unwrap();
    token.cancel();
    tokio::task::yield_now().await;
    assert!(!pending.is_finished());
    assert!(workspace.resolve("first").is_none());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    assert!(matches!(
        pending.await.unwrap(),
        Err(SourceError::Cancelled)
    ));
    let prepared = plan(&workspace, "catalog echo value:new > first", no_files()).await;
    assert_eq!(prepared.nodes().next().unwrap().as_str(), "id1000");
}

#[tokio::test]
async fn prepared_source_cannot_commit_over_a_competing_structural_change() {
    let (mut workspace, _) = workspace();
    let prepared = plan(&workspace, "catalog echo value:old > old", no_files()).await;
    plan(&workspace, "catalog echo value:new > new", no_files())
        .await
        .commit(&mut workspace, Duration::ZERO)
        .unwrap();
    assert!(matches!(
        prepared.commit(&mut workspace, Duration::ZERO),
        Err(WorkspaceError::Obsolete)
    ));
    assert!(workspace.resolve("old").is_none());
}

#[tokio::test]
async fn limits_pre_cancel_and_recorded_workspace_queries_are_explicit() {
    assert!(matches!(
        SourceInput::new(" ".into(), String::new()),
        Err(SourceError::Identity)
    ));
    assert!(matches!(
        SourceInput::new("a\nb".into(), String::new()),
        Err(SourceError::Identity)
    ));
    assert!(matches!(
        SourceInput::new("x".repeat(257), String::new()),
        Err(SourceError::Identity)
    ));
    assert!(matches!(
        SourceInput::new("cell".into(), "é".repeat(524289)),
        Err(SourceError::Capacity)
    ));
    let workspace = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap());
    let source = input(&":type check \"ok\" as:Text\n".repeat(1001));
    assert!(matches!(
        prepare_declarations(
            source,
            workspace.draft().unwrap(),
            no_files(),
            CancellationToken::new()
        )
        .await,
        Err(SourceError::Capacity)
    ));
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        prepare_declarations(
            input(":package load path:no.yaml"),
            workspace.draft().unwrap(),
            no_files(),
            token
        )
        .await,
        Err(SourceError::Cancelled)
    ));
    let prepared = plan(
        &workspace,
        ":list workspaces\n:type check \"ok\" as:Text",
        no_files(),
    )
    .await;
    assert!(prepared.diagnostics().diagnostics.is_empty());
    assert_eq!(prepared.accepted().len(), 2);
    assert!(
        prepared
            .record()
            .unwrap()
            .replay
            .contains(":list workspaces")
    );
    assert!(!format!("{:?}", input("private-source")).contains("private-source"));
}

#[tokio::test]
async fn invalid_recorded_controls_leave_later_declarations_and_exact_replay_intact() {
    let (mut workspace, calls) = workspace();
    let text = "catalog echo value:hello > first\n:timeout $first after:PT0S\n:policy $first mode:invalid\n:name unbind \"absent\"\n:change $first value:$missing\n$first > alias\n:name unbind \"alias\"\ncatalog echo value:$first > second";
    let prepared = plan(&workspace, text, no_files()).await;
    assert_eq!(prepared.accepted().len(), 4);
    assert_eq!(prepared.diagnostics().diagnostics.len(), 4);
    assert_eq!(prepared.unbound(), ["alias"]);
    assert!(prepared.removed().is_empty());
    assert_eq!(
        prepared.record().unwrap().replay,
        "catalog echo value:hello > first\n$first > alias\n:name unbind \"alias\"\ncatalog echo value:$first > second"
    );
    assert!(workspace.runtime().graph().is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    let applied = prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    assert!(applied.effects.is_empty());
    assert_eq!(applied.unbound, ["alias"]);
    assert!(workspace.resolve("alias").is_none());
    run_all(&mut workspace).await;
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn command_mutation_evidence_uses_resolved_accepted_targets_and_replay_checks_it() {
    let (workspace, calls) = workspace();
    let text = "catalog echo value:one > first\ncatalog echo value:two > second\n$first > alias\n:change $second value:changed\n:change $alias value:changed\n:change $first value:again\n:change $missing value:rejected";
    let prepared = plan(&workspace, text, no_files()).await;
    let record = prepared.record().unwrap().clone();
    assert_eq!(
        record.changed_nodes,
        vec![record.nodes[1].clone(), record.nodes[0].clone()]
    );
    assert!(!record.replay.contains("$missing"));
    assert!(!prepared.diagnostics().diagnostics.is_empty());
    wes_engine::source::prepare_replay(
        &record,
        workspace.draft().unwrap(),
        CancellationToken::new(),
    )
    .await
    .unwrap();
    for targets in [
        vec![],
        vec![record.nodes[0].clone()],
        vec![record.nodes[0].clone(), record.nodes[1].clone()],
    ] {
        let mut tampered = record.clone();
        tampered.changed_nodes = targets;
        assert!(matches!(
            wes_engine::source::prepare_replay(
                &tampered,
                workspace.draft().unwrap(),
                CancellationToken::new()
            )
            .await,
            Err(SourceError::ReplayMismatch)
        ));
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn recognizable_credential_literals_warn_even_when_syntax_is_rejected() {
    let text = ":calc { return 'Bearer synthetic-token'";
    let SourcePreparation::Rejected { diagnostics, .. } =
        wes_engine::source::preflight(input(text)).unwrap()
    else {
        panic!("invalid syntax");
    };
    let warnings: Vec<_> = diagnostics
        .diagnostics
        .iter()
        .filter(|d| d.code == "SEC001")
        .collect();
    assert_eq!(warnings.len(), 1);
    assert_eq!(warnings[0].severity, Severity::Warning);
    assert!(!format!("{:?}", warnings[0]).contains("synthetic-token"));
}

#[tokio::test]
async fn source_warning_is_shared_and_does_not_rewrite_or_block_the_program() {
    let (workspace, _) = workspace();
    let source = ":calc { return 'Bearer synthetic-token'; } > response";
    let prepared = plan(&workspace, source, no_files()).await;
    let warnings: Vec<_> = prepared
        .diagnostics()
        .diagnostics
        .iter()
        .filter(|d| d.code == "SEC001")
        .collect();
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].message.contains("does not redact"));
    assert!(warnings[0].hints[0].contains("--credentials-stdin"));
    assert!(!format!("{:?}", warnings[0]).contains("synthetic-token"));
    assert_eq!(prepared.accepted().len(), 1);
    assert_eq!(prepared.record().unwrap().text, source);
    for ordinary in [
        ":calc { return 'normal text'; }",
        ":calc { return $token; }",
        ":calc { return 'Authorization'; }",
    ] {
        let prepared = plan(&workspace, ordinary, no_files()).await;
        assert!(
            !prepared
                .diagnostics()
                .diagnostics
                .iter()
                .any(|d| d.code == "SEC001")
        );
    }
}

#[tokio::test]
async fn whole_argument_structures_preserve_template_and_calculation_parameter_contracts() {
    let (mut workspace, calls) = workspace();
    let yaml = "types: {Body: {base: Record, fields: {count: Int}}}";
    let text = ":package load path:\"body.yaml\"\n:def send(body:Body) as catalog echo value:?body\n:def identity(input:Body) -> Body as :calc {return input;}\nsend body:{count:\"7\"} > sent\nidentity input:{count:8} > calculated\n:type check $calculated as:Body > checked";
    let prepared = plan(&workspace, text, capture(move |_, _| Ok(yaml.into()))).await;
    assert_eq!(prepared.accepted().len(), 6, "{:?}", prepared.diagnostics());
    prepared.commit(&mut workspace, Duration::ZERO).unwrap();
    run_all(&mut workspace).await;
    for (name, count) in [("sent", 7), ("calculated", 8), ("checked", 8)] {
        let node = workspace.resolve(name).unwrap().node;
        assert_eq!(
            workspace.runtime().value_of(&node).unwrap().data(),
            &Data::Record([("count".into(), Data::Int(count))].into())
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}
