use std::sync::Arc;
use wes_core::{
    Data,
    contracts::{
        Contract, ContractField, ContractRegistry, ResolvedContractBundle, SnapshotError,
        SnapshotLimits,
    },
};

fn bundle(source: &str, name: &str) -> ResolvedContractBundle {
    let mut registry = ContractRegistry::new();
    registry.load(source).unwrap();
    ResolvedContractBundle::capture(registry.resolve(name).unwrap(), SnapshotLimits::default())
        .unwrap()
}

#[test]
fn full_constraints_optional_paths_and_tones_survive_without_registry() {
    let snapshot = bundle(
        "version: 2\ntypes: {Status: {base: Text, enum: [ready, failed], display: {enumTones: {ready: ok, failed: bad}}}, Code: {base: Int, min: 1, max: 9}, Row: {base: Record, fields: {status: Status, 'a/b': Code, note: {type: 'Option<Text>', optional: true}}}}",
        "Row",
    );
    let root = snapshot.root();
    let good = Data::Record(
        [
            ("status".into(), Data::Text("ready".into())),
            ("a/b".into(), Data::Int(3)),
        ]
        .into(),
    );
    assert!(root.issues(&good).is_empty());
    let bad = Data::Record(
        [
            ("status".into(), Data::Text("unknown".into())),
            ("a/b".into(), Data::Int(10)),
        ]
        .into(),
    );
    let issues = root.issues(&bad);
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[1].path, "/a~1b");
    let restored =
        ResolvedContractBundle::decode(snapshot.encoded(), SnapshotLimits::default()).unwrap();
    assert_eq!(restored.digest(), snapshot.digest());
    assert_eq!(restored.root().issues(&bad), root.issues(&bad));
    assert_eq!(restored.encoded(), snapshot.encoded());
}

#[test]
fn snapshots_retain_full_enum_beyond_display_preview_and_exact_scalars() {
    let values = (0..200)
        .map(|n| format!("s{n}"))
        .collect::<Vec<_>>()
        .join(",");
    let snapshot = bundle(
        &format!(
            "types: {{Choice: {{base: Text, enum: [{values}]}}, Amount: {{base: Decimal, enum: [1.00, 9223372036854775808.01]}}, Row: {{base: Record, fields: {{choice: Choice, amount: Amount}}}}}}"
        ),
        "Row",
    );
    let row = |choice: &str, amount: &str| {
        Data::Record(
            [
                ("choice".into(), Data::Text(choice.into())),
                ("amount".into(), Data::Decimal(amount.parse().unwrap())),
            ]
            .into(),
        )
    };
    assert!(snapshot.root().issues(&row("s199", "1.00")).is_empty());
    assert!(!snapshot.root().issues(&row("outside", "1.0")).is_empty());
    let restored =
        ResolvedContractBundle::decode(snapshot.encoded(), SnapshotLimits::default()).unwrap();
    assert!(
        restored
            .root()
            .issues(&row("s199", "9223372036854775808.01"))
            .is_empty()
    );
}

#[test]
fn aliases_base_identity_patterns_and_containers_roundtrip() {
    let snapshot = bundle(
        "types: {Tag: {base: Text, minLength: 2, maxLength: 4, pattern: '^[a-z]+$'}, Narrow: {base: Tag, minLength: 3}, Entries: {base: 'List<Option<Narrow>>', maxItems: 3}, Table: {base: Record, fields: {entries: Entries, lookup: 'Map<Text,Int>', mixed: 'Union<Int,Text>'}}}",
        "Table",
    );
    let restored =
        ResolvedContractBundle::decode(snapshot.encoded(), SnapshotLimits::default()).unwrap();
    assert_eq!(restored.digest(), snapshot.digest());
    let list = bundle(
        "types: {Tag: {base: Text, pattern: '^[a-z]+$'}, Entries: {base: 'List<Option<Tag>>', maxItems: 2}}",
        "Entries",
    );
    assert!(
        !list
            .root()
            .issues(&Data::List(vec![Data::Option(Some(Box::new(Data::Text(
                "123".into()
            ))))]))
            .is_empty()
    );
}

#[test]
fn malformed_digests_forward_refs_versions_extra_nodes_and_duplicate_keys_refuse() {
    let original = bundle("types: {Row: {base: Record, fields: {n: Int}}}", "Row");
    let wire: serde_json::Value = serde_json::from_slice(original.encoded()).unwrap();
    for change in 0..5 {
        let mut changed = wire.clone();
        match change {
            0 => changed["version"] = serde_json::json!(2),
            1 => {
                changed["nodes"][0]["digest"] =
                    serde_json::json!(format!("sha256:{}", "0".repeat(64)))
            }
            2 => changed["nodes"].as_array_mut().unwrap().reverse(),
            3 => {
                let node = changed["nodes"][0].clone();
                changed["nodes"].as_array_mut().unwrap().push(node);
            }
            4 => changed["root"] = serde_json::json!(format!("sha256:{}", "f".repeat(64))),
            _ => unreachable!(),
        }
        assert!(
            ResolvedContractBundle::decode(
                &serde_json::to_vec(&changed).unwrap(),
                SnapshotLimits::default()
            )
            .is_err(),
            "change {change}"
        );
    }
    let text = String::from_utf8(original.encoded().to_vec()).unwrap();
    let duplicate = text.replacen("\"version\":1", "\"version\":1,\"version\":1", 1);
    assert!(
        ResolvedContractBundle::decode(duplicate.as_bytes(), SnapshotLimits::default()).is_err()
    );
}

#[test]
fn captured_schema_budgets_bound_depth_nodes_bytes_and_native_pattern_work() {
    let snapshot = bundle(
        "types: {Tag: {base: Text, pattern: '^[a-z]+$'}, Row: {base: Record, fields: {tag: 'Option<List<Tag>>'}}}",
        "Row",
    );
    for limits in [
        SnapshotLimits {
            bytes: 64,
            ..Default::default()
        },
        SnapshotLimits {
            nodes: 2,
            ..Default::default()
        },
        SnapshotLimits {
            depth: 1,
            ..Default::default()
        },
        SnapshotLimits {
            patterns: 0,
            ..Default::default()
        },
    ] {
        assert!(ResolvedContractBundle::decode(snapshot.encoded(), limits).is_err());
    }
    let mut deep = Arc::new(
        ContractRegistry::new()
            .resolve("Int")
            .unwrap()
            .as_ref()
            .clone(),
    );
    for n in 0..10 {
        deep = Arc::new(
            Contract::record(
                &format!("Row{n}"),
                [(
                    "next".into(),
                    ContractField {
                        contract: deep,
                        optional: false,
                    },
                )]
                .into(),
            )
            .unwrap(),
        );
    }
    assert_eq!(
        ResolvedContractBundle::capture(
            deep,
            SnapshotLimits {
                depth: 4,
                ..Default::default()
            }
        )
        .unwrap_err(),
        SnapshotError::Limit("depth")
    );
}
