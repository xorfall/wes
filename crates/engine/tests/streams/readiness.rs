use super::*;
use streams::{MAX_READY_WAIT, ReadyError};

#[tokio::test]
async fn acknowledgement_without_first_event_is_ready_and_receipt_expires_on_close() {
    let (call, replacement) = calls();
    let run = call.run.clone();
    let (provider, mut entered) = provider(None, false);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let started = entered.recv().await.unwrap();
    let waiting = tokio::spawn({
        let handle = handle.clone();
        let run = run.clone();
        async move {
            handle
                .wait_ready(&run, Duration::from_secs(2), CancellationToken::new())
                .await
        }
    });
    tokio::task::yield_now().await;
    assert!(!waiting.is_finished());
    started.sink.opened().unwrap();
    let receipt = waiting.await.unwrap().unwrap();
    assert!(matches!(handle.snapshot().window.data(),Data::List(v) if v.is_empty()));
    assert_eq!(receipt.admit(&run, || 42), Ok(42));
    assert_eq!(
        receipt.admit(&replacement.run, || panic!("foreign admission")),
        Err::<(), _>(ReadyError::Changed)
    );
    handle.cancel();
    assert_eq!(
        receipt.admit(&run, || panic!("closed admission")),
        Err::<(), _>(ReadyError::Closed)
    );
    started
        .finish
        .send(Err(InvocationError::Cancelled))
        .unwrap();
    task.join().await.unwrap();
}
#[tokio::test(start_paused = true)]
async fn cancelled_and_expired_waits_do_not_cancel_or_restart_the_source() {
    let (call, other) = calls();
    let run = call.run.clone();
    let (provider, mut entered) = provider(None, false);
    let (handle, task) = streams::spawn(
        call,
        provider,
        Provenance::default(),
        Limits::default(),
        CancellationToken::new(),
    )
    .unwrap();
    let started = entered.recv().await.unwrap();
    assert!(matches!(
        handle
            .wait_ready(&other.run, Duration::from_secs(1), CancellationToken::new())
            .await,
        Err(ReadyError::Changed)
    ));
    assert!(matches!(
        handle
            .wait_ready(
                &run,
                MAX_READY_WAIT + Duration::from_secs(1),
                CancellationToken::new()
            )
            .await,
        Err(ReadyError::Invalid)
    ));
    let token = CancellationToken::new();
    token.cancel();
    assert!(matches!(
        handle.wait_ready(&run, Duration::from_secs(1), token).await,
        Err(ReadyError::Cancelled)
    ));
    assert!(matches!(
        handle
            .wait_ready(&run, Duration::from_secs(1), CancellationToken::new())
            .await,
        Err(ReadyError::Timeout)
    ));
    assert!(!started.token.is_cancelled());
    started.finish.send(Ok(())).unwrap();
    task.join().await.unwrap();
    assert!(matches!(
        handle
            .wait_ready(&run, Duration::from_secs(1), CancellationToken::new())
            .await,
        Err(ReadyError::Closed)
    ));
}
