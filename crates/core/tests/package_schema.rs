//! Schema parity and architectural boundaries, with synthetic declarations only.
use wes_core::{
    IterMode,
    contracts::{Constructor, ContractRegistry, TYPE_CONSTRUCTORS, read_package},
    environments::Package,
    package_schema::{Kind, declarations},
};

#[test]
fn all_exported_references_and_semantic_descriptors_are_registered() {
    let registry = declarations();
    for schema in registry.definitions.values() {
        for constraint in &schema.constraints {
            assert!(registry.constraints.contains_key(constraint));
        }
        let references: Vec<&str> = match &schema.kind {
            Kind::Object {
                fields,
                exclusive,
                conditions,
            } => {
                for group in exclusive {
                    assert!(group.fields.iter().all(|field| fields.contains_key(field)));
                }
                for condition in conditions {
                    assert!(fields.contains_key(condition.field));
                    assert!(
                        condition
                            .allowed
                            .iter()
                            .chain(&condition.required)
                            .all(|field| fields.contains_key(field))
                    );
                }
                fields.values().map(|field| field.schema).collect()
            }
            Kind::Map { values } => vec![values],
            Kind::List { items } => vec![items],
            Kind::Union { variants } => variants.clone(),
            Kind::Discriminated { field, variants } => {
                for (choice, target) in variants {
                    let Kind::Object { fields, .. } = &registry.definitions[target].kind else {
                        panic!("variant object");
                    };
                    let Kind::Scalar { choices, .. } =
                        &registry.definitions[fields[field].schema].kind
                    else {
                        panic!("variant discriminator");
                    };
                    assert_eq!(choices, &vec![*choice]);
                }
                variants.values().copied().collect()
            }
            Kind::Any | Kind::Scalar { .. } => vec![],
        };
        for reference in references {
            assert!(
                registry.definitions.contains_key(reference),
                "missing {reference}"
            );
        }
    }
}

#[test]
fn environment_schema_enforces_structure_without_disclosing_payloads() {
    let invalid = [
        "version: 1\nenvironments: {}\nprivate-payload: secret-marker",
        "version: '1'\nenvironments: {}",
        "version: 1\nenvironments: []",
        "version: 1\nenvironments: {demo: {protected: secret-marker}}",
        "version: 1\nenvironments: {demo: {hide: {imports: {old: true}}}}",
        "version: 1\nenvironments: {demo: {extends: {env: parent, track: latest, revision: secret-marker}}}",
        "version: 1\nenvironments: {}\ntargets: {host: {kind: local, socket: secret-marker}}",
    ];
    for source in invalid {
        let error = Package::parse(source).unwrap_err();
        assert_eq!(error.code, "ENV001", "{source}");
        assert!(!error.message.contains("secret-marker"));
        assert!(!error.message.contains("private-payload"));
    }
    Package::parse(
        "version: +1\npackage: synthetic\nenvironments: {}\ntargets: {host: {kind: local}}",
    )
    .unwrap();
    let error = Package::parse(
        "version: 1\nenvironments: {demo: {parameters: {port: {type: Int, default: wrong}}}}",
    )
    .unwrap_err();
    assert_eq!(error.code, "ENV003");
}

#[test]
fn published_environment_schema_is_complete_and_tracks_validation_changes() {
    use wes_core::package_schema::Field;
    let mut registry = declarations().clone();
    let Kind::Object { fields, .. } = &mut registry
        .definitions
        .get_mut("env.target.local")
        .unwrap()
        .kind
    else {
        panic!("local target");
    };
    fields.insert(
        "futureOption",
        Field {
            schema: "bool",
            required: false,
        },
    );
    let source = read_package(
        "version: 1\ntargets: {sample: {kind: local, futureOption: true}}\nenvironments: {}\n",
    )
    .unwrap();
    registry.validate("env.package", &source).unwrap();
    let schema = registry.describe_root("env").unwrap();
    assert_eq!(schema["roots"], serde_json::json!({"env":"env.package"}));
    assert_eq!(
        schema["definitions"]["env.target.local"]["fields"]["futureOption"]["schema"],
        "bool"
    );
    assert!(schema["definitions"].get("contract.package").is_none());
    fn references(schema: &serde_json::Value) -> Vec<&str> {
        match schema["kind"].as_str().unwrap() {
            "object" => schema["fields"]
                .as_object()
                .unwrap()
                .values()
                .map(|f| f["schema"].as_str().unwrap())
                .collect(),
            "map" => vec![schema["values"].as_str().unwrap()],
            "list" => vec![schema["items"].as_str().unwrap()],
            "union" => schema["variants"]
                .as_array()
                .unwrap()
                .iter()
                .map(|v| v.as_str().unwrap())
                .collect(),
            "discriminated" => schema["variants"]
                .as_object()
                .unwrap()
                .values()
                .map(|v| v.as_str().unwrap())
                .collect(),
            "any" | "scalar" => vec![],
            _ => panic!("schema kind"),
        }
    }
    for definition in schema["definitions"].as_object().unwrap().values() {
        for id in references(definition) {
            assert!(schema["definitions"].get(id).is_some(), "missing {id}");
        }
        if let Some(rules) = definition["constraints"].as_array() {
            for rule in rules {
                assert!(schema["constraints"].get(rule.as_str().unwrap()).is_some());
            }
        }
    }
    assert!(schema.to_string().len() < 24 * 1024);
    assert!(registry.describe_root("missing").is_none());
}

#[test]
fn unknown_declaration_fields_show_only_schema_owned_choices() {
    for (source, expected) in [
        (
            "version: 1\nenvironments: {}\nprivate-field: private-value",
            "valid fields: version, package, targets, environments",
        ),
        (
            "version: 1\nenvironments: {sample: {private-field: private-value}}",
            "valid fields: targets, owner, drift",
        ),
        (
            "version: 1\ntargets: {sample: {kind: private-value}}\nenvironments: {}",
            "valid kind values: local, docker, ssh",
        ),
    ] {
        let error = Package::parse(source).unwrap_err();
        assert!(error.message.contains(expected), "{}", error.message);
        assert!(!error.message.contains("private-field"));
        assert!(!error.message.contains("private-value"));
    }
}

#[test]
fn contract_packages_reject_unsupported_fields_atomically() {
    let mut registry = ContractRegistry::new();
    let error = registry
        .load("types: {Bad: {base: Text, minItems: 1}}")
        .unwrap_err();
    assert_eq!(error.code, "TYP002");
    for field in ["views", "unsupported"] {
        let error = registry
            .load(&format!(
                "types: {{MustNotInstall: {{base: Text}}}}\n{field}: {{}}"
            ))
            .unwrap_err();
        assert_eq!(error.code, "TYP002");
        assert!(registry.resolve("MustNotInstall").is_err());
        assert!(registry.sources().is_empty());
    }
    registry.load("types: {Current: {base: Text}}").unwrap();
    assert!(registry.resolve("Current").is_ok());
}

#[test]
fn shared_catalogues_drive_iterator_choices_constructor_resolution_and_reservation() {
    let schema = &declarations().definitions["iterator.mode"];
    let Kind::Scalar { choices, .. } = &schema.kind else {
        panic!("mode choices");
    };
    assert_eq!(
        *choices,
        IterMode::ALL
            .iter()
            .map(|mode| mode.name())
            .collect::<Vec<_>>()
    );
    for mode in IterMode::ALL {
        assert_eq!(IterMode::parse(mode.name()), Some(*mode));
    }
    for mode in ["chars", "words", "json-lines"] {
        let mut registry = ContractRegistry::new();
        registry.load(&format!("types: {{}}\niterators: {{Recipe: {{input: Text, output: 'Iter<Unknown>', mode: {mode}}}}}")).unwrap();
    }
    for spec in TYPE_CONSTRUCTORS {
        assert_eq!(
            Constructor::named(spec.name).unwrap().parameters,
            spec.parameters
        );
        let mut registry = ContractRegistry::new();
        assert!(
            registry
                .load(&format!("types: {{{}: {{base: Text}}}}", spec.name))
                .is_err()
        );
        let args = if spec.parameters.len() == 2 {
            "Text, Int"
        } else {
            "Int"
        };
        registry.resolve(&format!("{}<{args}>", spec.name)).unwrap();
        assert!(
            registry
                .resolve(&format!("{}<Int, Int, Int>", spec.name))
                .is_err()
        );
    }
}

#[test]
fn union_and_recursive_yaml_data_are_accepted_without_schema_recursion() {
    let data = read_package("{nested: [1, {more: true}]}").unwrap();
    declarations().validate("datum", &data).unwrap();
    for source in ["Text", "{type: Text, optional: true}"] {
        declarations()
            .validate("contract.field", &read_package(source).unwrap())
            .unwrap();
    }
    assert!(
        declarations()
            .validate(
                "contract.field",
                &read_package("{type: Text, default: 1}").unwrap()
            )
            .is_err()
    );
}

#[test]
fn provider_publication_is_complete_and_keeps_semantics_explicit() {
    let registry = declarations();
    let published = registry.describe_root("provider").unwrap();
    assert_eq!(published["roots"]["provider"], "provider.document");
    assert!(published["roots"].get("env").is_none());
    let fields = &published["definitions"]["provider.document"]["fields"];
    assert_eq!(fields["backend"]["required"], true);
    assert_eq!(fields["types"]["required"], false);
    let backends = &published["definitions"]["provider.backend"]["variants"];
    assert_eq!(backends["process"], "provider.backend.process");
    assert_eq!(backends["session"], "provider.backend.session");
    assert_eq!(
        published["definitions"]["provider.encoding"]["choices"],
        serde_json::json!(["scalar", "spread", "flag", "json"])
    );
    assert_eq!(
        published["definitions"]["provider.control"]["fields"]["output_policy"]["schema"],
        "provider.output-policy"
    );
    assert!(
        published["definitions"]
            .get("contract.field.object")
            .is_some()
    );
    for rule in [
        "provider-session",
        "provider-read-access",
        "provider-control",
        "type-resolution",
    ] {
        assert_eq!(published["constraints"][rule]["validation"], "semantic");
    }
    assert!(published.to_string().len() < 24 * 1024);
    for source in [
        r#"{"version":1,"provider":"sample","backend":{"kind":"process","bin":"./tool"},"operations":[{"path":["read"],"argv":[],"result":{"format":"text"}}]}"#,
        r#"{"version":1,"provider":"sample","backend":{"kind":"session","bin":"./tool","credentials":["Key"]},"read_access":{"scope":["tenant"],"revalidate":["probe"]},"operations":[{"path":["probe"],"parameters":[{"name":"tenant","type":"Text"}],"argv":[],"result":{"format":"json","type":"Bool"}}]}"#,
    ] {
        registry
            .validate("provider.document", &read_package(source).unwrap())
            .unwrap();
    }
}
