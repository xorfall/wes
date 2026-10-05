use wes_core::{
    Shape,
    capability::{
        Capability, Catalogue, DeclaredRule, Parameter, ProviderDescription, Rule, RuleBasis,
        Safety,
    },
};

fn capability() -> Capability {
    Capability::new(["items", "get"], Shape::Unknown, Safety::Safe)
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
