//! Shipped and user-built Views share checked artifacts; discovery never runs renderer code.
mod artifact;
mod catalogue;
pub use catalogue::Catalogue;
mod package;
pub use artifact::{Artifact, ArtifactSource, MAX_ARTIFACT_BYTES, sdk_version};
pub use package::{Interaction, Manifest, Mode, Package, Port, Slot, validate_catalogue};
use std::sync::OnceLock;
include!(concat!(env!("OUT_DIR"), "/packages.rs"));

pub fn catalogue() -> &'static [Package] {
    static PACKAGES: OnceLock<Vec<Package>> = OnceLock::new();
    PACKAGES.get_or_init(|| {
        artifacts()
            .iter()
            .map(|artifact| artifact.package.clone())
            .collect()
    })
}
pub fn artifacts() -> &'static [Artifact] {
    static ARTIFACTS: OnceLock<Vec<Artifact>> = OnceLock::new();
    ARTIFACTS.get_or_init(|| {
        SOURCES
            .iter()
            .map(|source| {
                Artifact::parse(source.as_bytes()).expect("build validated View artifact")
            })
            .collect()
    })
}
pub fn named(name: &str) -> Option<&'static Package> {
    catalogue().iter().find(|p| p.manifest.name == name)
}
pub fn names() -> impl Iterator<Item = &'static str> {
    catalogue().iter().map(|p| p.manifest.name.as_str())
}

/// Metadata contains no provider handles, credentials or execution capability.
pub fn metadata(value: serde_json::Value) -> wes_core::Data {
    use wes_core::Data;
    match value {
        serde_json::Value::Null => Data::Option(None),
        serde_json::Value::String(s) => Data::Text(s.into()),
        serde_json::Value::Bool(b) => Data::Bool(b),
        serde_json::Value::Number(n) => Data::Int(n.as_i64().expect("bounded metadata integer")),
        serde_json::Value::Array(a) => Data::List(a.into_iter().map(metadata).collect()),
        serde_json::Value::Object(o) => {
            Data::Record(o.into_iter().map(|(k, v)| (k, metadata(v))).collect())
        }
    }
}
