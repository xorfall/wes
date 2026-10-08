use wes_core::{
    Data, Shape,
    capability::{
        Capability, Catalogue, DeclaredRule, EnumDomain, EnumKind, Parameter, ProviderDescription,
        Rule, RuleBasis, Safety,
    },
};

fn capability() -> Capability {
    Capability::new(["items", "get"], Shape::Unknown, Safety::Safe)
}

fn enum_contract(base: &str, members: &str) -> std::sync::Arc<wes_core::contracts::Contract> {
    let mut registry = wes_core::contracts::ContractRegistry::new();
    registry
        .load(&format!(
            "types: {{Choice: {{base: {base}, enum: [{members}]}}}}"
        ))
        .unwrap();
    registry.resolve("Choice").unwrap()
}

fn one_of(key: &str, values: &[&str], binding: bool) -> DeclaredRule {
    DeclaredRule {
        rule: Rule::OneOf {
            key: key.into(),
            values: values.iter().map(|v| (*v).into()).collect(),
        },
        basis: if binding {
            RuleBasis::Documented { note: None }
        } else {
            RuleBasis::Inferred {
                reason: "synthetic observation".into(),
            }
        },
    }
}

#[test]
fn resolved_scalar_enums_preserve_kind_spelling_order_and_shared_full_domain() {
    for (base, members, kind, expected) in [
        (
            "Text",
            "'true', '123', 'Türkçe', 'true'",
            EnumKind::Text,
            vec!["true", "123", "Türkçe"],
        ),
        (
            "Int",
            "9007199254740993, -9223372036854775808, 9007199254740993",
            EnumKind::Int,
            vec!["9007199254740993", "-9223372036854775808"],
        ),
        (
            "Decimal",
            "12.50, 0.123456789012345678901234567890, 12.50, 12.5",
            EnumKind::Decimal,
            vec!["12.50", "0.123456789012345678901234567890", "12.5"],
        ),
        (
            "Bool",
            "true, false, true",
            EnumKind::Bool,
            vec!["true", "false"],
        ),
    ] {
        let contract = enum_contract(base, members);
        let parameter = Parameter::new("choice", contract.shape(), true).constrained_by(&contract);
        let domain = parameter.enum_domain.as_ref().unwrap();
        assert_eq!(domain.kind, kind);
        let preview = domain.choices("choice", &[]);
        assert_eq!(preview.members, expected);
        assert_eq!(preview.total, expected.len());
        assert!(preview.complete);
        assert!(std::sync::Arc::ptr_eq(
            &domain.members,
            &parameter.clone().enum_domain.unwrap().members
        ));
    }
    let mut registry = wes_core::contracts::ContractRegistry::new();
    registry.load("types: {Parent: {base: Text, enum: [a, b]}, Child: {base: Parent, enum: [b]}, Alias: {base: Child}, Flag: {base: Bool, enum: [true]}}").unwrap();
    assert_eq!(
        EnumDomain::from_contract(&registry.resolve("Alias").unwrap())
            .unwrap()
            .choices("x", &[])
            .members,
        ["b"]
    );
    for kind in [
        "Text",
        "Int",
        "Option<Flag>",
        "List<Flag>",
        "Union<Flag,Text>",
        "Unknown",
        "Instant",
    ] {
        assert!(
            EnumDomain::from_contract(&registry.resolve(kind).unwrap()).is_none(),
            "{kind}"
        );
    }
    let parameter = Parameter::new("x", Shape::Unknown, true)
        .constrained_by(&registry.resolve("Flag").unwrap())
        .constrained_by(&registry.resolve("Text").unwrap());
    assert!(parameter.enum_domain.is_none());
}

#[test]
fn previews_count_full_domains_at_64_65_and_200_members() {
    for count in [64, 65, 200] {
        let members = (0..count)
            .map(|i| i.to_string())
            .collect::<Vec<_>>()
            .join(",");
        let domain = EnumDomain::from_contract(&enum_contract("Int", &members)).unwrap();
        assert_eq!(domain.members.len(), count);
        let preview = domain.choices("x", &[]);
        assert_eq!(preview.total, count);
        assert_eq!(
            preview.members,
            (0..64).map(|i| i.to_string()).collect::<Vec<_>>()
        );
        assert_eq!(preview.complete, count == 64);
        if count == 200 {
            assert_eq!(domain.members[199], Data::Int(199));
            let narrowed = domain.choices("x", &[one_of("x", &["199"], true)]);
            assert_eq!(narrowed.members, ["199"]);
            assert_eq!(narrowed.total, 1);
            assert!(narrowed.complete);
        }
    }
}

#[test]
fn every_documented_one_of_intersects_and_inferred_or_unrelated_rules_do_not() {
    let domain = EnumDomain::from_contract(&enum_contract("Text", "a, b, c, a")).unwrap();
    let rules = [
        one_of("x", &["a", "b", "outside"], true),
        one_of("x", &["b", "c"], true),
        one_of("x", &["a"], false),
        one_of("other", &[], true),
    ];
    let preview = domain.choices("x", &rules);
    assert_eq!(preview.members, ["b"]);
    assert_eq!(preview.total, 1);
    assert!(preview.complete);
    assert_eq!(domain.choices("x", &[one_of("x", &[], false)]).total, 3);
    for rules in [
        vec![one_of("x", &["outside"], true)],
        vec![one_of("x", &["a"], true), one_of("x", &["b"], true)],
        vec![one_of("x", &[], true)],
    ] {
        let preview = domain.choices("x", &rules);
        assert!(preview.members.is_empty());
        assert_eq!(preview.total, 0);
        assert!(preview.complete);
    }
    let decimal = EnumDomain::from_contract(&enum_contract("Decimal", "12.50, 12.5")).unwrap();
    assert_eq!(
        decimal
            .choices("x", &[one_of("x", &["12.50"], true)])
            .members,
        ["12.50"]
    );
}

#[test]
fn encoded_byte_limit_includes_json_escaping_utf8_and_metadata() {
    let members = [
        "\u{0}".repeat(2000),
        "Türkçe\n\"\\".repeat(1000),
        "last".into(),
    ];
    let contract = enum_contract(
        "Text",
        &members
            .iter()
            .map(|s| serde_json::to_string(s).unwrap())
            .collect::<Vec<_>>()
            .join(","),
    );
    let domain = EnumDomain::from_contract(&contract).unwrap();
    let preview = domain.choices("x", &[]);
    assert_eq!(preview.total, 3);
    assert_eq!(preview.members, members[..1]);
    assert!(!preview.complete);
    let encoded = serde_json::to_vec(&serde_json::json!({"kind":preview.kind.name(),"members":preview.members,"total":preview.total,"complete":preview.complete})).unwrap();
    assert!(encoded.len() <= EnumDomain::MAX_ENCODED_BYTES);
    let narrowed = domain.choices("x", &[one_of("x", &["last"], true)]);
    assert_eq!(narrowed.members, ["last"]);
    assert!(narrowed.complete);
    let huge = enum_contract(
        "Text",
        &serde_json::to_string(&"x".repeat(16 * 1024)).unwrap(),
    );
    let preview = EnumDomain::from_contract(&huge).unwrap().choices("x", &[]);
    assert_eq!(preview.total, 1);
    assert!(preview.members.is_empty());
    assert!(!preview.complete);
}

#[test]
fn catalogue_replaces_atomically_and_retains_prior_descriptors() {
    let mut catalogue = Catalogue::new();
    let provider = ProviderDescription::new(
        "catalog",
        [capability()],
        vec!["token".into(), "account".into()],
    )
    .unwrap();
    assert_eq!(provider.secrets(), ["token", "account"]);
    catalogue.register(provider);
    let snapshot = catalogue.clone();
    let previous = catalogue
        .register(ProviderDescription::new("catalog", [], vec![]).unwrap())
        .unwrap();
    assert_eq!(previous.capabilities().len(), 1);
    assert_eq!(
        snapshot.provider("catalog").unwrap().capabilities().len(),
        1
    );
    assert_eq!(
        catalogue.provider("catalog").unwrap().capabilities().len(),
        0
    );
    assert!(catalogue.unregister("catalog").is_some());
    assert!(catalogue.is_empty());
}

#[test]
fn malformed_metadata_is_rejected_at_import_boundary() {
    assert!(ProviderDescription::new(" ", [capability()], vec![]).is_err());
    assert!(ProviderDescription::new("catalog", [capability(), capability()], vec![]).is_err());
    let mut broken = capability();
    broken.path.clear();
    assert!(ProviderDescription::new("catalog", [broken], vec![]).is_err());
    let mut broken = capability();
    broken.parameters = vec![Parameter::new("x", Shape::Unknown, true); 2];
    assert!(ProviderDescription::new("catalog", [broken], vec![]).is_err());
    let mut broken = capability();
    broken.rules.push(DeclaredRule {
        rule: Rule::Requires {
            key: "a".into(),
            needs: "b".into(),
        },
        basis: RuleBasis::Inferred { reason: "".into() },
    });
    assert!(ProviderDescription::new("catalog", [broken], vec![]).is_err());
}

#[test]
fn renaming_does_not_rewrite_capabilities_or_secret_requirements() {
    let original =
        ProviderDescription::new("first", [capability()], vec!["api-token".into()]).unwrap();
    let renamed = original.renamed("second").unwrap();
    assert_eq!(renamed.name(), "second");
    assert_eq!(original.name(), "first");
    assert!(std::sync::Arc::ptr_eq(
        original.capabilities().next().unwrap(),
        renamed.capabilities().next().unwrap()
    ));
    assert_eq!(original.secrets(), renamed.secrets());
}
