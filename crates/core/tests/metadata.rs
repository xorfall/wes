use serde_json::{Value as Json, json};
use wes_core::{
    Data, Provenance, Shape, Value,
    contracts::{ContractRegistry, boundary, metadata::ValueMetadata},
};
fn capture(yaml: &str, name: &str) -> Json {
    let mut r = ContractRegistry::new();
    r.load(yaml).unwrap();
    let m = ValueMetadata::capture(&r.resolve(name).unwrap());
    m.validate().unwrap();
    serde_json::to_value(m).unwrap()
}
#[test]
fn declaration_paths_cover_empty_lists_options_optional_fields_and_escaped_keys() {
    let m = capture(
        "types: {Status: {base: Text, enum: [ready, failed]}, Row: {base: Record, fields: {'a/b~': Status, e: 'Option<Status>', missing: {type: Status, optional: true}}}}",
        "List<Row>",
    );
    assert_eq!(
        m["fields"]["/e/f:a~1b~0"]["members"],
        json!(["ready", "failed"])
    );
    assert_eq!(
        m["fields"]["/e/f:e/o"]["members"],
        json!(["ready", "failed"])
    );
    assert_eq!(m["fields"]["/e/f:missing"]["kind"], "text");
    assert!(m["fields"]["/e/f:e"].get("members").is_none());
    assert_eq!(m["fields"]["/e"]["contract"]["name"], "Row");
    let meta: ValueMetadata = serde_json::from_value(m).unwrap();
    let v = Value::new(
        Shape::List(Box::new(Shape::Unknown)),
        Data::List(vec![]),
        Provenance::default(),
    )
    .unwrap()
    .with_metadata(Some(meta.clone()));
    assert_eq!(
        v.with_provenance(Provenance::default()).metadata(),
        Some(&meta)
    );
    let sub = serde_json::to_value(meta.project("/e/f:e/o").unwrap()).unwrap();
    assert_eq!(sub["contract"]["name"], "Status");
    assert!(sub["fields"].get("").is_some());
    assert!(meta.project("/e/f:unobserved").is_none());
}
#[test]
fn exact_domains_have_bounded_previews_and_full_deduplicated_counts() {
    for count in [64, 65, 200] {
        let values = (0..count)
            .map(|i| format!("s{i}"))
            .collect::<Vec<_>>()
            .join(",");
        let m = capture(
            &format!("types: {{S: {{base: Text, enum: [{values},s0]}}}}"),
            "S",
        );
        assert_eq!(m["fields"][""]["total"], count);
        assert_eq!(
            m["fields"][""]["members"].as_array().unwrap().len(),
            count.min(64)
        );
        assert_eq!(m["fields"][""]["complete"], count == 64);
    }
    let m = capture(
        "types: {I: {base: Int, enum: [9223372036854775807]}, D: {base: Decimal, enum: [1.23000000000000000001]}, B: {base: Bool, enum: [true, false]}, R: {base: Record, fields: {i: I, d: D, b: B}}}",
        "R",
    );
    assert_eq!(
        m["fields"]["/f:i"]["members"],
        json!(["9223372036854775807"])
    );
    assert_eq!(
        m["fields"]["/f:d"]["members"],
        json!(["1.23000000000000000001"])
    );
    assert_eq!(m["fields"]["/f:b"]["members"], json!(["true", "false"]));
    let text = "ğ\"\\".repeat(5000);
    let yaml = format!(
        "types: {{S: {{base: Text, enum: [{}]}}}}",
        serde_json::to_string(&text).unwrap()
    );
    let m = capture(&yaml, "S");
    assert!(serde_json::to_vec(&m["fields"][""]).unwrap().len() <= 16 * 1024);
    assert_eq!(m["fields"][""]["complete"], false);
}
#[test]
fn descriptor_count_byte_depth_and_work_limits_truncate_deterministically() {
    let fields = (0..200)
        .map(|i| format!("f{i}: Text"))
        .collect::<Vec<_>>()
        .join(",");
    let m = capture(
        &format!("types: {{R: {{base: Record, fields: {{{fields}}}}}}}"),
        "R",
    );
    assert_eq!(m["fields"].as_object().unwrap().len(), 128);
    assert_eq!(m["truncated"], true);
    assert!(m["fields"].get("/f:f126").is_some());
    assert!(m["fields"].get("/f:f127").is_none());
    let fields = (0..50)
        .map(|i| format!("f{i}: S"))
        .collect::<Vec<_>>()
        .join(",");
    let members = (0..64)
        .map(|i| format!("'{}{}'", "ğ".repeat(100), i))
        .collect::<Vec<_>>()
        .join(",");
    let m = capture(
        &format!(
            "types: {{S: {{base: Text, enum: [{members}]}}, R: {{base: Record, fields: {{{fields}}}}}}}"
        ),
        "R",
    );
    assert_eq!(m["truncated"], true);
    assert!(serde_json::to_vec(&m).unwrap().len() <= 64 * 1024);
}
#[test]
fn digests_are_stable_content_bound_and_argument_checks_do_not_capture() {
    let a = capture("types: {S: {base: Text, enum: [a,b]}}", "S");
    let b = capture(
        "# comment\ntypes:\n S:\n  enum: [a, b]\n  base: Text\n",
        "S",
    );
    let c = capture("types: {S: {base: Text, enum: [b,a]}}", "S");
    assert_eq!(a["contract"]["digest"], b["contract"]["digest"]);
    assert_ne!(a["contract"]["digest"], c["contract"]["digest"]);
    let mut r = ContractRegistry::new();
    r.load("types: {S: {base: Text, enum: [a,b]}} ").unwrap();
    let contract = r.resolve("S").unwrap();
    let v = Value::new(
        Shape::Unknown,
        Data::Text("a".into()),
        Provenance::default(),
    )
    .unwrap();
    let checked =
        boundary::require("input", &[contract.clone()], &Shape::Unknown, &v, &|| false).unwrap();
    assert!(checked.metadata().is_none());
    let produced = boundary::checked_result(&contract, &v, &|| false).unwrap();
    let arg = boundary::require(
        "input",
        &[r.resolve("Text").unwrap()],
        &Shape::Unknown,
        &produced,
        &|| false,
    )
    .unwrap();
    assert_eq!(arg.metadata(), produced.metadata());
    assert!(v.with_shape(contract.shape()).unwrap().metadata().is_none());
}

#[test]
fn depth_and_enum_visit_bounds_stop_without_materializing_data() {
    let mut yaml = String::from("types:\n A0: {base: Text}\n");
    for i in 1..=70 {
        yaml.push_str(&format!(" A{i}: {{base: 'List<A{}>'}}\n", i - 1));
    }
    let m = capture(&yaml, "A70");
    assert_eq!(m["truncated"], true);
    assert_eq!(m["fields"].as_object().unwrap().len(), 65);
    let members = (0..2000)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let fields = (0..100)
        .map(|i| format!("f{i}: S"))
        .collect::<Vec<_>>()
        .join(",");
    let m = capture(
        &format!(
            "types: {{S: {{base: Text, enum: [{members}]}}, R: {{base: Record, fields: {{{fields}}}}}}}"
        ),
        "R",
    );
    assert_eq!(m["truncated"], true);
    assert_eq!(m["fields"].as_object().unwrap().len(), 50);
}

#[test]
fn public_metadata_omits_container_kinds_and_bounds_utf16_identity_names() {
    let mut registry = ContractRegistry::new();
    registry.load("types: {Row: {base: Record, fields: {text: Text, bytes: Bytes, optional: 'Option<Int>'}}}").unwrap();
    let meta = ValueMetadata::capture(&registry.resolve("List<Row>").unwrap());
    let wire = meta.wire().unwrap();
    wire.validate_wire().unwrap();
    let json = serde_json::to_value(&wire).unwrap();
    assert_eq!(json["fields"].as_object().unwrap().len(), 2);
    assert_eq!(json["fields"]["/e/f:optional/o"]["kind"], "int");
    assert!(meta.needs_projection_snapshot());
    assert!(meta.project("/e").unwrap().wire().is_some());
    for length in [1024, 1025] {
        let name = "S".repeat(length);
        let mut registry = ContractRegistry::new();
        registry.load(&format!("types: {{{name}: {{base: Text}}, Row: {{base: Record, fields: {{field: {name}}}}}}}")).unwrap();
        let scalar = ValueMetadata::capture(&registry.resolve(&name).unwrap());
        assert_eq!(scalar.wire().is_some(), length == 1024);
        let nested = ValueMetadata::capture(&registry.resolve("Row").unwrap())
            .wire()
            .unwrap();
        nested.validate_wire().unwrap();
        let json = serde_json::to_value(nested).unwrap();
        assert_eq!(json["truncated"], length > 1024);
        assert_eq!(json["fields"].get("/f:field").is_some(), length == 1024);
    }
}
