use wes_core::contracts::ContractRegistry;

#[test]
fn package_acceptance_errors_and_atomic_registry_match_acceptance_fixtures() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/contract-acceptance.json"
    ))
    .unwrap();
    let mut differences = Vec::new();
    for case in cases.as_array().unwrap() {
        let input = case["input"].as_str().unwrap();
        let mut registry = ContractRegistry::new();
        let result = registry.load(input);
        let code = result.as_ref().err().map_or("", |error| error.code);
        // Built-in declaration order is presentation metadata, not type semantics.
        let names = registry
            .snapshot()
            .keys()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        let expected = case["names"]
            .as_array()
            .unwrap()
            .iter()
            .map(|name| name.as_str().unwrap().to_owned())
            .collect();
        if code != case["code"].as_str().unwrap() || names != expected {
            differences.push(format!(
                "{input}\nactual: {result:?}\nexpected code: {}",
                case["code"]
            ));
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n\n"));
}
