//! Saved data can feed a View without becoming a mutable or live control object.
use wes::runtime::{RuntimeOptions, launch};
use wes_engine::{session::SessionHandle, source::SourceInput};

async fn submit(session: &SessionHandle, text: &str) {
    let reply = session
        .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
        .await
        .unwrap();
    assert!(
        !reply
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error),
        "{reply:?}"
    );
    session.wait_idle().await.unwrap();
}

#[tokio::test]
async fn saved_members_are_readonly_view_inputs_before_and_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let options = || RuntimeOptions::new(root.path().join("home"), root.path().into());
    let runtime = launch(options()).await.unwrap();
    let session = runtime.handle.current().unwrap().session;
    submit(
        &session,
        ":transient { :calc pure { return {view:'metric',value:17}; } > metric } > study",
    )
    .await;
    session
        .read_sandbox_export("study.metric", false)
        .await
        .unwrap();
    submit(&session, ":keep $study.metric > saved_metric").await;
    submit(&session, ":keep $study > saved_study").await;
    for text in [
        ":refresh $saved_metric",
        ":remove $saved_study scope:downstream",
    ] {
        assert!(
            session
                .submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into()).unwrap())
                .await
                .is_err()
        );
    }
    // An unsaved child is still outside the parent's input namespace.
    let denied = session
        .submit(
            SourceInput::new(
                "unsaved-input".into(),
                ":view create Metric input:$study.metric > forbidden".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    assert!(
        denied
            .diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == wes_language::Severity::Error)
    );
    submit(&session, ":view create Metric input:$saved_metric > chart").await;
    submit(&session, ":view bind $chart input:$saved_study.metric").await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state.execution.errors.is_empty(),
        "{:?}",
        state.execution.errors
    );
    let frame = session
        .view_frame(state.names["chart"].node.clone())
        .await
        .unwrap();
    assert_eq!(
        frame.instances[0]
            .input
            .as_ref()
            .unwrap()
            .value()
            .unwrap()
            .data(),
        &wes_core::Data::Record(
            [
                ("view".into(), wes_core::Data::Text("metric".into())),
                ("value".into(), wes_core::Data::Int(17)),
            ]
            .into_iter()
            .collect()
        )
    );
    drop(session);
    runtime.shutdown().await.unwrap();

    let reopened = launch(options()).await.unwrap();
    let session = reopened.handle.current().unwrap().session;
    submit(
        &session,
        ":view create Metric input:$saved_study.metric > reopened_chart",
    )
    .await;
    let state = session.snapshot().await.unwrap();
    assert!(
        state.execution.errors.is_empty(),
        "{:?}",
        state.execution.errors
    );
    let frame = session
        .view_frame(state.names["reopened_chart"].node.clone())
        .await
        .unwrap();
    let wes_core::Data::Record(fields) = frame.instances[0]
        .input
        .as_ref()
        .unwrap()
        .value()
        .unwrap()
        .data()
    else {
        panic!("metric")
    };
    assert_eq!(fields["value"], wes_core::Data::Int(17));
    assert_eq!(
        session
            .read_sandbox("saved_metric.value", false)
            .await
            .unwrap()
            .data,
        wes_core::Data::Int(17)
    );
    drop(session);
    reopened.shutdown().await.unwrap();
}
