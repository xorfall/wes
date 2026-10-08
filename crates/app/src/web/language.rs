//! The calculation language package, as the engine is using it.
//!
//! A client that highlights or completes `:calc` must not carry its own copy of the grammar: the
//! package is the engine's, and the UI is generated from it. So the engine publishes what it parsed
//! rather than the client guessing, and publishes the package source with it — `calc::Package`
//! documents the immutable full source as a package's capture identity, and that is what a run
//! records, so a client can tell exactly which package a result was produced under.
use axum::{
    http::header,
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};
use wes_language::calc::Package;

/// Build-time component contracts. Read-only metadata; renderer code and execution handles are absent.
pub(super) async fn views(
    super::workspaces::Scoped(shared): super::workspaces::Scoped,
) -> Response {
    let Ok(current) = shared.application.current() else {
        return axum::http::StatusCode::GONE.into_response();
    };
    let catalogue = match current.session.view_catalogue().await {
        Ok(c) => c,
        Err(e) => return (axum::http::StatusCode::FORBIDDEN, e.to_string()).into_response(),
    };
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        json!(
            catalogue
                .values()
                .map(|p| p.description())
                .collect::<Vec<_>>()
        )
        .to_string(),
    )
        .into_response()
}
pub fn published_views() -> Value {
    json!(
        wes_views::catalogue()
            .iter()
            .map(|p| p.description())
            .collect::<Vec<_>>()
    )
}

/// `GET /language/calc`. The package is the same one every command in this session is prepared with.
pub(super) async fn calc() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        published(&Package::standard()).to_string(),
    )
        .into_response()
}

/// The wire form of a package. Shared with the bundled client copy, which is checked against it.
pub fn published(package: &Package) -> Value {
    json!({
        "language": "calc",
        "version": package.version(),
        "lexical": package.lexical()
            .map(|(key, production)| (key.to_owned(), Value::from(production)))
            .collect::<serde_json::Map<_, _>>(),
        "statements": package.statements()
            .map(|(word, keyword)| (word.to_owned(), Value::from(keyword.id())))
            .collect::<serde_json::Map<_, _>>(),
        "operators": package.binaries()
            .map(|(symbol, spec)| (symbol.to_owned(), json!({"operation": spec.operation.id(), "precedence": spec.precedence})))
            .collect::<serde_json::Map<_, _>>(),
        "operations": package.operations()
            .map(|(name, spec)| (name.to_owned(), json!({"operation": spec.operation.id(), "min": spec.min, "max": spec.max, "method": spec.operation.supports_method()})))
            .collect::<serde_json::Map<_, _>>(),
        "source": package.source(),
    })
}

/// `GET /language/yaml`: the same structural schema used by the core package readers.
pub(super) async fn yaml() -> Response {
    (
        [
            (header::CONTENT_TYPE, "application/json"),
            (header::CACHE_CONTROL, "no-store"),
        ],
        published_yaml().to_string(),
    )
        .into_response()
}

/// Transport projection only: no field names, accepted values or validation rules live here.
pub fn published_yaml() -> Value {
    publish_yaml_registry(wes_core::package_schema::declarations())
}

fn publish_yaml_registry(registry: &wes_core::package_schema::Registry) -> Value {
    use wes_core::package_schema::{Kind, ScalarType};
    let definitions = registry.definitions.iter().map(|(name, schema)| {
        let mut value = match &schema.kind {
            Kind::Any => json!({"kind":"any"}),
            Kind::Scalar { scalar, choices } => {
                let name = match scalar { ScalarType::Text => "Text", ScalarType::Int => "Int", ScalarType::Decimal => "Decimal", ScalarType::Bool => "Bool", ScalarType::TypeExpression => "TypeExpression" };
                let mut value = json!({"kind":"scalar", "type":name});
                if !choices.is_empty() { value["choices"] = json!(choices); }
                value
            }
            Kind::Object { fields, exclusive, conditions } => {
                let fields = fields.iter().map(|(name, field)| ((*name).to_owned(), json!({"schema":field.schema,"required":field.required}))).collect::<serde_json::Map<_, _>>();
                let mut value = json!({"kind":"object", "fields":fields});
                if !exclusive.is_empty() { value["exclusive"] = json!(exclusive.iter().map(|group| json!({"fields":group.fields,"min":group.min,"max":group.max})).collect::<Vec<_>>()); }
                if !conditions.is_empty() { value["conditions"] = json!(conditions.iter().map(|condition| {
                    let mut value = json!({"field":condition.field,"values":condition.values,"allowed":condition.allowed});
                    if let Some(prefix) = condition.prefix { value["prefix"] = json!(prefix); }
                    if !condition.required.is_empty() { value["required"] = json!(condition.required); }
                    value
                }).collect::<Vec<_>>()); }
                value
            }
            Kind::Map { values } => json!({"kind":"map", "values":values}),
            Kind::List { items } => json!({"kind":"list", "items":items}),
            Kind::Union { variants } => json!({"kind":"union", "variants":variants}),
            Kind::Discriminated { field, variants } => json!({"kind":"discriminated", "field":field,"variants":variants.iter().map(|(key,value)| ((*key).to_owned(),json!(value))).collect::<serde_json::Map<_,_>>()}),
        };
        if let Some(hint) = schema.hint { value["hint"] = json!(hint); }
        if !schema.constraints.is_empty() { value["constraints"] = json!(schema.constraints); }
        ((*name).to_owned(), value)
    }).collect::<serde_json::Map<_,_>>();
    json!({
        "language":"yaml", "version":1,
        "roots":registry.roots.iter().map(|(name,schema)| ((*name).to_owned(),json!(schema))).collect::<serde_json::Map<_,_>>(),
        "definitions":definitions,
        "typeConstructors":wes_core::contracts::TYPE_CONSTRUCTORS.iter().map(|spec| json!({"name":spec.name,"parameters":spec.parameters})).collect::<Vec<_>>(),
        "constraints":registry.constraints.iter().map(|(name,description)| ((*name).to_owned(),json!({"description":description,"validation":"semantic"}))).collect::<serde_json::Map<_,_>>(),
    })
}

#[cfg(test)]
mod yaml_tests {
    use super::*;
    #[test]
    fn one_schema_change_updates_validation_and_publication() {
        use wes_core::package_schema::{Field, Kind};
        let mut registry = wes_core::package_schema::declarations().clone();
        let source =
            wes_core::contracts::read_package("version: 1\nenvironments: {}\nfuture: true")
                .unwrap();
        assert!(registry.validate("env.package", &source).is_err());
        let Kind::Object { fields, .. } =
            &mut registry.definitions.get_mut("env.package").unwrap().kind
        else {
            panic!("object");
        };
        fields.insert(
            "future",
            Field {
                schema: "bool",
                required: false,
            },
        );
        registry.validate("env.package", &source).unwrap();
        let published = publish_yaml_registry(&registry);
        assert_eq!(
            published["definitions"]["env.package"]["fields"]["future"]["schema"],
            "bool"
        );
        let invalid =
            wes_core::contracts::read_package("version: 1\nenvironments: {}\nfuture: 2").unwrap();
        assert!(registry.validate("env.package", &invalid).is_err());
    }
}
