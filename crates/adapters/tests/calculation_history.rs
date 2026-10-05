use wes_adapters::codec::{
    Limits,
    history::{decode_journal, encode_journal},
};
use wes_engine::history::{CommandRecord, JournalEntry};
#[test]
fn native_calculation_package_roundtrips_exactly_and_rejects_unknown_versions() {
    let package = wes_language::calc::Package::standard()
        .source()
        .replace('\n', "\r\n");
    let mut command = CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position {
            line: 3,
            column: 19,
        },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "calc".into(),
        text: ":calc { return 1; }".into(),
        replay: ":calc { return 1; }".into(),
        nodes: vec![],
        type_sources: Default::default(),
        imports: vec![],
        calculation_package: Some(package),
    };
    let entry = JournalEntry::Command(command.clone());
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    let wire: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 1);
    assert_eq!(wire["entry"]["source_start"], serde_json::json!([3, 19]));
    let mut malformed = wire.clone();
    malformed["entry"]["source_start"] = serde_json::json!([0, 1]);
    assert!(decode_journal(&serde_json::to_vec(&malformed).unwrap(), Limits::default()).is_err());
    let mut old = wire.clone();
    old["version"] = serde_json::json!(10);
    assert!(decode_journal(&serde_json::to_vec(&old).unwrap(), Limits::default()).is_err());
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    command.calculation_package = Some("version: 9000".into());
    assert!(encode_journal(&JournalEntry::Command(command), Limits::default()).is_err());
}
