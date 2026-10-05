use super::*;
use wes_engine::history::{
    DiagnosticRecord, HistoryCursor, HistoryPage, HistoryPageLimits, JournalEntry,
};
#[derive(Default)]
struct PagingSink {
    writes: u64,
    gate: Option<(oneshot::Sender<()>, mpsc::Receiver<()>)>,
    excessive: bool,
    panic: bool,
}
impl JournalSink for PagingSink {
    fn append(&mut self, _: &Record) -> Result<AppendReceipt, RecordError> {
        self.writes += 1;
        Ok(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.writes,
        })
    }
    fn page(
        &mut self,
        _: Option<HistoryCursor>,
        _: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        if let Some((entered, release)) = self.gate.take() {
            entered.send(()).unwrap();
            release.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert!(!self.panic, "synthetic private paging panic");
        let mut page = HistoryPage::new(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: self.writes,
        });
        if self.excessive {
            for id in ["one", "two"] {
                page.push(
                    JournalEntry::Diagnosed(
                        DiagnosticRecord::new(
                            id.into(),
                            wes_core::Timestamp::new(100, 0).unwrap(),
                            "cell".into(),
                            "".into(),
                            wes_language::Diagnostic::error(
                                "TEST",
                                wes_language::Span::at(0),
                                "synthetic",
                            ),
                        )
                        .unwrap(),
                    ),
                    HistoryPageLimits::default(),
                )?;
            }
        }
        Ok(page)
    }
}
#[tokio::test]
async fn retained_page_credit_is_separate_from_appends_and_shutdown() {
    let (recorder, writer) =
        spawn_recorder(PagingSink::default(), RecorderLimits::default()).unwrap();
    recorder.append(record("before")).await.unwrap();
    let page = recorder
        .page(None, HistoryPageLimits::default())
        .await
        .unwrap();
    assert_eq!(page.page().checkpoint().end_offset, 1);
    assert_eq!(page.append_report().attempted, 1);
    assert!(matches!(
        recorder.try_page(None, HistoryPageLimits::default()).await,
        Err(RecordError::ReadBusy)
    ));
    recorder.append(record("after")).await.unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(5),
            recorder.page(None, HistoryPageLimits::default())
        )
        .await
        .is_err()
    );
    assert_eq!(recorder.flush().await.unwrap().attempted, 2);
    let r = recorder.clone();
    let waiting = tokio::spawn(async move { r.page(None, HistoryPageLimits::default()).await });
    assert_eq!(recorder.shutdown().await.unwrap().attempted, 2);
    assert!(matches!(waiting.await.unwrap(), Err(RecordError::Closed)));
    writer.join().await.unwrap();
    assert!(matches!(
        recorder.try_page(None, HistoryPageLimits::default()).await,
        Err(RecordError::Closed)
    ));
    assert_eq!(page.page().checkpoint().end_offset, 1);
}
#[tokio::test]
async fn abandoned_page_keeps_physical_read_owned_until_shutdown_joins_it() {
    let (entered, blocked) = oneshot::channel();
    let (release, released) = mpsc::channel();
    let (recorder, writer) = spawn_recorder(
        PagingSink {
            gate: Some((entered, released)),
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    let r = recorder.clone();
    let pending = tokio::spawn(async move { r.page(None, HistoryPageLimits::default()).await });
    blocked.await.unwrap();
    pending.abort();
    assert!(pending.await.unwrap_err().is_cancelled());
    let r = recorder.clone();
    let closing = tokio::spawn(async move { r.shutdown().await });
    tokio::task::yield_now().await;
    assert!(!closing.is_finished());
    release.send(()).unwrap();
    assert_eq!(closing.await.unwrap().unwrap().attempted, 0);
    writer.join().await.unwrap();
}
#[tokio::test]
async fn unsupported_invalid_and_excessive_pages_are_reads_not_failed_appends() {
    let (recorder, writer) = spawn_recorder(sink(), RecorderLimits::default()).unwrap();
    assert!(matches!(
        recorder.page(None, HistoryPageLimits::default()).await,
        Err(RecordError::PageUnsupported)
    ));
    assert!(matches!(
        recorder
            .page(
                None,
                HistoryPageLimits {
                    entries: 0,
                    bytes: 1
                }
            )
            .await,
        Err(RecordError::Limit(_))
    ));
    recorder.append(record("after")).await.unwrap();
    assert_eq!(recorder.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
    let (recorder, writer) = spawn_recorder(
        PagingSink {
            excessive: true,
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        recorder
            .page(
                None,
                HistoryPageLimits {
                    entries: 1,
                    ..Default::default()
                }
            )
            .await,
        Err(RecordError::Limit(_))
    ));
    assert_eq!(recorder.shutdown().await.unwrap().failed, 0);
    writer.join().await.unwrap();
}
#[tokio::test]
async fn paging_panic_closes_and_joins_the_writer_without_exporting_panic_payload() {
    let (recorder, writer) = spawn_recorder(
        PagingSink {
            panic: true,
            ..Default::default()
        },
        RecorderLimits::default(),
    )
    .unwrap();
    assert!(matches!(
        recorder.page(None, HistoryPageLimits::default()).await,
        Err(RecordError::Closed)
    ));
    assert!(matches!(writer.join().await, Err(RecordError::Closed)));
}
