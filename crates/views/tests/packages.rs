use wes_core::{Data, contracts::ContractRegistry};
use wes_views::{Package, catalogue};
const TYPES: &str = "types:\n  Badge: {base: Record, fields: {label: Text, count: Int}}\n";
fn manifest() -> serde_json::Value {
    serde_json::json!({"name":"Badge","id":"badge","summary":"Show a label and count.","renderer":"View.tsx","input":"Badge","outputs":{},"interaction":null})
}
#[test]
fn dataset_is_read_only_even_when_an_output_reuses_the_input_contract_name() {
    let types = "types: {Badge: {base: Record, fields: {rows: 'Dataset<Int>'}}, State: {base: Record, fields: {count: Int}}}";
    let mut m = manifest();
    let package = Package::parse(&m.to_string(), types).unwrap();
    let contracts = &package.description()["contracts"];
    assert_eq!(contracts["Dataset<Int>"]["kind"], "dataset");
    m["interaction"] = serde_json::json!({"protocol":"State","state":"State","event":"State"});
    m["outputs"] = serde_json::json!({"rows":{"type":"Badge","mode":"state"}});
    assert!(
        Package::parse(&m.to_string(), types)
            .unwrap_err()
            .contains("read-only input")
    );
    m["outputs"] = serde_json::json!({});
    for port in ["state", "event"] {
        let mut candidate = m.clone();
        candidate["interaction"][port] = serde_json::json!("Badge");
        assert!(
            Package::parse(&candidate.to_string(), types)
                .unwrap_err()
                .contains("read-only input")
        );
    }
}
#[test]
fn dataset_rows_preserve_native_patterns_without_admitting_browser_authored_values() {
    let types = "types: {Token: {base: Text, pattern: '^[a-z]{3}$'}, Badge: {base: Record, fields: {rows: 'Dataset<Token>'}}, State: {base: Record, fields: {count: Int}}}";
    let mut m = manifest();
    let p = Package::parse(&m.to_string(), types).unwrap();
    assert_eq!(
        p.description()["contracts"]["Token"]["constraints"]["patterns"],
        serde_json::json!(["^[a-z]{3}$"])
    );
    // The same named declaration is read-only only beneath Dataset. Reuse in
    // a writable port or an ordinary input must still fail before execution.
    m["outputs"] = serde_json::json!({"chosen":{"type":"Token","mode":"state"}});
    m["interaction"] = serde_json::json!({"protocol":"State","state":"State","event":"State"});
    assert!(
        Package::parse(&m.to_string(), types)
            .unwrap_err()
            .contains("Pattern-constrained")
    );
    m["outputs"] = serde_json::json!({});
    let mixed = types.replace(
        "rows: 'Dataset<Token>'",
        "rows: 'Dataset<Token>', direct: Token",
    );
    assert!(
        Package::parse(&m.to_string(), &mixed)
            .unwrap_err()
            .contains("Pattern-constrained")
    );
}
#[test]
fn independent_package_uses_wes_contracts_without_a_named_parser_branch() {
    let p = Package::parse(&manifest().to_string(), TYPES).unwrap();
    let mut registry = ContractRegistry::new();
    registry.load(TYPES).unwrap();
    assert_eq!(
        p.input().shape(),
        registry.resolve("Badge").unwrap().shape()
    );
    let data = Data::Record(
        [
            ("label".into(), Data::Text("Tasks".into())),
            ("count".into(), Data::Int(3)),
        ]
        .into_iter()
        .collect(),
    );
    assert!(p.validate_input(&data).is_ok());
    assert!(p.validate_input(&Data::Text("wrong".into())).is_err());
    assert_eq!(p.description()["inputModes"], serde_json::json!(["value"]));
    assert_eq!(p.description()["execution"], "none");
    assert_eq!(
        p.digest,
        Package::parse(&serde_json::to_string_pretty(&manifest()).unwrap(), TYPES)
            .unwrap()
            .digest
    );
    let changed = TYPES.replace("count: Int", "count: Text");
    assert_ne!(
        p.digest,
        Package::parse(&manifest().to_string(), &changed)
            .unwrap()
            .digest
    );
}
#[test]
fn malformed_contracts_and_unsupported_delivery_fail_closed() {
    for (key, value) in [
        ("input", serde_json::json!("Missing")),
        ("renderer", serde_json::json!("../View.tsx")),
        ("name", serde_json::json!("bad-name")),
        ("id", serde_json::json!("json")),
        ("unknown", serde_json::json!(true)),
    ] {
        let mut m = manifest();
        m[key] = value;
        assert!(Package::parse(&m.to_string(), TYPES).is_err());
    }
    assert!(Package::parse(&manifest().to_string(), "types: {Badge: {base: Unknown}}").is_err());
    assert!(
        Package::parse(
            &manifest().to_string(),
            "types: {Badge: {base: Record, fields: {x: 'Iter<Int>'}}}"
        )
        .is_err()
    );
    let repeated=manifest().to_string().replace("\"outputs\":{}","\"outputs\":{\"x\":{\"type\":\"Text\",\"mode\":\"state\"},\"x\":{\"type\":\"Int\",\"mode\":\"state\"}}");
    assert!(
        Package::parse(&repeated, TYPES)
            .unwrap_err()
            .contains("duplicate")
    );
    let mut m = manifest();
    m["outputs"] = serde_json::json!({"x":{"type":"Text","mode":"event"}});
    m["interaction"] = serde_json::json!({"protocol":"BadgeState","state":"Badge","event":"Badge"});
    assert!(
        Package::parse(&m.to_string(), TYPES)
            .unwrap_err()
            .contains("Event output")
    );
    assert!(Package::parse(&" ".repeat(65537), TYPES).is_err());
}
#[test]
fn shipped_descriptors_are_complete_and_have_unique_identity() {
    let installed = wes_views::Catalogue::default();
    let mut names = std::collections::BTreeSet::new();
    let mut ids = std::collections::BTreeSet::new();
    for p in catalogue() {
        assert!(names.insert(&p.manifest.name));
        assert!(ids.insert(&p.manifest.id));
        let d = p.description();
        assert!(d["contracts"].get(&p.manifest.input).is_some());
        assert_eq!(d["digest"].as_str().unwrap().len(), 64);
        let artifact = installed.artifact(p.artifact.as_deref().unwrap()).unwrap();
        assert_eq!(artifact.package.digest, p.digest);
        assert!(!artifact.source.javascript.is_empty());
    }
    assert_eq!(
        names.into_iter().map(String::as_str).collect::<Vec<_>>(),
        [
            "Choice",
            "Dashboard",
            "Histogram",
            "Metric",
            "Timeline",
            "TimelineGroup"
        ]
    );
}

#[test]
fn shared_state_fields_and_protocol_identity_are_declarative() {
    let mut m = manifest();
    m["interaction"] = serde_json::json!({"protocol":"BadgeState","state":"Badge","event":"Badge","sharedFields":["count"]});
    m["outputs"] = serde_json::json!({"count":{"type":"Int","mode":"state","shared":true}});
    let package = Package::parse(&m.to_string(), TYPES).unwrap();
    assert_eq!(package.description()["outputScope"], "instance");
    m["interaction"]["sharedFields"] = serde_json::json!(["label"]);
    let changed = Package::parse(&m.to_string(), TYPES).unwrap();
    assert_ne!(package.protocol_identity(), changed.protocol_identity());
    for names in [
        serde_json::json!(["missing"]),
        serde_json::json!(["count", "count"]),
        serde_json::json!([]),
    ] {
        m["interaction"]["sharedFields"] = names;
        assert!(Package::parse(&m.to_string(), TYPES).is_err());
    }
}

#[test]
fn numeric_enum_metadata_preserves_full_int_precision_for_renderer_generation() {
    let types = "types: {Allowed: {base: Int, enum: [9223372036854775807]}, Badge: {base: Record, fields: {label: Text, count: Allowed}}}";
    let package = Package::parse(&manifest().to_string(), types).unwrap();
    assert_eq!(
        package.description()["contracts"]["Allowed"]["constraints"]["enum"],
        serde_json::json!(["9223372036854775807"])
    );
    assert!(
        package
            .validate_input(&Data::Record(
                [
                    ("label".into(), Data::Text("Count".into())),
                    ("count".into(), Data::Int(i64::MAX))
                ]
                .into()
            ))
            .is_ok()
    );
}

#[test]
fn layout_sizes_are_validated_projected_and_part_of_renderer_identity() {
    let tier = serde_json::json!({"min":{"columns":24,"rows":4},"preferred":{"columns":80,"rows":14},"max":{"columns":160,"rows":40}});
    let mut m = manifest();
    m["layout"] = serde_json::json!({"preview":tier,"expanded":tier,"window":tier});
    let valid = Package::parse(&m.to_string(), TYPES).unwrap();
    assert_eq!(valid.description()["layout"], m["layout"]);
    assert_ne!(
        valid.digest,
        Package::parse(&manifest().to_string(), TYPES)
            .unwrap()
            .digest
    );
    for (field, value) in [("columns", 0), ("columns", 513), ("rows", 201)] {
        let mut invalid = m.clone();
        invalid["layout"]["expanded"]["max"][field] = serde_json::json!(value);
        assert!(Package::parse(&invalid.to_string(), TYPES).is_err());
    }
    let mut invalid = m.clone();
    invalid["layout"]["window"]["min"]["rows"] = serde_json::json!(20);
    assert!(Package::parse(&invalid.to_string(), TYPES).is_err());
    invalid["layout"]["window"]["min"]["rows"] = serde_json::json!(4);
    invalid["layout"]["window"]["min"]["pixels"] = serde_json::json!(100);
    assert!(Package::parse(&invalid.to_string(), TYPES).is_err());
}

#[test]
fn view_placement_is_tier_specific_validated_and_part_of_identity() {
    let tier = serde_json::json!({"min":{"columns":24,"rows":4},"preferred":{"columns":80,"rows":14},"max":{"columns":160,"rows":40}});
    let mut m = manifest();
    m["layout"] = serde_json::json!({"preview":tier,"expanded":tier,"window":tier});
    let old = Package::parse(&m.to_string(), TYPES).unwrap();
    m["layout"]["expanded"]["placement"] =
        serde_json::json!({"width":"preferred","align":"center"});
    let valid = Package::parse(&m.to_string(), TYPES).unwrap();
    assert_eq!(valid.description()["layout"], m["layout"]);
    assert_ne!(old.digest, valid.digest);
    for placement in [
        serde_json::json!({"width":"fixed","align":"center"}),
        serde_json::json!({"width":"fill","align":"left"}),
        serde_json::json!({"width":"fill"}),
        serde_json::json!({"width":"fill","align":"start","pixels":300}),
    ] {
        m["layout"]["expanded"]["placement"] = placement;
        assert!(Package::parse(&m.to_string(), TYPES).is_err());
    }
}

#[test]
fn descriptions_advertise_reference_kinds_and_delivery_without_removed_modes() {
    let definition = wes_views::named("Timeline").unwrap().description();
    assert_eq!(
        definition["inputReferences"],
        serde_json::json!(["unlinked", "current", "retained"])
    );
    assert_eq!(
        definition["inputDelivery"],
        serde_json::json!(["finite", "window"])
    );
    assert!(definition.get("bindingModes").is_none());
}
