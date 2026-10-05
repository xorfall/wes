use super::*;
use wes_engine::history::{RequestClaim, RequestRecord};

#[tokio::test]
async fn request_identity_is_shared_atomic_and_independent_of_live_actor_authority() {
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    let source = |cell: &str, actor: &str| {
        input(cell, "catalog echo value:hello")
            .with_client(actor.into())
            .unwrap()
            .cooperative()
    };
    let (one, two) = tokio::join!(
        handle.submit_request(
            "pane".into(),
            "request".into(),
            "context".into(),
            true,
            source("first", "old-actor")
        ),
        handle.submit_request(
            "pane".into(),
            "request".into(),
            "context".into(),
            true,
            source("second", "old-actor")
        )
    );
    let (one, two) = (one.unwrap(), two.unwrap());
    assert_ne!(one.claim.fresh, two.claim.fresh);
    assert_eq!(one.claim.record.cell, two.claim.record.cell);
    assert_eq!(handle.observe().await.unwrap().cells.len(), 1);
    let resumed = handle
        .submit_request(
            "pane".into(),
            "request".into(),
            "context".into(),
            false,
            source("unused", "new-actor"),
        )
        .await
        .unwrap();
    assert!(!resumed.claim.fresh);
    assert_eq!(resumed.claim.record.cell, one.claim.record.cell);
    assert!(matches!(
        handle
            .read_source("new-actor".into(), one.claim.record.cell.clone())
            .await,
        Err(SessionError::Authority)
    ));
    for (context, changed) in [
        ("other", source("c", "old-actor")),
        ("context", source("c", "old-actor").with_reactive(true)),
        (
            "context",
            input("c", "catalog echo value:changed")
                .with_client("old-actor".into())
                .unwrap()
                .cooperative(),
        ),
    ] {
        assert!(matches!(
            handle
                .submit_request(
                    "pane".into(),
                    "request".into(),
                    context.into(),
                    true,
                    changed
                )
                .await,
            Err(RecordError::RequestConflict)
        ));
    }
    assert!(matches!(
        handle
            .submit_request(
                "pane".into(),
                "new".into(),
                "old-context".into(),
                false,
                source("new", "old-actor")
            )
            .await,
        Err(RecordError::RequestContext)
    ));
    handle
        .submit_request(
            "another-pane".into(),
            "request".into(),
            "context".into(),
            true,
            source("other", "other-actor"),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(3), async {
        while calls.load(Ordering::SeqCst) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    stop(handle, task).await;
}
struct Claims {
    records: Arc<Mutex<Vec<Record>>>,
    gate: Option<Gate>,
    fail: bool,
}
impl JournalSink for Claims {
    fn claim_request(
        &mut self,
        record: RequestRecord,
        current: bool,
    ) -> Result<RequestClaim, RecordError> {
        if let Some((entered, released)) = self.gate.take() {
            entered.send(()).unwrap();
            released.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        if self.fail {
            return Err(RecordError::backend(
                "synthetic reservation",
                true,
                std::io::Error::other("uncertain"),
            ));
        }
        if let Some(Record::Journal(JournalEntry::Requested(previous))) = self.records.lock().unwrap().iter().find(|r| matches!(r, Record::Journal(JournalEntry::Requested(r)) if r.namespace == record.namespace && r.request == record.request)) {
            if !previous.matches(&record) { return Err(RecordError::RequestConflict); }
            return Ok(RequestClaim { record: previous.clone(), fresh: false, persistence: Persistence::FileSynced });
        }
        if !current {
            return Err(RecordError::RequestContext);
        }
        self.append(&Record::Journal(JournalEntry::Requested(record.clone())))?;
        Ok(RequestClaim {
            record,
            fresh: true,
            persistence: Persistence::FileSynced,
        })
    }
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        let mut records = self.records.lock().unwrap();
        records.push(record.clone());
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: records.len() as u64,
        })
    }
}
#[tokio::test]
async fn abandoning_a_wait_does_not_abandon_reserved_work_and_reservation_failure_never_executes() {
    for fail in [false, true] {
        let records = Arc::new(Mutex::new(vec![]));
        let (entered, blocked) = oneshot::channel();
        let (release, released) = mpsc::channel();
        let (recorder, writer) = spawn_recorder(
            Claims {
                records: records.clone(),
                gate: Some((entered, released)),
                fail,
            },
            RecorderLimits::default(),
        )
        .unwrap();
        let (base, calls) = workspace(None, None);
        let (handle, task) = session::spawn(
            base,
            RecordingMode::Required(CallJournal::new(
                recorder.clone(),
                RequiredPersistence::FileSynced,
            )),
            no_files(),
            NonZeroUsize::new(2).unwrap(),
        )
        .unwrap();
        let pending = {
            let handle = handle.clone();
            tokio::spawn(async move {
                handle
                    .submit_request(
                        "pane".into(),
                        "r".into(),
                        "c".into(),
                        true,
                        input("cell", "catalog echo value:hello"),
                    )
                    .await
            })
        };
        blocked.await.unwrap();
        if !fail {
            pending.abort();
        }
        release.send(()).unwrap();
        if fail {
            assert!(pending.await.unwrap().is_err());
            assert_eq!(calls.load(Ordering::SeqCst), 0);
            assert!(handle.observe().await.unwrap().cells.is_empty());
            assert_eq!(recorder.flush().await.unwrap().failed, 1);
        } else {
            tokio::time::timeout(Duration::from_secs(3), async {
                while calls.load(Ordering::SeqCst) != 1 {
                    tokio::task::yield_now().await;
                }
            })
            .await
            .unwrap();
            let duplicate = handle
                .submit_request(
                    "pane".into(),
                    "r".into(),
                    "c".into(),
                    false,
                    input("unused", "catalog echo value:hello"),
                )
                .await
                .unwrap();
            assert!(!duplicate.claim.fresh);
            assert_eq!(duplicate.claim.record.cell, "cell");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
            assert!(matches!(
                records.lock().unwrap().first(),
                Some(Record::Journal(JournalEntry::Requested(_)))
            ));
        }
        stop(handle, task).await;
        recorder.shutdown().await.unwrap();
        writer.join().await.unwrap();
    }
}

struct BlockedObservationRequests {
    claims: Claims,
    gate: Option<Gate>,
}
impl JournalSink for BlockedObservationRequests {
    fn claim_request(
        &mut self,
        record: RequestRecord,
        current: bool,
    ) -> Result<RequestClaim, RecordError> {
        self.claims.claim_request(record, current)
    }
    fn find_request(
        &mut self,
        namespace: &str,
        request: &str,
    ) -> Result<Option<RequestRecord>, RecordError> {
        Ok(self
            .claims
            .records
            .lock()
            .unwrap()
            .iter()
            .find_map(|record| match record {
                Record::Journal(JournalEntry::Requested(record))
                    if record.namespace == namespace && record.request == request =>
                {
                    Some(record.clone())
                }
                _ => None,
            }))
    }
    fn append(&mut self, record: &Record) -> Result<AppendReceipt, RecordError> {
        if matches!(record, Record::Journal(JournalEntry::Observed(_))) {
            if let Some((entered, released)) = self.gate.take() {
                entered.send(()).unwrap();
                released.recv_timeout(Duration::from_secs(10)).unwrap();
            }
        }
        self.claims.append(record)
    }
}

#[tokio::test]
async fn known_request_cancellation_bypasses_blocked_recording_without_granting_authority() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        BlockedObservationRequests {
            claims: Claims {
                records: Default::default(),
                gate: None,
                fail: false,
            },
            gate: Some((entered, released)),
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let (base, calls) = workspace(None, None);
    let (handle, task) = session::spawn(
        base,
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(2).unwrap(),
    )
    .unwrap();
    handle
        .observe_actor("owner".into(), "terminal".into())
        .await
        .unwrap();
    let submitted = handle
        .submit_request(
            "pane".into(),
            "run".into(),
            "context".into(),
            true,
            input("work", "catalog echo value:hello > answer")
                .with_client("owner".into())
                .unwrap()
                .cooperative(),
        )
        .await
        .unwrap();
    let node = submitted.submission.unwrap().unwrap().nodes[0].clone();
    blocked.await.unwrap();
    // The old lookup path really is stuck behind disk; no provider has started yet.
    assert!(
        tokio::time::timeout(
            Duration::from_millis(30),
            recorder.find_request("pane", "run")
        )
        .await
        .is_err()
    );
    let record = tokio::time::timeout(Duration::from_secs(1), handle.find_request("pane", "run"))
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(record.cell, "work");
    assert!(matches!(
        handle
            .cancel_actor_work("foreign".into(), record.cell.clone())
            .await,
        Err(SessionError::Authority) | Err(SessionError::AccessDenied(_))
    ));
    assert!(matches!(
        handle
            .cancel_actor_work("owner".into(), "missing".into())
            .await,
        Err(SessionError::UnknownNode)
    ));
    tokio::time::timeout(
        Duration::from_secs(1),
        handle.cancel_actor_work("owner".into(), record.cell.clone()),
    )
    .await
    .unwrap()
    .unwrap();
    // The trusted UI uses the same current cell membership, also independent of recording.
    tokio::time::timeout(Duration::from_secs(1), handle.cancel_work(record.cell))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        handle
            .observe()
            .await
            .unwrap()
            .state
            .execution
            .graph
            .node(&node)
            .unwrap()
            .state(),
        NodeState::Cancelled
    );
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    release.send(()).unwrap();
    // Cold/unknown identities still consult the authoritative journal; no false negatives.
    assert!(
        handle
            .find_request("other-pane", "run")
            .await
            .unwrap()
            .is_none()
    );
    stop(handle, task).await;
    recorder.shutdown().await.unwrap();
    writer.join().await.unwrap();
}
