use wes_core::{Data, Shape, contracts::ContractRegistry, literals};

#[test]
fn literal_acceptance_exact_values_and_canonical_forms_match_acceptance_fixtures() {
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/literal-acceptance.json"
    ))
    .unwrap();
    let types = ContractRegistry::new();
    let mut differences = Vec::new();
    for case in cases.as_array().unwrap() {
        let shape: Shape = types
            .resolve(case["type"].as_str().unwrap())
            .unwrap()
            .shape();
        let input = case["input"].as_str().unwrap();
        let value = literals::read(input, &shape);
        let canonical = value
            .as_ref()
            .map(|value| match value {
                Data::Text(text) => serde_json::to_string(text.as_ref()).unwrap(),
                Data::Bool(value) => value.to_string(),
                Data::Int(value) => value.to_string(),
                Data::Decimal(value) => value.to_string(),
                Data::Instant(value) => serde_json::to_string(&value.to_string()).unwrap(),
                Data::Duration(value) => serde_json::to_string(&value.to_string()).unwrap(),
                _ => panic!("unexpected fixture data"),
            })
            .unwrap_or_default();
        if value.is_some() != case["accepted"].as_bool().unwrap()
            || canonical != case["canonical"].as_str().unwrap()
        {
            differences.push(format!(
                "{} {input:?}: actual {canonical:?}, expected {}",
                case["type"], case["canonical"]
            ));
        }
        if let Some(parts) = value.and_then(|value| match value {
            Data::Instant(value) => Some(value.parts()),
            Data::Duration(value) => Some(value.parts()),
            _ => None,
        }) {
            assert_eq!(
                parts.seconds(),
                case["parts"][0].as_str().unwrap().parse::<i64>().unwrap(),
                "{input}"
            );
            assert_eq!(
                parts.nanos(),
                case["parts"][1].as_str().unwrap().parse::<u32>().unwrap(),
                "{input}"
            );
        }
    }
    assert!(differences.is_empty(), "{}", differences.join("\n"));
}
