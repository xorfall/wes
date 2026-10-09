use super::*;
use crate::{
    driver::{CancellationToken, Executor},
    providers::{Call, InvocationFuture, Invoker},
    tasks::TaskExecutor,
    views::commands::CommandRequest,
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
struct NoEffects;
impl Invoker for NoEffects {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        panic!("command preparation dispatched a provider")
    }
}
fn statement(source: &str) -> Statement {
    let parsed = wes_language::parse(&wes_language::SourceText::new("synthetic", source));
    assert!(parsed.diagnostics.is_empty());
    parsed.script.statements.into_iter().next().unwrap()
}
fn add(workspace: &mut Workspace, source: &str) -> Option<NodeId> {
    let Preparation::Change(change) = workspace.prepare(&statement(source)).unwrap() else {
        panic!()
    };
    workspace.commit(change).unwrap().node
}
#[tokio::test]
async fn a_policy_change_within_one_stream_run_invalidates_a_partial_frame_command() {
    let mut workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
    let mut capability = Capability::new(
        ["effect"],
        Shape::Primitive(Primitive::Text),
        Safety::Unsafe,
    );
    capability.parameters = vec![Parameter::new(
        "value",
        Shape::Primitive(Primitive::Text),
        true,
    )];
    workspace
        .register_provider(
            ProviderDescription::new("synthetic", [capability], vec![]).unwrap(),
            Arc::new(NoEffects),
        )
        .unwrap();
    add(
        &mut workspace,
        ":def send(value:Text) as synthetic effect value:?value",
    );
    let source = add(&mut workspace, ":calc { return 0; } > source").unwrap();
    let run = workspace
        .runtime
        .start(Duration::ZERO)
        .into_iter()
        .find_map(|e| {
            if let Effect::Spawn(ticket) = e {
                Some(ticket.run)
            } else {
                None
            }
        })
        .unwrap();
    assert!(workspace.runtime.enter_stream(&run));
    let public = Value::new(
        wes_views::named("Metric").unwrap().input().shape(),
        Data::Record(
            [
                ("view".into(), Data::Text("metric".into())),
                ("value".into(), Data::Int(42)),
            ]
            .into(),
        ),
        Provenance::default(),
    )
    .unwrap();
    workspace
        .runtime
        .stream_window(&run, public.clone(), Duration::ZERO)
        .unwrap();
    let root = add(&mut workspace, ":view create Dashboard > board").unwrap();
    let child = add(&mut workspace, ":view create Metric input:$source > child").unwrap();
    let mut pending = std::collections::VecDeque::from(workspace.start(Duration::ZERO));
    while let Some(effect) = pending.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let ticket = workspace.enter_ticket(ticket).unwrap().unwrap();
            let task_run = ticket.run.clone();
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            pending.extend(workspace.complete(&task_run, report.outcome, Duration::ZERO));
        }
    }
    let root_handle = workspace
        .views
        .resolve(workspace.runtime.value_of(&root).unwrap())
        .unwrap();
    let child_handle = workspace
        .views
        .resolve(workspace.runtime.value_of(&child).unwrap())
        .unwrap();
    workspace
        .views
        .connect(&child_handle, &root_handle, None, 0)
        .unwrap();
    let frame = workspace.view_frame(&root).unwrap();
    let entry = &frame.instances[0];
    let command = workspace
        .prepare_view_command(CommandRequest {
            root: root.to_string(),
            instance: entry.identity.to_string(),
            member: root.to_string(),
            revision: entry.revision,
            input_revision: entry.input_revision,
            environments: None,
            template: "send".into(),
            arguments: [("value".into(), serde_json::json!("reviewed"))].into(),
        })
        .unwrap();
    let mut draft = workspace.draft().unwrap();
    let Preparation::Change(change) = draft.prepare(&statement(&command.source)).unwrap() else {
        panic!()
    };
    draft.stage(change).unwrap();
    let batch = draft.finish();
    let restricted = public.with_provenance(Provenance::default().with_policy(
        &wes_core::flow::FlowPolicy::default().confidential(wes_core::flow::Residence::Retainable),
    ));
    workspace
        .runtime
        .stream_window(&run, restricted, Duration::ZERO)
        .unwrap();
    assert_eq!(workspace.runtime.value_run(&source), Some(run.id()));
    let partial = workspace.view_frame(&root).unwrap();
    assert_eq!(
        frame.revisions(),
        partial.revisions(),
        "no observation occurred between these windows"
    );
    assert_ne!(frame.authority_epoch, partial.authority_epoch);
    assert!(
        partial
            .instances
            .iter()
            .find(|v| v.id == child)
            .unwrap()
            .input
            .is_none()
    );
    assert!(workspace.prepare(&statement(&command.source)).is_err());
    assert!(workspace.commit_batch(batch, Duration::ZERO).is_err());
    workspace.close();
}
