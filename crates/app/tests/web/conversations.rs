use super::*;
use tokio::sync::mpsc;
use wes_core::{Data, Primitive, Provenance};
use wes_engine::{
    conversations::{Channel, ConversationIo, Input, InteractiveInvoker, OutputSink},
    providers::InvocationError,
    runtime::Run,
};

struct Dialogue {
    entered: mpsc::UnboundedSender<(Run, OutputSink)>,
    received: mpsc::UnboundedSender<Vec<u8>>,
    calls: Arc<AtomicUsize>,
}
impl InteractiveInvoker for Dialogue {
    fn start(
        &self,
        call: Call,
        mut io: ConversationIo,
        token: CancellationToken,
    ) -> InvocationFuture {
        self.calls.fetch_add(1, Ordering::SeqCst);
        io.output.write(Channel::Stdout, b"question: ").unwrap();
        io.output.opened().unwrap();
        self.entered.send((call.run, io.output.clone())).unwrap();
        let received = self.received.clone();
        Box::pin(async move {
            loop {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => return Err(InvocationError::Cancelled),
                    input = io.receive() => match input {
                        Some(Input::Bytes(bytes)) => { received.send(bytes).unwrap(); },
                        Some(Input::Eof) | None => break,
                    }
                }
            }
            Ok(wes_core::Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("done".into()),
                Provenance::default(),
            )
            .unwrap())
        })
    }
}
async fn active(events: &mut Events) -> Value {
    loop {
        let frame = events.until("conversation").await;
        if frame["active"] == true {
            return frame;
        }
    }
}
#[tokio::test]
async fn browser_dialogue_routes_run_bound_input_eof_and_transient_output_across_reconnect_and_refresh()
 {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let (received, mut inputs) = mpsc::unbounded_channel();
    let calls = Arc::new(AtomicUsize::new(0));
    let invoked = calls.clone();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            workspace.register_provider_all_ports(
                ProviderDescription::new(
                    "dialog",
                    [Capability::new(
                        ["ask"],
                        Shape::Primitive(Primitive::Text),
                        Safety::Unsafe,
                    )],
                    vec![],
                )
                .unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                None,
                Some(Arc::new(Dialogue {
                    entered: entered.clone(),
                    received: received.clone(),
                    calls: invoked.clone(),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "@interactive dialog ask > talking";
    assert_eq!(fixture.source(&generation, "dialogue", source).await, 202);
    let (run, sink) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(events.until("created").await["interactive"], true);
    let conversation = active(&mut events).await;
    assert_eq!(conversation["run"], run.id().as_str());
    assert_eq!(events.until("output").await["text"], "question: ");
    let input = |id: &str, text: &str| json!({"request":"input", "node":run.node().as_str(), "run":id, "text":text});
    assert_eq!(
        fixture
            .post(&generation, input("stale-run", "wrong"))
            .await
            .status(),
        409
    );
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"input", "node":run.node().as_str(), "text":"missing-run"})
            )
            .await
            .status(),
        400
    );
    assert_eq!(
        fixture
            .post(&generation, input(run.id().as_str(), &"x".repeat(65537)))
            .await
            .status(),
        413
    );
    assert!(inputs.try_recv().is_err());
    assert_eq!(
        fixture
            .post(&generation, input(run.id().as_str(), "private-answer\n"))
            .await
            .status(),
        202
    );
    assert_eq!(inputs.recv().await.unwrap(), b"private-answer\n");
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(active(&mut reconnected).await["run"], run.id().as_str());
    reconnected.until("output-gap").await;
    assert_eq!(fixture.source(&generation, "dialogue", source).await, 202);
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    sink.write(Channel::Stdout, &[0xe2, 0x82]).unwrap();
    sink.write(Channel::Stdout, &[0xac]).unwrap();
    assert_eq!(events.until("output").await["text"], "€");
    assert_eq!(reconnected.until("output").await["text"], "€");
    let eof =
        |run: &Run| json!({"request":"eof", "node":run.node().as_str(), "run":run.id().as_str()});
    assert_eq!(fixture.post(&generation, eof(&run)).await.status(), 202);
    let ready = events.until("ready").await;
    assert_eq!(ready["kept"], false);
    let result = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(result["data"], "done");
    assert_eq!(
        fixture
            .post(&generation, input(run.id().as_str(), "too late"))
            .await
            .status(),
        409
    );
    assert_eq!(
        fixture
            .source(&generation, "refresh", ":refresh $talking")
            .await,
        202
    );
    let (replacement, _) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    assert_ne!(replacement.id(), run.id());
    assert_eq!(active(&mut events).await["run"], replacement.id().as_str());
    assert_eq!(
        fixture
            .post(&generation, input(run.id().as_str(), "old answer"))
            .await
            .status(),
        409
    );
    assert!(sink.write(Channel::Stdout, b"old output").is_err());
    assert_eq!(
        fixture.post(&generation, eof(&replacement)).await.status(),
        202
    );
    let refreshed = events.until("ready").await;
    assert_ne!(refreshed["handle"], ready["handle"]);
    assert_eq!(refreshed["kept"], false);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert!(inputs.try_recv().is_err());
    drop(events);
    drop(reconnected);
    fixture.close().await;
}

#[tokio::test]
async fn console_default_records_interactive_semantics_and_keeps_run_bound_input() {
    let (entered, mut arrivals) = mpsc::unbounded_channel();
    let (received, mut inputs) = mpsc::unbounded_channel();
    let fixture = Fixture::configured(
        |_| wes::web::Services::default(),
        Arc::new(move |workspace| {
            workspace.register_provider_all_ports(
                ProviderDescription::new(
                    "dialog",
                    [Capability::new(
                        ["ask"],
                        Shape::Primitive(Primitive::Text),
                        Safety::Unsafe,
                    )],
                    vec![],
                )
                .unwrap(),
                Arc::new(Echo(Arc::new(AtomicUsize::new(0)))),
                None,
                Some(Arc::new(Dialogue {
                    entered: entered.clone(),
                    received: received.clone(),
                    calls: Arc::new(AtomicUsize::new(0)),
                })),
            )?;
            Ok(())
        }),
    )
    .await;
    let generation = fixture.app.current().unwrap().generation;
    let response = fixture.post(&generation, json!({"request":"submit","client":"web-fixture", "cell":"console-call", "text":"dialog ask > answer", "console":true})).await;
    assert_eq!(response.status(), 202);
    let (run, _) = tokio::time::timeout(Duration::from_secs(3), arrivals.recv())
        .await
        .unwrap()
        .unwrap();
    let current = fixture.app.current().unwrap();
    let observation = current.session.observe().await.unwrap();
    assert_eq!(
        observation.cells.last().unwrap().input.text(),
        "@interactive dialog ask > answer"
    );
    let node = observation
        .cells
        .last()
        .unwrap()
        .reply
        .as_ref()
        .unwrap()
        .as_ref()
        .unwrap()
        .nodes[0]
        .clone();
    current
        .session
        .input(node.clone(), run.id().clone(), b"line\n")
        .await
        .unwrap();
    assert_eq!(inputs.recv().await.unwrap(), b"line\n");
    current.session.eof(node, run.id().clone()).await.unwrap();
    current.session.wait_idle().await.unwrap();
    fixture.close().await;
}
