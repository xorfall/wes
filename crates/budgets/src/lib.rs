//! Immutable process-start operating policy. Explicit per-operation configurations still win.
//! Embedders/tests use catalogue defaults unless they explicitly activate a profile before use.
use serde::{Deserialize, Serialize, de};
use std::{
    collections::BTreeMap,
    sync::{LazyLock, OnceLock},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Definition {
    pub id: String,
    pub label: String,
    pub description: String,
    pub group: String,
    pub unit: String,
    pub default: u64,
    pub min: u64,
    pub max: u64,
    pub source: String,
}
static CATALOGUE: LazyLock<BTreeMap<String, Definition>> = LazyLock::new(|| {
    let definitions: Vec<Definition> =
        serde_json::from_str(include_str!("../../../packages/budgets/catalog.json"))
            .expect("valid built-in operating budget catalogue");
    let mut result = BTreeMap::new();
    for definition in definitions {
        assert!(
            definition.min > 0
                && definition.min <= definition.default
                && definition.default <= definition.max
                && definition.max <= 9_007_199_254_740_991
        );
        assert!(matches!(
            definition.unit.as_str(),
            "count" | "bytes" | "milliseconds"
        ));
        assert!(
            result.insert(definition.id.clone(), definition).is_none(),
            "duplicate budget id"
        );
    }
    result
});
static ACTIVE: OnceLock<BTreeMap<String, u64>> = OnceLock::new();
pub fn catalogue() -> &'static BTreeMap<String, Definition> {
    &CATALOGUE
}
pub fn get(id: &str) -> u64 {
    let definition = CATALOGUE.get(id).expect("registered operating budget");
    ACTIVE
        .get()
        .and_then(|values| values.get(id))
        .copied()
        .unwrap_or(definition.default)
}
/// Called once by an executable entry point, never by a workspace/data-home opening operation.
pub fn activate(values: BTreeMap<String, u64>) -> Result<(), String> {
    validate(&values)?;
    ACTIVE
        .set(values)
        .map_err(|_| "Operating policy has already been activated for this process.".into())
}
pub fn validate(values: &BTreeMap<String, u64>) -> Result<(), String> {
    for (id, value) in values {
        let definition = CATALOGUE
            .get(id)
            .ok_or_else(|| format!("Unknown operating budget: {id}"))?;
        if !(definition.min..=definition.max).contains(value) {
            return Err(format!(
                "{} must be between {} and {} {}.",
                definition.label, definition.min, definition.max, definition.unit
            ));
        }
    }
    Ok(())
}
/// Reject duplicate keys instead of silently accepting the last value in a saved policy.
pub fn unique_values<'de, D: de::Deserializer<'de>>(
    deserializer: D,
) -> Result<BTreeMap<String, u64>, D::Error> {
    struct Unique;
    impl<'de> de::Visitor<'de> for Unique {
        type Value = BTreeMap<String, u64>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("unique operating budget values")
        }
        fn visit_map<M: de::MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
            let mut result = BTreeMap::new();
            while let Some((key, value)) = map.next_entry::<String, u64>()? {
                if result.insert(key, value).is_some() {
                    return Err(de::Error::custom("Duplicate operating budget id"));
                }
                if result.len() > CATALOGUE.len() {
                    return Err(de::Error::custom("Too many operating budgets"));
                }
            }
            Ok(result)
        }
    }
    deserializer.deserialize_map(Unique)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compiled_catalogue_matches_the_shared_authoring_source() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../packages/budgets/catalog.json");
        let source: Vec<Definition> =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let source: BTreeMap<_, _> = source
            .into_iter()
            .map(|definition| (definition.id.clone(), definition))
            .collect();
        assert_eq!(
            serde_json::to_value(catalogue()).unwrap(),
            serde_json::to_value(source).unwrap(),
            "The compiled engine and GUI must use the same budget definitions"
        );
    }
    #[test]
    fn defaults_and_every_registered_range_are_valid() {
        assert!(catalogue().len() > 100);
        for entry in catalogue().values() {
            assert_eq!(get(&entry.id), entry.default);
            for value in [entry.min, entry.default, entry.max] {
                validate(&[(entry.id.clone(), value)].into()).unwrap();
            }
            assert!(validate(&[(entry.id.clone(), 0)].into()).is_err());
            assert!(validate(&[(entry.id.clone(), entry.max + 1)].into()).is_err());
        }
        assert!(validate(&[("unknown".into(), 1)].into()).is_err());
    }
}
