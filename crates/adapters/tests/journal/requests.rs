use super::*;
use wes_engine::history::RequestRecord;
fn request(index: usize) -> RequestRecord {
    RequestRecord {
        namespace: "pane".into(),
        request: format!("r{index}"),
        cell: format!("cell{index}"),
        fingerprint: format!("{index:064x}"),
        steps: Vec::new(),
    }
}
#[test]
fn more_than_ten_thousand_requests_survive_hot_eviction_reopen_and_old_context() {
    let root = private_temp();
    let mut history = open(root.path());
    for index in 0..10_005 {
        assert!(history.claim_request(request(index), true).unwrap().fresh);
    }
    let before = fs::metadata(root.path().join("journal.jsonl"))
        .unwrap()
        .len();
    for index in [0, 9999, 10004] {
        let mut retry = request(index);
        retry.cell = "unused".into();
        let claim = history.claim_request(retry, false).unwrap();
        assert!(!claim.fresh);
        assert_eq!(claim.record, request(index));
    }
    assert_eq!(
        fs::metadata(root.path().join("journal.jsonl"))
            .unwrap()
            .len(),
        before
    );
    drop(history);
    let mut history = open(root.path());
    for index in [0, 9999, 10004] {
        assert_eq!(
            history.find_request("pane", &format!("r{index}")).unwrap(),
            Some(request(index))
        );
    }
    assert!(history.find_request("other-pane", "r0").unwrap().is_none());
    let mut conflict = request(0);
    conflict.fingerprint = "f".repeat(64);
    assert!(matches!(
        history.claim_request(conflict, true),
        Err(RecordError::RequestConflict)
    ));
    assert!(matches!(
        history.claim_request(request(20000), false),
        Err(RecordError::RequestContext)
    ));
    assert!(history.claim_request(request(20000), true).unwrap().fresh);
}
#[test]
fn request_codec_is_strict_versioned_and_has_no_source_or_result_copy() {
    use wes_adapters::codec::history::{decode_journal, encode_journal};
    let record = JournalEntry::Requested(request(0));
    let bytes = encode_journal(&record, Limits::default()).unwrap();
    assert!(bytes.len() < 256);
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        record
    );
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 1);
    for (field, value) in [
        ("namespace", serde_json::json!("")),
        ("request", serde_json::json!("r\n")),
        ("fingerprint", serde_json::json!("bad")),
        ("source", serde_json::json!("unexpected")),
    ] {
        let mut bad = wire.clone();
        bad["entry"][field] = value;
        assert!(decode_journal(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    }
    let mut bad = wire;
    bad["version"] = serde_json::json!(6);
    assert!(decode_journal(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
}
