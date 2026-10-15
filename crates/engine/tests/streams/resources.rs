use super::*;
use streams::ExclusiveResource;

struct Exclusive {
    inner: Controlled,
    keys: Vec<ExclusiveResource>,
}
impl StreamingInvoker for Exclusive {
    fn exclusive_resources(&self, _: &Call) -> Result<Vec<ExclusiveResource>, StreamError> {
        Ok(self.keys.clone())
    }
    fn subscribe(&self, call: Call, sink: StreamSink, token: CancellationToken) -> StreamFuture {
        self.inner.subscribe(call, sink, token)
    }
}
fn exclusive(
    keys: Vec<ExclusiveResource>,
) -> (Arc<dyn StreamingInvoker>, mpsc::UnboundedReceiver<Started>) {
    let (entered, received) = mpsc::unbounded_channel();
    (
        Arc::new(Exclusive {
            inner: Controlled {
                entered,
                first: None,
                acknowledge: true,
            },
            keys,
        }),
        received,
    )
}
fn start(
    call: Call,
    provider: Arc<dyn StreamingInvoker>,
) -> Result<(StreamHandle, streams::StreamTask), StreamError> {
    streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
}
fn key(namespace: &str, part: &str) -> ExclusiveResource {
    ExclusiveResource::new(namespace, vec![part.into()]).unwrap()
}

#[tokio::test]
async fn cancellation_and_dropped_task_preserve_reservation_until_physical_exit() {
    let namespace = uuid::Uuid::new_v4().to_string();
    let (call, replacement) = calls();
    let (provider, mut entered) = exclusive(vec![key(&namespace, "port")]);
    let (handle, task) = start(call.clone(), provider.clone()).unwrap();
    let started = entered.recv().await.unwrap();
    // A fresh adapter and a different run still share the physical resource.
    let (alias, mut alias_entered) = exclusive(vec![key(&namespace, "port")]);
    assert!(matches!(
        start(replacement.clone(), alias.clone()),
        Err(StreamError::ResourceBusy)
    ));
    assert!(alias_entered.try_recv().is_err());
    let receipt = handle
        .wait_ready(&call.run, Duration::from_secs(1), CancellationToken::new())
        .await
        .unwrap();
    drop(task);
    assert!(started.token.is_cancelled());
    assert!(matches!(
        receipt.admit(&call.run, || ()),
        Err(streams::ReadyError::Closed)
    ));
    assert!(matches!(
        start(replacement.clone(), alias.clone()),
        Err(StreamError::ResourceBusy)
    ));
    // Dropping a weak display handle does not release the live owner's resource.
    let mut updates = handle.subscribe();
    drop(handle);
    assert!(matches!(
        start(replacement.clone(), alias.clone()),
        Err(StreamError::ResourceBusy)
    ));
    started
        .finish
        .send(Err(InvocationError::Cancelled))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while updates.changed().await.is_ok() {}
    })
    .await
    .unwrap(); // sender closure proves the native lifetime task finished
    let (_, joined) = start(replacement, alias).unwrap();
    let next = alias_entered.recv().await.unwrap();
    next.finish.send(Ok(())).unwrap();
    joined.join().await.unwrap();
}

#[tokio::test]
async fn multiple_resources_are_atomic_and_independent_resources_can_run() {
    let namespace = uuid::Uuid::new_v4().to_string();
    let (call, replacement) = calls();
    let (held, mut entered) = exclusive(vec![key(&namespace, "a")]);
    let (_, task) = start(call.clone(), held).unwrap();
    let first = entered.recv().await.unwrap();
    let (conflicting, mut forbidden) = exclusive(vec![key(&namespace, "b"), key(&namespace, "a")]);
    assert!(matches!(
        start(replacement.clone(), conflicting),
        Err(StreamError::ResourceBusy)
    ));
    assert!(forbidden.try_recv().is_err());
    let (free, mut free_entered) = exclusive(vec![key(&namespace, "b")]);
    let (_, other) = start(replacement, free).unwrap();
    let second = free_entered.recv().await.unwrap();
    first.finish.send(Ok(())).unwrap();
    second.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    other.join().await.unwrap();
}
