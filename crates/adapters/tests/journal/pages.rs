use super::*;
use wes_engine::history::{DiagnosticRecord, HistoryCursor, HistoryPageLimits};
fn diagnostic(id: &str) -> Record {
    Record::Journal(JournalEntry::Diagnosed(
        DiagnosticRecord::new(
            id.into(),
            wes_core::Timestamp::new(100, 0).unwrap(),
            "cell".into(),
            "abc".into(),
            wes_language::Diagnostic::error(
                "TEST",
                wes_language::Span::at(0),
                "synthetic diagnostic",
            ),
        )
        .unwrap(),
    ))
}
fn ids(page: &wes_engine::history::HistoryPage) -> Vec<&str> {
    page.entries()
        .iter()
        .map(|entry| match &entry.entry {
            JournalEntry::Diagnosed(record) => record.id(),
            _ => panic!("diagnostic fixture"),
        })
        .collect()
}
#[test]
fn paged_history_holds_a_stable_prefix_while_later_appends_remain_owned_and_readable() {
    let root = private_temp();
    let mut history = open(root.path());
    history.append(&command("inert command text")).unwrap();
    history.append(&diagnostic("one")).unwrap();
    let end = history.append(&diagnostic("two")).unwrap();
    let limits = HistoryPageLimits {
        entries: 1,
        ..Default::default()
    };
    let first = history.page(None, limits).unwrap();
    assert_eq!(ids(&first), ["one"]);
    assert_eq!(first.checkpoint(), end);
    let cursor = first.next().unwrap();
    let later = history.append(&diagnostic("three")).unwrap();
    let before = fs::read(root.path().join("journal.jsonl")).unwrap();
    let second = history.page(Some(cursor), limits).unwrap();
    assert_eq!(ids(&second), ["two"]);
    assert_eq!(second.checkpoint(), end);
    assert!(second.next().is_none());
    let fresh = history.page(None, HistoryPageLimits::default()).unwrap();
    assert_eq!(ids(&fresh), ["one", "two", "three"]);
    assert_eq!(fresh.checkpoint(), later);
    assert_eq!(fs::read(root.path().join("journal.jsonl")).unwrap(), before);
    assert!(!root.path().join("recovery.jsonl").exists());
}
#[test]
fn page_byte_limits_and_invalid_or_foreign_cursors_never_skip_records_or_poison_valid_writes() {
    let root = private_temp();
    let mut history = open(root.path());
    history.append(&diagnostic("one")).unwrap();
    let charge = history
        .page(None, HistoryPageLimits::default())
        .unwrap()
        .charged_bytes();
    history.append(&diagnostic("two")).unwrap();
    let limits = HistoryPageLimits {
        entries: 200,
        bytes: charge,
    };
    let first = history.page(None, limits).unwrap();
    let cursor = first.next().unwrap();
    assert_eq!(ids(&first), ["one"]);
    assert_eq!(ids(&history.page(Some(cursor), limits).unwrap()), ["two"]);
    assert!(matches!(
        history.page(
            None,
            HistoryPageLimits {
                entries: 1,
                bytes: 1
            }
        ),
        Err(RecordError::Limit(_))
    ));
    for invalid in [
        HistoryCursor::new(cursor.journal(), cursor.boundary(), 1).unwrap(),
        HistoryCursor::new(cursor.journal(), cursor.boundary() + 1, 0).unwrap(),
        HistoryCursor::new(uuid::Uuid::new_v4(), cursor.boundary(), 0).unwrap(),
    ] {
        assert!(matches!(
            history.page(Some(invalid), limits),
            Err(RecordError::InvalidCursor)
        ));
    }
    history.append(&diagnostic("after")).unwrap();
    drop(history);
    let mut reopened = open(root.path());
    assert!(matches!(
        reopened.page(Some(cursor), limits),
        Err(RecordError::InvalidCursor)
    ));
    assert_eq!(
        ids(&reopened.page(None, HistoryPageLimits::default()).unwrap()),
        ["one", "two", "after"]
    );
}
#[test]
fn pages_make_progress_past_non_log_records() {
    let root = private_temp();
    drop(open(root.path()));
    let Record::Journal(command) = command("inert") else {
        unreachable!()
    };
    let mut line =
        wes_adapters::codec::history::encode_journal(&command, Limits::default()).unwrap();
    line.push(b'\n');
    let mut bytes = line.repeat(2001);
    let Record::Journal(diagnostic) = diagnostic("legacy") else {
        unreachable!()
    };
    let encoded =
        wes_adapters::codec::history::encode_journal(&diagnostic, Limits::default()).unwrap();
    bytes.extend(encoded);
    bytes.push(b'\n');
    fs::write(root.path().join("journal.jsonl"), &bytes).unwrap();
    let mut history = open(root.path());
    let first = history.page(None, HistoryPageLimits::default()).unwrap();
    assert!(first.entries().is_empty());
    let second = history
        .page(first.next(), HistoryPageLimits::default())
        .unwrap();
    assert_eq!(ids(&second), ["legacy"]);

    assert!(second.next().is_none());
    assert_eq!(fs::read(root.path().join("journal.jsonl")).unwrap(), bytes);
}
#[test]
fn page_detects_out_of_band_change_without_repair_and_empty_read_creates_no_journal() {
    let root = private_temp();
    let mut history = open(root.path());
    assert!(
        history
            .page(None, HistoryPageLimits::default())
            .unwrap()
            .entries()
            .is_empty()
    );
    assert!(!root.path().join("journal.jsonl").exists());
    history.append(&diagnostic("one")).unwrap();
    let path = root.path().join("journal.jsonl");
    let mut changed = fs::read(&path).unwrap();
    changed.extend(b"broken");
    fs::write(&path, &changed).unwrap();
    assert!(matches!(
        history.page(None, HistoryPageLimits::default()),
        Err(RecordError::Backend { .. })
    ));
    assert!(matches!(
        history.append(&diagnostic("two")),
        Err(RecordError::Poisoned)
    ));
    assert_eq!(fs::read(path).unwrap(), changed);
}
