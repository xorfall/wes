use super::*;
use wes_engine::history::{HistoryCursor, HistoryPage, HistoryPageLimits};
struct Pages;
impl JournalSink for Pages {
    fn append(&mut self, _: &Record) -> Result<AppendReceipt, RecordError> {
        panic!("read creates no records")
    }
    fn page(
        &mut self,
        _: Option<HistoryCursor>,
        _: HistoryPageLimits,
    ) -> Result<HistoryPage, RecordError> {
        Ok(HistoryPage::new(AppendReceipt {
            persistence: Persistence::FileSynced,
            end_offset: 0,
        }))
    }
}
#[tokio::test]
async fn session_pages_are_read_only_and_retained_handle_does_not_keep_closed_writer_alive() {
    let (recorder, writer) = spawn_recorder(Pages, RecorderLimits::default()).unwrap();
    let (handle, task) = session::spawn(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Required(CallJournal::new(
            recorder.clone(),
            RequiredPersistence::FileSynced,
        )),
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    let page = handle
        .history_page(None, HistoryPageLimits::default())
        .await
        .unwrap();
    assert!(page.page().entries().is_empty());
    assert_eq!(page.append_report().attempted, 0);
    assert!(handle.observe().await.unwrap().cells.is_empty());
    drop(recorder);
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
    assert!(matches!(
        handle
            .history_page(None, HistoryPageLimits::default())
            .await,
        Err(RecordError::Closed)
    ));
    // Both the session handle and returned page still exist. Neither owns the writer.
    tokio::time::timeout(Duration::from_secs(3), writer.join())
        .await
        .unwrap()
        .unwrap();
    assert!(page.page().entries().is_empty());
}
#[tokio::test]
async fn memory_only_session_refuses_saved_history_pages_explicitly() {
    let (handle, task) = session::spawn(
        Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap()),
        RecordingMode::Ephemeral,
        no_files(),
        NonZeroUsize::new(1).unwrap(),
    )
    .unwrap();
    assert!(matches!(
        handle
            .history_page(None, HistoryPageLimits::default())
            .await,
        Err(RecordError::PageUnsupported)
    ));
    handle.shutdown().await.unwrap();
    task.join().await.unwrap();
}
