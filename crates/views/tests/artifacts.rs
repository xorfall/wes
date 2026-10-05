use serde_json::json;
use wes_views::{Artifact, Package};

fn source() -> serde_json::Value {
    let manifest = json!({"name":"TaskBadge","id":"task-badge","summary":"Shows task count.","renderer":"View.tsx","input":"TaskBadge","outputs":{},"interaction":null}).to_string();
    let types = "types: {TaskBadge: {base: Record, fields: {count: Int}}}";
    let package = Package::parse(&manifest, types).unwrap();
    json!({"format":1,"sdk":wes_views::sdk_version(),"manifest":manifest,"types":types,"definition":package.digest,"javascript":"export {};","css":""})
}

#[test]
fn artifact_pins_contract_and_executable_bytes_without_executing_them() {
    let s = source();
    let a = Artifact::parse(s.to_string().as_bytes()).unwrap();
    let b = Artifact::parse(serde_json::to_string_pretty(&s).unwrap().as_bytes()).unwrap();
    assert_eq!(a.digest, b.digest);
    let mut changed = s;
    changed["javascript"] = json!("throw new Error('fixture');");
    let changed = Artifact::parse(changed.to_string().as_bytes()).unwrap();
    assert_ne!(a.digest, changed.digest);
    assert_eq!(a.package.digest, changed.package.digest);
}

#[test]
fn malformed_unsupported_and_mismatched_artifacts_are_rejected() {
    for (key, value) in [
        ("definition", json!("wrong")),
        ("format", json!(2)),
        ("sdk", json!(9)),
        ("javascript", json!("")),
        ("url", json!("https://example.invalid/code.js")),
        ("css", json!(" ".repeat(256 * 1024 + 1))),
    ] {
        let mut s = source();
        s[key] = value;
        assert!(Artifact::parse(s.to_string().as_bytes()).is_err(), "{key}");
    }
    assert!(Artifact::parse(&vec![b' '; wes_views::MAX_ARTIFACT_BYTES + 1]).is_err());
    let mut previous = source();
    previous["sdk"] = json!(1);
    let error = Artifact::parse(previous.to_string().as_bytes()).unwrap_err();
    assert!(error.contains("Rebuild the source package"), "{error}");
}

#[test]
fn installation_is_atomic_idempotent_and_rejects_replacement_code_under_the_same_identity() {
    use std::sync::Arc;
    let original = Arc::new(Artifact::parse(source().to_string().as_bytes()).unwrap());
    let installed = wes_views::Catalogue::default()
        .installed(original.clone())
        .unwrap();
    assert_eq!(
        installed["TaskBadge"].artifact.as_deref(),
        Some(original.digest.as_str())
    );
    assert!(
        installed
            .installed(original.clone())
            .unwrap()
            .artifact(&original.digest)
            .is_some()
    );
    let mut other = source();
    other["javascript"] = json!("export const different=1;");
    assert!(
        installed
            .installed(Arc::new(
                Artifact::parse(other.to_string().as_bytes()).unwrap()
            ))
            .is_err()
    );
    assert_eq!(
        installed["TaskBadge"].artifact.as_deref(),
        Some(original.digest.as_str())
    );
}
