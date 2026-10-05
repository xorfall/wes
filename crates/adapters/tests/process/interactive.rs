use super::*;
use wes_engine::conversations::{self, ConversationHandle, ConversationTask};

fn start(
    cap: Arc<Capability>,
    invoker: &ProcessInvoker,
    arguments: IndexMap<String, Value>,
) -> (ConversationHandle, ConversationTask) {
    conversations::spawn(
        call(cap, arguments),
        Arc::new(invoker.piped()),
        128,
        CancellationToken::new(),
    )
    .unwrap()
}
async fn joined(task: ConversationTask) -> Result<Value, InvocationError> {
    tokio::time::timeout(Duration::from_secs(5), task.join())
        .await
        .unwrap()
        .unwrap()
}
async fn opened(handle: &ConversationHandle) {
    let mut changes = handle.subscribe();
    tokio::time::timeout(Duration::from_secs(4), async {
        while changes.borrow_and_update().phase == conversations::Phase::Opening {
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(handle.snapshot().phase, conversations::Phase::Open);
}

#[tokio::test]
async fn interactive_prompt_precedes_result_and_input_eof_preserves_raw_streams_and_exit() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker(
        "conversation",
        "",
        ProcessConfig {
            timeout: Duration::from_nanos(1),
            ..ProcessConfig::default()
        },
    );
    let (handle, task) = start(cap, &invoker, IndexMap::new());
    opened(&handle).await;
    let mut changes = handle.subscribe();
    let prompt = tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let output = handle.take_output();
            if !output.is_empty() {
                break output.text;
            }
            changes.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(prompt, "answer: ");
    handle.send("cevap 🎉\n".as_bytes()).unwrap();
    handle.eof().unwrap();
    let value = joined(task).await.unwrap();
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes("answer: cevap 🎉\n".as_bytes().to_vec().into())
    );
    assert_eq!(
        fields(&value)["stderr"],
        Data::Bytes(b"done".to_vec().into())
    );
    assert_eq!(fields(&value)["exitCode"], Data::Int(7));
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn interactive_large_stdin_does_not_block_either_output_pipe_and_live_loss_is_explicit() {
    let fixture = Fixture::new().await;
    let (cap, invoker) = fixture.invoker("duplex", "", ProcessConfig::default());
    let (handle, task) = start(cap, &invoker, IndexMap::new());
    for _ in 0..4 {
        handle.send(&vec![b'a'; 64 * 1024]).unwrap();
    }
    handle.eof().unwrap();
    let value = joined(task).await.unwrap();
    assert_eq!(
        fields(&value)["stdout"],
        Data::Bytes(vec![b'o'; 128 * 1024].into())
    );
    assert_eq!(
        fields(&value)["stderr"],
        Data::Bytes(vec![b'e'; 128 * 1024].into())
    );
    let output = handle.take_output();
    assert_eq!(output.text.len(), 128);
    assert_eq!(output.omitted_bytes, 256 * 1024 - 128);
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn interactive_cancellation_joins_child_with_blocked_stdin() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (cap, invoker) = fixture.invoker(
        "wait",
        &listener.local_addr().unwrap().to_string(),
        ProcessConfig::default(),
    );
    let (handle, task) = start(cap, &invoker, IndexMap::new());
    let (mut socket, _) = listener.accept().await.unwrap();
    socket.read_exact(&mut [0; 5]).await.unwrap();
    handle.send(&vec![b'x'; 64 * 1024]).unwrap();
    handle.cancel();
    assert!(matches!(
        joined(task).await,
        Err(InvocationError::Cancelled)
    ));
    assert!(fixture.lock_is_available());
    assert_eq!(
        handle.send(b"late"),
        Err(conversations::ConversationError::Closed)
    );
}

#[tokio::test]
async fn interactive_explicit_timeout_and_aggregate_output_limit_reap_children() {
    let fixture = Fixture::new().await;
    // Startup is inside the explicit budget: a loaded host may time out before the fixture's
    // first instruction. Precreate the test-owned lock so both valid paths can verify release.
    std::fs::write(&fixture.lock, []).unwrap();
    let (cap, invoker) = fixture.invoker("wait", "", ProcessConfig::default());
    let timeout = "PT0.2S".parse().unwrap();
    let (_, task) = start(cap, &invoker, args([("timeout", Data::Duration(timeout))]));
    assert_eq!(failure(joined(task).await).code(), "PROC003");
    assert!(fixture.lock_is_available());
    let (cap, invoker) = fixture.invoker(
        "limit",
        "",
        ProcessConfig {
            output_bytes: 1024,
            ..ProcessConfig::default()
        },
    );
    let (_, task) = start(cap, &invoker, IndexMap::new());
    assert_eq!(failure(joined(task).await).code(), "PROC004");
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn explicit_handover_refuses_piped_input_and_reports_uncaptured_output() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let (cap, invoker) = fixture.invoker(
        "handover",
        &listener.local_addr().unwrap().to_string(),
        ProcessConfig::default(),
    );
    let (handle, task) = conversations::spawn(
        call(cap, IndexMap::new()),
        Arc::new(invoker.handed_over()),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    socket.read_exact(&mut [0; 5]).await.unwrap();
    assert_eq!(
        handle.send(b"private"),
        Err(conversations::ConversationError::Closed)
    );
    socket.write_all(b"x").await.unwrap();
    let value = joined(task).await.unwrap();
    assert_eq!(fields(&value)["stdout"], Data::Bytes(vec![].into()));
    assert_eq!(fields(&value)["stderr"], Data::Bytes(vec![].into()));
    assert_eq!(fields(&value)["exitCode"], Data::Int(7));
    assert!(
        value
            .provenance()
            .fact("ranInteractively")
            .unwrap()
            .contains("terminal")
    );
    assert!(handle.take_output().is_empty());
    assert!(fixture.lock_is_available());
}

#[tokio::test]
async fn one_terminal_owner_serializes_distinct_providers_until_physical_exit() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let terminal = process::TerminalHandover::new();
    let (cap, first) = fixture.invoker("handover", &endpoint, ProcessConfig::default());
    let (_, first_task) = conversations::spawn(
        call(cap, IndexMap::new()),
        Arc::new(terminal.conversation(&first)),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    let (mut first_socket, _) = listener.accept().await.unwrap();
    first_socket.read_exact(&mut [0; 5]).await.unwrap();
    let second_lock = fixture.lock.with_extension("second.lock");
    let (description, second) = process::provider(
        "second",
        vec![
            fixture.executable.clone(),
            "handover".into(),
            second_lock.to_str().unwrap().into(),
            endpoint,
        ],
        "arg",
        ProcessConfig::default(),
    )
    .unwrap();
    let (second_handle, second_task) = conversations::spawn(
        call(
            description.capabilities().next().unwrap().clone(),
            IndexMap::new(),
        ),
        Arc::new(terminal.clone().conversation(&second)),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
    assert_eq!(
        second_handle.snapshot().phase,
        conversations::Phase::Opening
    );
    assert!(!second_lock.exists());
    first_socket.write_all(b"x").await.unwrap();
    joined(first_task).await.unwrap();
    let (mut second_socket, _) = listener.accept().await.unwrap();
    second_socket.read_exact(&mut [0; 5]).await.unwrap();
    second_socket.write_all(b"x").await.unwrap();
    joined(second_task).await.unwrap();
    assert!(fixture.lock_is_available());
    assert!(
        OpenOptions::new()
            .read(true)
            .write(true)
            .open(second_lock)
            .unwrap()
            .try_lock()
            .is_ok()
    );
}

#[tokio::test]
async fn terminal_waiting_cancellation_and_explicit_budget_never_launch_a_second_child() {
    let fixture = Fixture::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = listener.local_addr().unwrap().to_string();
    let terminal = process::TerminalHandover::new();
    let (cap, first) = fixture.invoker("handover", &endpoint, ProcessConfig::default());
    let (_, first_task) = conversations::spawn(
        call(cap, IndexMap::new()),
        Arc::new(terminal.conversation(&first)),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    let (mut socket, _) = listener.accept().await.unwrap();
    socket.read_exact(&mut [0; 5]).await.unwrap();
    let second_lock = fixture.lock.with_extension("never.lock");
    let (description, second) = process::provider(
        "second",
        vec![
            fixture.executable.clone(),
            "handover".into(),
            second_lock.to_str().unwrap().into(),
            endpoint,
        ],
        "arg",
        ProcessConfig::default(),
    )
    .unwrap();
    let cap = description.capabilities().next().unwrap().clone();
    let (handle, task) = conversations::spawn(
        call(cap.clone(), IndexMap::new()),
        Arc::new(terminal.conversation(&second)),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    handle.cancel();
    assert!(matches!(
        joined(task).await,
        Err(InvocationError::Cancelled)
    ));
    let (_, task) = conversations::spawn(
        call(
            cap,
            args([("timeout", Data::Duration("PT0.01S".parse().unwrap()))]),
        ),
        Arc::new(terminal.conversation(&second)),
        128,
        CancellationToken::new(),
    )
    .unwrap();
    assert_eq!(failure(joined(task).await).code(), "PROC003");
    assert!(!second_lock.exists());
    socket.write_all(b"x").await.unwrap();
    joined(first_task).await.unwrap();
    assert!(fixture.lock_is_available());
}
