use serde_json::{Value, json};
use std::io::{self, BufReader, Cursor, Read};
use wes_adapters::{
    codec::{
        CodecError, Limits,
        history::{decode_journal, decode_recovery, encode_journal, encode_recovery},
    },
    journal::{ReadError, ReadLimits, read_journal, read_recovery},
};
use wes_engine::{
    graph::NodeId,
    history::{CallRecord, JournalEntry, RecoveryEntry},
    runtime::RunId,
};

fn fixtures() -> Vec<Value> {
    serde_json::from_str(include_str!(
        "../../../tests/fixtures/history-acceptance.json"
    ))
    .unwrap()
}

#[test]
fn commands_require_current_source_identity_in_both_wire_directions() {
    let entry = imported_command();
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    for cell in [None, Some(Value::Null)] {
        let mut invalid = wire.clone();
        match cell {
            None => {
                invalid["entry"].as_object_mut().unwrap().remove("cell");
            }
            Some(cell) => invalid["entry"]["cell"] = cell,
        }
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
    for identity in [
        "".to_owned(),
        " \t".to_owned(),
        "bad\ncell".to_owned(),
        "x".repeat(257),
    ] {
        let mut invalid = wire.clone();
        invalid["entry"]["cell"] = json!(identity);
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
        let JournalEntry::Command(mut command) = entry.clone() else {
            panic!()
        };
        command.cell = identity;
        assert!(command.validate().is_err());
        assert!(encode_journal(&JournalEntry::Command(command), Limits::default()).is_err());
    }
    let JournalEntry::Command(mut command) = entry else {
        panic!()
    };
    command.text = "x".repeat(wes_engine::source::max_source_bytes() + 1);
    assert!(command.validate().is_err());
    assert!(encode_journal(&JournalEntry::Command(command), Limits::default()).is_err());
}

#[test]
fn retired_work_is_strict_identity_and_cleanup_evidence_without_source() {
    use wes_engine::{history::RetiredWork, storage::ValueHandle};
    let handle = ValueHandle::fresh();
    let record = JournalEntry::Retired(RetiredWork {
        nodes: vec![NodeId::new("id90").unwrap()],
        payloads: vec![handle.clone()],
        protected: vec![handle],
    });
    let bytes = encode_journal(&record, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        record
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 1);
    let mut invalid = wire.clone();
    invalid["version"] = json!(5);
    assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    let mut invalid = wire.clone();
    invalid["entry"]["payloads"] = json!([]);
    assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    let mut invalid = wire.clone();
    invalid["entry"]["nodes"] = json!(["id90", "id90"]);
    assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    let mut invalid = wire;
    invalid["entry"]["source"] = json!("hidden archive");
    assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
}

#[test]
fn temporary_payload_ownership_is_not_a_retained_result() {
    let record = JournalEntry::Payload {
        node: NodeId::new("id1").unwrap(),
        run: RunId::new("run").unwrap(),
        handle: wes_engine::storage::ValueHandle::fresh(),
    };
    let bytes = encode_journal(&record, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        record
    );
    let records = [record];
    let index = wes_engine::history::RestoreIndex::build(&records);
    assert!(index.retained.is_empty());
    let mut wire: Value = serde_json::from_slice(&bytes).unwrap();
    wire["entry"]["run"] = json!("");
    assert!(decode_journal(&serde_json::to_vec(&wire).unwrap(), Limits::default()).is_err());
}

#[test]
fn submission_presentation_roundtrips_as_non_executable_and_rejects_invalid_identity() {
    let record = JournalEntry::Submitted(wes_engine::history::SubmissionRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        refreshed: vec![NodeId::new("id1000").unwrap()],
        document: None,
        revision_of: None,
        id: "receipt".into(),
        cell: "attempt".into(),
        text: ":env use \"default\"".into(),
        client: "pane".into(),
        context: None,
        order: 3,
        nodes: vec![],
        repeat: None,
        run: None,
    });
    let bytes = encode_journal(&record, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        record
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 1);
    for (field, value) in [
        ("cell", json!("")),
        ("order", json!(10000)),
        ("acknowledge_effects", json!(true)),
    ] {
        let mut invalid = wire.clone();
        invalid["entry"][field] = value;
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
    let mut old = wire;
    old["version"] = json!(4);
    assert!(decode_journal(&serde_json::to_vec(&old).unwrap(), Limits::default()).is_err());
}

#[test]
fn retained_result_reason_roundtrips_and_unknown_evidence_is_not_inferred() {
    use wes_engine::{
        history::RetainedResult,
        storage::{Retention, ValueHandle},
    };
    for retention in [
        Retention::Automatic,
        Retention::Protected,
        Retention::Unknown,
    ] {
        let record = JournalEntry::Result(RetainedResult {
            node: NodeId::new("id1").unwrap(),
            handle: ValueHandle::fresh(),
            run: RunId::new("run1").unwrap(),
            retention,
        });
        let bytes = encode_journal(&record, Limits::default()).unwrap();
        assert_eq!(
            decode_journal(&bytes, Limits::default()).unwrap().entry,
            record
        );
        let mut wire: Value = serde_json::from_slice(&bytes).unwrap();
        wire["entry"]["retention"] = json!("guessed-from-age");
        assert!(decode_journal(&serde_json::to_vec(&wire).unwrap(), Limits::default()).is_err());
    }
}

fn imported_command() -> JournalEntry {
    use wes_core::{Data, Provenance, Shape, Value as DomainValue};
    use wes_engine::{
        history::CommandRecord,
        imports::{ImportRecipe, ImportRequest, ImportSnapshot},
    };
    let value = DomainValue::new(
        Shape::Unknown,
        Data::Record(
            [
                ("bytes".into(), Data::Bytes(vec![0, 255].into())),
                ("scale".into(), Data::Decimal("1.2300".parse().unwrap())),
                ("precise".into(), Data::Int(i64::MAX)),
            ]
            .into(),
        ),
        Provenance::default()
            .with_fact("origin", "fixture")
            .cautioned(["unchecked:example".into()]),
    )
    .unwrap();
    JournalEntry::Command(CommandRecord {
        source_name: "fixture.wes".into(),
        source_start: wes_language::Position { line: 1, column: 1 },
        changed_nodes: vec![],
        document: None,
        revision_of: None,
        environments: None,
        cell: "imports".into(),
        text: ":import fixture payload:x".into(),
        replay: ":import fixture payload:x".into(),
        nodes: vec![],
        type_sources: Default::default(),
        calculation_package: None,
        imports: vec![ImportSnapshot::new(
            ImportRequest::new(
                "fixture".into(),
                Some("alias".into()),
                [("payload".into(), value)].into(),
            )
            .unwrap(),
            ImportRecipe::new("fixture/v1".into(), "\u{feff}exact \"JSON\"\r\n🦀".into()).unwrap(),
        )],
    })
}

#[test]
fn versioned_command_imports_preserve_exact_values_provenance_and_recipe_bytes() {
    let entry = imported_command();
    let encoded = encode_journal(&entry, Limits::default()).unwrap();
    let decoded = decode_journal(&encoded, Limits::default()).unwrap();

    assert_eq!(decoded.entry, entry);
    let escaped_tag = String::from_utf8(encoded.clone())
        .unwrap()
        .replace("\"record\":\"command\"", "\"record\":\"\\u0063ommand\"");
    assert_eq!(
        decode_journal(escaped_tag.as_bytes(), Limits::default())
            .unwrap()
            .entry,
        entry
    );
    let json: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(
        json["entry"]["imports"][0]["arguments"]["payload"]["format"],
        "wes.value"
    );
    assert_eq!(json["entry"]["imports"][0]["alias"], "alias");
    assert_eq!(
        encode_journal(&decoded.entry, Limits::default()).unwrap(),
        encoded
    );
    for limits in [
        Limits {
            bytes: encoded.len() - 1,
            ..Limits::default()
        },
        Limits {
            nodes: 1,
            ..Limits::default()
        },
    ] {
        assert!(encode_journal(&entry, limits).is_err());
        assert!(decode_journal(&encoded, limits).is_err());
    }
    let JournalEntry::Command(mut empty) = entry else {
        unreachable!()
    };
    empty.imports.clear();
    let value: Value = serde_json::from_slice(
        &encode_journal(&JournalEntry::Command(empty), Limits::default()).unwrap(),
    )
    .unwrap();
    assert!(value["entry"].get("imports").is_none());
}

#[test]
fn imported_argument_keys_that_match_serde_internals_remain_ordinary_record_fields() {
    use wes_core::{Data, Shape, Value as DomainValue};
    use wes_engine::imports::{ImportRequest, ImportSnapshot};
    let JournalEntry::Command(mut command) = imported_command() else {
        unreachable!()
    };
    let value = DomainValue::new(
        Shape::Unknown,
        Data::Record(
            [(
                "$serde_json::private::Number".into(),
                Data::Text("ordinary field".into()),
            )]
            .into(),
        ),
        Default::default(),
    )
    .unwrap();
    command.imports[0] = ImportSnapshot::new(
        ImportRequest::new("fixture".into(), None, [("reserved".into(), value)].into()).unwrap(),
        command.imports[0].recipe().clone(),
    );
    let entry = JournalEntry::Command(command);
    // Do not route this through serde_json::Value: its arbitrary_precision marker interpretation
    // is exactly why the production boundary preserves raw JSON and uses the exact value codec.
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
}

#[test]
fn import_history_rejects_legacy_ambiguity_conflicts_and_over_budget_values() {
    let duplicate =
        String::from_utf8(encode_journal(&imported_command(), Limits::default()).unwrap())
            .unwrap()
            .replace(
                "\"kind\":\"fixture\"",
                "\"kind\":\"fixture\",\"kind\":\"other\"",
            );
    assert!(decode_journal(duplicate.as_bytes(), Limits::default()).is_err());
    let original: Value =
        serde_json::from_slice(&encode_journal(&imported_command(), Limits::default()).unwrap())
            .unwrap();
    for import_field in [original["entry"]["imports"].clone(), json!([]), Value::Null] {
        let mut legacy = original["entry"].clone();
        legacy["imports"] = import_field;
        assert!(decode_journal(&serde_json::to_vec(&legacy).unwrap(), Limits::default()).is_err());
    }
    for (key, value) in [
        ("kind", json!(" ")),
        ("format", json!("")),
        ("extra", json!(true)),
        ("source", json!("x".repeat(1024 * 1024 + 1))),
        (
            "arguments",
            json!({"payload":{"type":"Unknown","data":"legacy"}}),
        ),
        ("arguments", json!({"payload":"not an encoded value"})),
    ] {
        let mut malformed = original.clone();
        malformed["entry"]["imports"][0][key] = value;
        assert!(
            decode_journal(&serde_json::to_vec(&malformed).unwrap(), Limits::default()).is_err(),
            "{key}"
        );
    }
    for key in ["kind", "arguments", "format", "source"] {
        let mut malformed = original.clone();
        malformed["entry"]["imports"][0]
            .as_object_mut()
            .unwrap()
            .remove(key);
        assert!(
            decode_journal(&serde_json::to_vec(&malformed).unwrap(), Limits::default()).is_err()
        );
    }
    let mut conflicting = original.clone();
    let mut second = conflicting["entry"]["imports"][0].clone();
    second["source"] = json!("different");
    conflicting["entry"]["imports"]
        .as_array_mut()
        .unwrap()
        .push(second);
    assert!(
        decode_journal(
            &serde_json::to_vec(&conflicting).unwrap(),
            Limits::default()
        )
        .is_err()
    );
    let mut excessive = original.clone();
    excessive["entry"]["imports"] = json!(vec![original["entry"]["imports"][0].clone(); 257]);
    assert!(decode_journal(&serde_json::to_vec(&excessive).unwrap(), Limits::default()).is_err());
    let mut oversized = original.clone();
    oversized["entry"]["imports"][0]["arguments"]["payload"] = serde_json::from_slice::<Value>(
        &wes_adapters::codec::encode_value(
            &wes_core::Value::new(
                wes_core::Shape::Unknown,
                wes_core::Data::Text("x".repeat(128 * 1024).into()),
                Default::default(),
            )
            .unwrap(),
            Limits::default(),
        )
        .unwrap(),
    )
    .unwrap();
    assert!(decode_journal(&serde_json::to_vec(&oversized).unwrap(), Limits::default()).is_err());
}

#[test]
fn versioned_operational_notices_round_trip_context_error_identity_issues_and_cause() {
    use wes_core::{ErrorId, ErrorValue, Timestamp, ValidationIssue};
    use wes_engine::{
        history::{NoticeContext, NoticeRecord},
        storage::ValueHandle,
    };
    let node = NodeId::new("historical-node").unwrap();
    let run = RunId::new("historical-run").unwrap();
    let handle = ValueHandle::fresh();
    let error = ErrorValue::new(
        ErrorId::new("original-error").unwrap(),
        "TEST001",
        "operational failure 🦀",
        vec![ValidationIssue {
            path: "/items/0".into(),
            code: "TEST002".into(),
            message: "original issue".into(),
        }],
        Some(ErrorId::new("original-cause").unwrap()),
    )
    .unwrap()
    .with_locations(vec![wes_core::SourceLocation {
        source: "fixture.wes".into(),
        start: 10,
        end: 13,
        line: 2,
        column: 3,
        end_line: 2,
        end_column: 6,
    }])
    .unwrap();
    let contexts = [
        NoticeContext::Execution {
            node: node.clone(),
            run: run.clone(),
        },
        NoticeContext::Publication {
            node: node.clone(),
            run: run.clone(),
            handle: None,
        },
        NoticeContext::Publication {
            node,
            run,
            handle: Some(handle.clone()),
        },
        NoticeContext::Keep {
            handle: handle.clone(),
            may_have_applied: false,
        },
        NoticeContext::Release {
            handle,
            may_have_applied: true,
        },
        NoticeContext::Eviction,
        NoticeContext::StorageWorker,
        NoticeContext::WorkspaceShutdown,
    ];
    for context in contexts {
        let entry = JournalEntry::Noticed(
            NoticeRecord::new(
                "notice-id".into(),
                Timestamp::new(0, 42).unwrap(),
                context,
                error.clone(),
            )
            .unwrap(),
        );
        let bytes = encode_journal(&entry, Limits::default()).unwrap();
        let decoded = decode_journal(&bytes, Limits::default()).unwrap();

        assert_eq!(decoded.entry, entry);
        let wire: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(wire["entry"]["record"], "notice");
        assert_eq!(wire["entry"]["value"]["error"]["id"], "original-error");
        assert!(decode_journal(wire["entry"].to_string().as_bytes(), Limits::default()).is_err());
        assert!(
            encode_journal(
                &entry,
                Limits {
                    bytes: 32,
                    ..Limits::default()
                }
            )
            .is_err()
        );
        assert!(
            encode_journal(
                &entry,
                Limits {
                    nodes: 0,
                    ..Limits::default()
                }
            )
            .is_err()
        );
    }
}

#[test]
fn operational_notice_contexts_reject_missing_inconsistent_and_invalid_identity_data() {
    use wes_engine::history::{NoticeContext, NoticeRecord};
    use wes_engine::runtime::RuntimeCode;
    let entry = JournalEntry::Noticed(
        NoticeRecord::new(
            "event".into(),
            wes_core::Timestamp::new(0, 0).unwrap(),
            NoticeContext::Execution {
                node: NodeId::new("node").unwrap(),
                run: RunId::new("run").unwrap(),
            },
            RuntimeCode::RecordingFailed.error("failure", None),
        )
        .unwrap(),
    );
    let wire: Value =
        serde_json::from_slice(&encode_journal(&entry, Limits::default()).unwrap()).unwrap();
    for context in [
        json!({"kind":"execution","node":"node"}),
        json!({"kind":"execution","node":"node","run":" "}),
        json!({"kind":"eviction","node":"node"}),
        json!({"kind":"keep","handle":"../../other","mayHaveApplied":true}),
        json!({"kind":"unknown"}),
    ] {
        let mut invalid = wire.clone();
        invalid["entry"]["value"]["context"] = context;
        assert!(decode_journal(invalid.to_string().as_bytes(), Limits::default()).is_err());
    }
    let mut missing = wire.clone();
    missing["entry"]["value"]
        .as_object_mut()
        .unwrap()
        .remove("error");
    assert!(decode_journal(missing.to_string().as_bytes(), Limits::default()).is_err());
    let mut invalid = wire;
    invalid["entry"]["value"]["id"] = json!("");
    assert!(decode_journal(invalid.to_string().as_bytes(), Limits::default()).is_err());
}

#[test]
fn historical_unwrapped_records_are_rejected() {
    for fixture in fixtures() {
        let bytes = fixture["source"].as_str().unwrap().as_bytes();
        assert!(if fixture["kind"] == "journal" {
            decode_journal(bytes, Limits::default()).is_err()
        } else {
            decode_recovery(bytes, Limits::default()).is_err()
        });
    }
}

#[test]
fn modern_recovery_round_trips_generation_safe_calls() {
    let node = NodeId::new("id1000").unwrap();
    let run = RunId::new("run-with-identity").unwrap();
    for entry in [
        RecoveryEntry::Accepted {
            cell: "cell-a".into(),
        },
        RecoveryEntry::Calling(CallRecord {
            node: node.clone(),
            run: run.clone(),
            cell: "cell-a".into(),
            capability: "catalog create".into(),
            safe: false,
            at: "2026-09-10T12:00:00.123456789Z".parse().unwrap(),
        }),
        RecoveryEntry::Called {
            node,
            run,
            produced: false,
        },
    ] {
        let encoded = encode_recovery(&entry, Limits::default()).unwrap();
        let decoded = decode_recovery(&encoded, Limits::default()).unwrap();
        assert_eq!(decoded.entry, entry);

        let mut wire: Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(wire["format"], "wes.recovery");
        assert_eq!(wire["version"], 1);
        wire["format"] = json!("unrelated.recovery");
        assert!(decode_recovery(&serde_json::to_vec(&wire).unwrap(), Limits::default()).is_err());

        assert!(decode_journal(&encoded, Limits::default()).is_err());
    }
}

#[test]
fn retained_and_recovery_records_require_nonempty_run_identity() {
    let result = JournalEntry::Result(wes_engine::history::RetainedResult {
        node: NodeId::new("node").unwrap(),
        run: RunId::new("run").unwrap(),
        handle: wes_engine::storage::ValueHandle::fresh(),
        retention: wes_engine::storage::Retention::Protected,
    });
    let wire: Value =
        serde_json::from_slice(&encode_journal(&result, Limits::default()).unwrap()).unwrap();
    assert_eq!(wire["format"], "wes.journal");
    assert_eq!(wire["version"], 1);
    let mut old = wire.clone();
    old["format"] = json!("unrelated.journal");
    assert!(decode_journal(&serde_json::to_vec(&old).unwrap(), Limits::default()).is_err());
    for version in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let mut invalid = wire.clone();
        invalid["version"] = json!(version);
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
    for run in [None, Some(json!("")), Some(json!(null)), Some(json!("  "))] {
        let mut invalid = wire.clone();
        match &run {
            Some(value) => {
                invalid["entry"]["run"] = value.clone();
            }
            None => {
                invalid["entry"].as_object_mut().unwrap().remove("run");
            }
        }
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
        for entry in [
            json!({"record":"calling","node":"node","run":"run","cell":"cell","capability":"qa get","safe":true,"at":"2026-09-28T12:00:00Z"}),
            json!({"record":"called","node":"node","run":"run","produced":true}),
        ] {
            let mut record = json!({"format":"wes.recovery","version":1,"entry":entry});
            assert!(
                decode_recovery(&serde_json::to_vec(&record).unwrap(), Limits::default()).is_ok()
            );
            match &run {
                Some(value) => {
                    record["entry"]["run"] = value.clone();
                }
                None => {
                    record["entry"].as_object_mut().unwrap().remove("run");
                }
            }
            assert!(
                decode_recovery(&serde_json::to_vec(&record).unwrap(), Limits::default()).is_err()
            );
        }
    }
}

#[test]
fn diagnostics_preserve_utf16_wire_positions_without_splitting_unicode() {
    let fixture = fixtures()
        .into_iter()
        .find(|f| f["normalized"]["record"] == "diagnostic")
        .unwrap();
    let mut record = json!({"format":"wes.journal","version":1,"entry":fixture["normalized"]});
    let decoded = decode_journal(record.to_string().as_bytes(), Limits::default()).unwrap();
    let JournalEntry::Diagnosed(diagnostic) = decoded.entry else {
        panic!("diagnostic expected")
    };
    assert_eq!(diagnostic.diagnostic().code, "FUTURE999");
    assert_eq!(diagnostic.diagnostic().span.start(), 11);
    assert_eq!(diagnostic.diagnostic().span.end(), 15);
    record["entry"]["value"]["diagnostic"]["start"] = json!(6); // Middle of the emoji's UTF-16 pair.
    assert!(decode_journal(record.to_string().as_bytes(), Limits::default()).is_err());
    record["entry"]["value"]["diagnostic"]["start"] = json!(9);
    record["entry"]["value"]["diagnostic"]["end"] = json!(100);
    assert!(decode_journal(record.to_string().as_bytes(), Limits::default()).is_err());
}

#[test]
fn malformed_or_ambiguous_records_fail_instead_of_dropping_fields() {
    for source in [
        r#"{"record":"command","text":"x","nodes":[1]}"#,
        r#"{"record":"command","text":"x","nodes":["id1000","id1000"]}"#,
        r#"{"record":"command","text":"x","nodes":[],"typeSources":{"a":"one","a":"two"}}"#,
        r#"{"record":"command","text":"x","text":"y","nodes":[]}"#,
        r#"{"record":"command","text":"\ud800","nodes":[]}"#,
        r#"{"record":"unknown","text":"x","nodes":[]}"#,
        r#"{"record":"command","text":"x","nodes":[],"future":{}}"#,
        r#"{"format":"wes.journal","version":999,"entry":{"record":"command","text":"x","nodes":[]}}"#,
        r#"{"record":"observation","value":{"id":"x","node":"id1000","run":"r","at":"2026-09-10T00:00:00Z","state":"FAILED","error":[]}}"#,
        r#"{"record":"observation","value":{"id":"x","node":"id1000","run":"r","at":"2026-09-10T00:00:00Z","state":"READY","error":[1]}}"#,
    ] {
        assert!(
            decode_journal(source.as_bytes(), Limits::default()).is_err(),
            "{source}"
        );
    }
}

#[test]
fn record_byte_node_depth_and_output_limits_are_explicit() {
    let source = br#"{"format":"wes.journal","version":1,"entry":{"record":"command","source_name":"fixture.wes","source_start":[1,1],"cell":"cell","text":"x","nodes":[],"changedNodes":[]}}"#;
    let limits = Limits {
        bytes: source.len(),
        nodes: 13, // Includes the source-start pair and its two numeric values.
    };
    let entry = decode_journal(source, limits).unwrap().entry;
    assert!(matches!(
        decode_journal(
            source,
            Limits {
                bytes: source.len() - 1,
                ..limits
            }
        ),
        Err(CodecError::Bytes)
    ));
    assert!(matches!(
        decode_journal(source, Limits { nodes: 3, ..limits }),
        Err(CodecError::Work)
    ));
    let encoded = encode_journal(&entry, Limits::default()).unwrap();
    assert!(matches!(
        encode_journal(
            &entry,
            Limits {
                bytes: encoded.len() - 1,
                ..Limits::default()
            }
        ),
        Err(CodecError::Bytes)
    ));
    let source = format!("{}0{}", "[".repeat(10_000), "]".repeat(10_000));
    assert!(matches!(
        decode_journal(source.as_bytes(), Limits::default()),
        Err(CodecError::Depth)
    ));
}

#[test]
fn json_lines_stream_with_exact_offsets_across_short_reads_and_crlf() {
    let first = br#"{"format":"wes.journal","version":1,"entry":{"record":"command","source_name":"fixture.wes","source_start":[1,1],"cell":"cell","text":"line\nbreak","nodes":[],"changedNodes":[]}}"#;
    let second = br#"{"format":"wes.journal","version":1,"entry":{"record":"command","source_name":"fixture.wes","source_start":[1,1],"cell":"cell","text":"second","nodes":[],"changedNodes":[]}}"#;
    let mut bytes = b"\n \t\r\n".to_vec();
    let offset = bytes.len() as u64;
    bytes.extend(first);
    bytes.extend(b"\r\n");
    let second_offset = bytes.len() as u64;
    bytes.extend(second);
    let records = read_journal(
        BufReader::with_capacity(3, Cursor::new(&bytes)),
        ReadLimits::default(),
    )
    .collect::<Result<Vec<_>, _>>()
    .unwrap();
    assert_eq!(records.len(), 2);
    assert_eq!(
        (records[0].line, records[0].offset, records[0].end_offset),
        (3, offset, second_offset)
    );
    assert!(records[0].terminated);
    assert_eq!(
        (records[1].line, records[1].offset, records[1].end_offset),
        (4, second_offset, bytes.len() as u64)
    );
    assert!(!records[1].terminated);
}

#[test]
fn a_torn_tail_is_distinct_from_corruption_and_iteration_stops_at_both() {
    let good = "{\"format\":\"wes.journal\",\"version\":1,\"entry\":{\"record\":\"command\",\"source_name\":\"fixture.wes\",\"source_start\":[1,1],\"cell\":\"cell\",\"text\":\"x\",\"nodes\":[],\"changedNodes\":[]}}\n";
    for (bad, torn) in [
        ("{\"record\":", true),
        ("{\"record\":\n", false),
        ("{oops", false),
        ("{}", false),
    ] {
        let text = format!("{good}{bad}");
        let mut reader = read_journal(Cursor::new(text.as_bytes()), ReadLimits::default());
        assert!(reader.next().unwrap().is_ok());
        let error = reader.next().unwrap().unwrap_err();
        assert_eq!(error.is_torn_tail(), torn, "{bad}");
        assert!(reader.next().is_none());
        assert!(reader.next().is_none());
    }
    let text = format!("{good}{{oops}}\n{good}");
    let records = read_journal(Cursor::new(text), ReadLimits::default()).collect::<Vec<_>>();
    assert_eq!(records.len(), 2); // No silent skip to the third line.
    assert!(records[1].is_err());
}

#[test]
fn blank_lines_and_stream_growth_cannot_bypass_read_limits() {
    let limits = ReadLimits {
        lines: 2,
        ..ReadLimits::default()
    };
    assert!(matches!(
        read_journal(Cursor::new("\n\n\n"), limits).next().unwrap(),
        Err(ReadError::Limit {
            budget: "line count",
            ..
        })
    ));
    let limits = ReadLimits {
        bytes: 2,
        ..ReadLimits::default()
    };
    assert!(matches!(
        read_journal(Cursor::new("   "), limits).next().unwrap(),
        Err(ReadError::Limit {
            budget: "total byte",
            ..
        })
    ));
    let limits = ReadLimits {
        record: Limits {
            bytes: 2,
            ..Limits::default()
        },
        ..ReadLimits::default()
    };
    assert!(matches!(
        read_journal(Cursor::new("xxx"), limits).next().unwrap(),
        Err(ReadError::Limit {
            budget: "record byte",
            ..
        })
    ));
    assert!(
        read_journal(Cursor::new("\u{000b}"), ReadLimits::default())
            .next()
            .unwrap()
            .is_err()
    );
    assert!(
        read_recovery(
            Cursor::new("{\"format\":\"wes.recovery\",\"version\":1,\"entry\":{\"record\":\"accepted\",\"cell\":\"a\"}}\n"),
            ReadLimits::default()
        )
        .next()
        .unwrap()
        .is_ok()
    );
}

#[test]
fn io_failure_is_not_reported_as_an_empty_or_torn_journal() {
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("read failed"))
        }
    }
    let mut reader = read_journal(BufReader::new(Broken), ReadLimits::default());
    let error = reader.next().unwrap().unwrap_err();
    assert!(!error.is_torn_tail());
    assert!(matches!(
        error,
        ReadError::Io {
            line: 1,
            offset: 0,
            ..
        }
    ));
    assert!(reader.next().is_none());
}

#[test]
fn trace_journal_requires_current_schema_matching_identities_and_public_closed_evidence() {
    use wes_core::{Data, Provenance, Shape, Value as TypedValue};
    use wes_engine::trace::{TraceRecord, integer, record, text};
    let value = record(
        "Trace",
        [
            ("schema".into(), integer(1)),
            ("node".into(), text("n")),
            ("run".into(), text("r")),
            ("profile".into(), text("http")),
            ("state".into(), text("failed")),
            ("persistence".into(), text("recorded")),
            ("dropped".into(), integer(0)),
            (
                "events".into(),
                TypedValue::new(
                    Shape::List(Box::new(Shape::Unknown)),
                    Data::List(vec![]),
                    Provenance::default(),
                )
                .unwrap(),
            ),
        ],
        Provenance::default(),
    );
    let record = TraceRecord {
        node: NodeId::new("n").unwrap(),
        run: RunId::new("r").unwrap(),
        value,
    };
    for profile in ["grpc", "binary"] {
        let mut selected = record.clone();
        let Data::Record(mut fields) = record.value.data().clone() else {
            panic!()
        };
        fields.insert("profile".into(), Data::Text(profile.into()));
        selected.value = TypedValue::new(
            record.value.shape().clone(),
            Data::Record(fields),
            record.value.provenance().clone(),
        )
        .unwrap();
        let entry = JournalEntry::Trace(selected);
        let bytes = encode_journal(&entry, Limits::default()).unwrap();
        assert_eq!(
            decode_journal(&bytes, Limits::default()).unwrap().entry,
            entry
        );
    }
    let entry = JournalEntry::Trace(record.clone());
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let json: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(json["version"], 1);
    for version in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let mut legacy = json.clone();
        legacy["version"] = json!(version);
        assert!(decode_journal(&serde_json::to_vec(&legacy).unwrap(), Limits::default()).is_err());
    }
    let mut mismatched = record.clone();
    mismatched.run = RunId::new("other").unwrap();
    assert!(encode_journal(&JournalEntry::Trace(mismatched), Limits::default()).is_err());
    let mut private = record.clone();
    private.value = private.value.clone().with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    assert!(encode_journal(&JournalEntry::Trace(private), Limits::default()).is_err());
    let mut malformed = json;
    malformed["entry"]["value"] = json!("{}");
    assert!(decode_journal(&serde_json::to_vec(&malformed).unwrap(), Limits::default()).is_err());
}

#[test]
fn protected_run_roundtrips_without_becoming_a_restore_result() {
    let entry = JournalEntry::ProtectedRun {
        node: wes_engine::graph::NodeId::new("id1").unwrap(),
        run: wes_engine::runtime::RunId::new("run1").unwrap(),
        handle: wes_engine::storage::ValueHandle::fresh(),
    };
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let entries = [entry];
    let index = wes_engine::history::RestoreIndex::build(&entries);
    assert!(index.retained.is_empty());
}

#[test]
fn public_diagnostic_explanations_are_optional_bounded_and_round_trip() {
    let mut record = fixtures()
        .into_iter()
        .find(|f| f["normalized"]["record"] == "diagnostic")
        .unwrap()["normalized"]
        .clone();
    record = json!({"format":"wes.journal","version":1,"entry":record});
    let old = decode_journal(record.to_string().as_bytes(), Limits::default()).unwrap();
    let JournalEntry::Diagnosed(old_diagnostic) = &old.entry else {
        panic!("diagnostic")
    };
    assert!(old_diagnostic.diagnostic().public_message.is_none());
    assert!(
        old_diagnostic
            .diagnostic()
            .public_summary()
            .contains("not exportable")
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&encode_journal(&old.entry, Limits::default()).unwrap())
            .unwrap()["entry"],
        record["entry"]
    );
    record["entry"]["value"]["diagnostic"]["public_message"] =
        json!("Unexpected positional word. Expected name:<environment>.");
    let new = decode_journal(record.to_string().as_bytes(), Limits::default()).unwrap();
    let JournalEntry::Diagnosed(new_diagnostic) = &new.entry else {
        panic!("diagnostic")
    };
    assert_eq!(
        new_diagnostic.diagnostic().public_summary(),
        "Unexpected positional word. Expected name:<environment>."
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&encode_journal(&new.entry, Limits::default()).unwrap())
            .unwrap()["entry"],
        record["entry"]
    );
    for invalid in ["".into(), " ".into(), "ç".repeat(257)] {
        record["entry"]["value"]["diagnostic"]["public_message"] = json!(invalid);
        assert!(decode_journal(record.to_string().as_bytes(), Limits::default()).is_err());
    }
    record["entry"]["value"]["diagnostic"]["public_message"] = json!("ç".repeat(256));
    assert!(decode_journal(record.to_string().as_bytes(), Limits::default()).is_ok());
}

#[test]
fn live_snapshot_is_atomic_bounded_and_validates_its_retention_identity() {
    use wes_engine::{
        graph::NodeState,
        history::{ExecutionRecord, LiveSnapshot, RestoreIndex, RetainedResult},
        storage::{Retention, ValueHandle},
    };
    let node = NodeId::new("id2").unwrap();
    let run = RunId::new("event-run").unwrap();
    let snapshot = LiveSnapshot {
        source: NodeId::new("id1").unwrap(),
        epoch: RunId::new("source-run").unwrap(),
        delivery: Some(42),
        observation: ExecutionRecord::new(
            "checkpoint-observation".into(),
            node.clone(),
            Some(run.clone()),
            "2026-09-26T00:00:00Z".parse().unwrap(),
            NodeState::Ready,
            None,
        )
        .unwrap(),
        result: Some(RetainedResult {
            node: node.clone(),
            run,
            handle: ValueHandle::fresh(),
            retention: Retention::Protected,
        }),
    };
    let record = JournalEntry::Snapshot(snapshot.clone());
    let bytes = encode_journal(&record, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        record
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(wire["version"], 1);
    for version in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10] {
        let mut bad = wire.clone();
        bad["version"] = json!(version);
        assert!(decode_journal(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    }
    let mut bad = wire.clone();
    bad["entry"]["epoch"] = json!("");
    assert!(decode_journal(&serde_json::to_vec(&bad).unwrap(), Limits::default()).is_err());
    let mut bad = snapshot.clone();
    bad.result.as_mut().unwrap().node = NodeId::new("wrong").unwrap();
    assert!(encode_journal(&JournalEntry::Snapshot(bad), Limits::default()).is_err());
    let receipt = wes_engine::history::AppendReceipt {
        persistence: wes_engine::history::Persistence::FileSynced,
        end_offset: 1,
    };
    let mut page = wes_engine::history::HistoryPage::new(receipt);
    page.push(
        record.clone(),
        wes_engine::history::HistoryPageLimits::default(),
    )
    .unwrap();
    assert_eq!(page.entries()[0].entry, record);
    let mut automatic = snapshot.clone();
    automatic.result.as_mut().unwrap().retention = Retention::Automatic;
    let upgraded = [
        JournalEntry::Snapshot(automatic),
        JournalEntry::Result(snapshot.result.clone().unwrap()),
    ];
    assert_eq!(
        RestoreIndex::build(&upgraded).retained[&node].retention,
        Retention::Protected
    );
    let mut unavailable = snapshot.clone();
    unavailable.result = None;
    let entries = [record, JournalEntry::Snapshot(unavailable)];
    assert!(RestoreIndex::build(&entries).retained.is_empty());
    let mut limits = Limits::default();
    limits.bytes = 32;
    assert!(encode_journal(&entries[0], limits).is_err());
}

#[test]
fn stale_reason_roundtrips_and_old_journal_schemas_are_rejected() {
    use wes_engine::{graph::NodeState, history::ExecutionRecord, runtime::StaleReason};
    let entry = JournalEntry::Observed(
        ExecutionRecord::new(
            "e".into(),
            NodeId::new("n").unwrap(),
            None,
            "2026-09-27T10:00:00Z".parse().unwrap(),
            NodeState::Stale,
            None,
        )
        .unwrap()
        .with_stale_reason(Some(StaleReason::DependencyChanged))
        .unwrap(),
    );
    let encoded = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&encoded, Limits::default()).unwrap().entry,
        entry
    );
    let mut legacy: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(legacy["version"], 1);
    legacy["version"] = json!(10);
    // The observation wire embeds the execution record as `value`.
    legacy["entry"]["value"]
        .as_object_mut()
        .unwrap()
        .remove("stale_reason");
    assert!(decode_journal(&serde_json::to_vec(&legacy).unwrap(), Limits::default()).is_err());
    legacy["version"] = json!(1);
    legacy["entry"]["value"]["stale_reason"] = json!("dependency_changed");
    legacy["entry"]["value"]["state"] = json!("READY");
    assert!(decode_journal(&serde_json::to_vec(&legacy).unwrap(), Limits::default()).is_err());
}

#[test]
fn command_mutation_evidence_is_required_unique_and_round_trips() {
    let JournalEntry::Command(mut command) = imported_command() else {
        panic!()
    };
    command.changed_nodes = vec![
        NodeId::new("changed-a").unwrap(),
        NodeId::new("changed-b").unwrap(),
    ];
    let entry = JournalEntry::Command(command.clone());
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    for targets in [
        None,
        Some(json!(null)),
        Some(json!(["changed-a", "changed-a"])),
        Some(json!([""])),
    ] {
        let mut invalid = wire.clone();
        match targets {
            Some(targets) => invalid["entry"]["changedNodes"] = targets,
            None => {
                invalid["entry"]
                    .as_object_mut()
                    .unwrap()
                    .remove("changedNodes");
            }
        }
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
    command.changed_nodes.push(command.changed_nodes[0].clone());
    assert!(encode_journal(&JournalEntry::Command(command), Limits::default()).is_err());
}

#[test]
fn view_checkpoint_codec_preserves_typed_metadata_and_rejects_private_or_invalid_images() {
    let record = wes_engine::views::ViewRecord {
        id: "40000000-0000-4000-8000-000000000001".into(),
        value: wes_core::Value::new(
            wes_core::Shape::Unknown,
            wes_core::Data::List(vec![]),
            wes_core::Provenance::default(),
        )
        .unwrap(),
    };
    let entry = JournalEntry::Views(record.clone());
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let mut invalid = record.clone();
    invalid.id = "invalid".into();
    assert!(encode_journal(&JournalEntry::Views(invalid), Limits::default()).is_err());
    let mut private = record.clone();
    private.value = private.value.with_provenance(
        wes_core::Provenance::default()
            .with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    assert!(encode_journal(&JournalEntry::Views(private), Limits::default()).is_err());
    let mut invalid = record;
    invalid.value = wes_core::Value::new(
        wes_core::Shape::Unknown,
        wes_core::Data::List(vec![wes_core::Data::Int(1)]),
        wes_core::Provenance::default(),
    )
    .unwrap();
    assert!(encode_journal(&JournalEntry::Views(invalid), Limits::default()).is_err());
}

#[test]
fn sequential_request_identity_roundtrips_and_rejects_malformed_step_sets() {
    let record = wes_engine::history::RequestRecord {
        namespace: "pane".into(),
        request: "request".into(),
        cell: "root".into(),
        fingerprint: "a".repeat(64),
        steps: vec!["root".into(), "second".into()],
    };
    let entry = JournalEntry::Requested(record);
    let bytes = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&bytes, Limits::default()).unwrap().entry,
        entry
    );
    let wire: Value = serde_json::from_slice(&bytes).unwrap();
    for steps in [
        json!(["other"]),
        json!(["root", "root"]),
        json!(["root", ""]),
        json!(["root", "bad\nstep"]),
        json!(vec!["root"; 65]),
    ] {
        let mut invalid = wire.clone();
        invalid["entry"]["steps"] = steps;
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
}

#[test]
fn applied_import_origin_and_frozen_scalar_provenance_round_trip_without_authority() {
    use wes_core::{Data, Primitive, Provenance, Shape, Value as DomainValue};
    use wes_engine::imports::{ImportOrigin, ImportRecipe, ImportRequest, ImportSnapshot};
    let JournalEntry::Command(mut command) = imported_command() else {
        panic!()
    };
    command.text = ":import apply $plan".into();
    command.replay = command.text.clone();
    command.imports = vec![ImportSnapshot::from_origin(
        ImportRequest::new(
            "fixture".into(),
            Some("library".into()),
            [(
                "file".into(),
                DomainValue::new(
                    Shape::Primitive(Primitive::Text),
                    Data::Text("synthetic".into()),
                    Provenance::default()
                        .with_fact("format", "fixture")
                        .cautioned(["synthetic caution".into()]),
                )
                .unwrap(),
            )]
            .into(),
        )
        .unwrap(),
        ImportRecipe::new("fixture/v1".into(), "synthetic".into()).unwrap(),
        ImportOrigin::Applied,
    )];
    let entry = JournalEntry::Command(command);
    let encoded = encode_journal(&entry, Limits::default()).unwrap();
    assert_eq!(
        decode_journal(&encoded, Limits::default()).unwrap().entry,
        entry
    );
    let wire: Value = serde_json::from_slice(&encoded).unwrap();
    assert_eq!(wire["entry"]["imports"][0]["origin"], "applied");
    for origin in [Some(json!("unknown")), Some(Value::Null)] {
        let mut invalid = wire.clone();
        match origin {
            None => {
                invalid["entry"]["imports"][0]
                    .as_object_mut()
                    .unwrap()
                    .remove("origin");
            }
            Some(origin) => invalid["entry"]["imports"][0]["origin"] = origin,
        }
        assert!(decode_journal(&serde_json::to_vec(&invalid).unwrap(), Limits::default()).is_err());
    }
}
