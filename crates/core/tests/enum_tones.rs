use serde_json::{Value as Json, json};
use wes_core::{
    Data,
    contracts::{ContractRegistry, EnumTone, metadata::ValueMetadata},
};
fn metadata(r: &ContractRegistry, name: &str) -> Json {
    let m = ValueMetadata::capture(&r.resolve(name).unwrap());
    m.validate().unwrap();
    serde_json::to_value(m).unwrap()
}
#[test]
fn narrowed_aliases_inherit_override_and_keep_display_outside_constraints() {
    let mut r = ContractRegistry::new();
    r.load("version: 2\ntypes: {Status: {base: Text, enum: [ready, failed, unknown], display: {enumTones: {ready: ok, failed: bad, unknown: dim}}}, Narrow: {base: Status, enum: [ready, failed], display: {enumTones: {ready: ink}}}, Child: {base: Narrow, enum: [ready]}} ").unwrap();
    assert_eq!(
        metadata(&r, "Status")["fields"][""]["tones"],
        json!({"ready":"ok","failed":"bad","unknown":"dim"})
    );
    assert_eq!(
        metadata(&r, "Narrow")["fields"][""]["tones"],
        json!({"ready":"ink","failed":"bad"})
    );
    assert_eq!(
        metadata(&r, "Child")["fields"][""]["tones"],
        json!({"ready":"ink"})
    );
    assert!(
        r.resolve("Narrow")
            .unwrap()
            .is_subtype_of(&r.resolve("Status").unwrap())
    );
    let mut plain = ContractRegistry::new();
    plain
        .load("types: {Status: {base: Text, enum: [ready, failed, unknown]}} ")
        .unwrap();
    let displayed = r.resolve("Status").unwrap();
    let plain = plain.resolve("Status").unwrap();
    assert!(displayed.is_subtype_of(&plain) && plain.is_subtype_of(&displayed));
    assert_eq!(
        displayed.issues(&Data::Text("failed".into())),
        plain.issues(&Data::Text("failed".into()))
    );
    assert_ne!(displayed.digest(), plain.digest());
    assert_eq!(
        EnumTone::NAMES,
        &["ok", "warn", "bad", "dim", "meta", "ink"]
    );
}
#[test]
fn invalid_display_declarations_are_typ002_and_loading_is_atomic() {
    for yaml in [
        "version: 1\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {x: ok}}}}",
        "types: {Bad: {base: Text, enum: [x], display: {enumTones: {x: ok}}}}",
        "version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {x: inherit}}}}",
        "version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {x: '#fff'}}}}",
        "version: 2\ntypes: {Bad: {base: Text, display: {enumTones: {x: ok}}}}",
        "version: 2\ntypes: {Bad: {base: Int, enum: [1], display: {enumTones: {'2': ok}}}}",
        "version: 2\ntypes: {Bad: {base: Int, enum: [1], display: {enumTones: {'01': ok}}}}",
        "version: 2\ntypes: {Bad: {base: Bool, enum: [true], display: {enumTones: {'True': ok}}}}",
        "version: 2\ntypes: {Bad: {base: Decimal, enum: [1.00], display: {enumTones: {'1.0': ok}}}}",
        "version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {x: ok, x: bad}}}}",
        "version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {other: {x: ok}}}}",
        "version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {x: ok}, extra: true}}}",
        "version: 2\ntypes: {Bad: {base: Int, enum: [1], display: {enumTones: {1: ok}}}}",
        "version: 2\ntypes: {Bad: {base: Bool, enum: [true], display: {enumTones: {true: ok}}}}",
        "version: 2\ntypes: {Bad: {base: 'List<Text>', display: {enumTones: {x: ok}}}}",
    ] {
        let mut r = ContractRegistry::new();
        let error = r.load(yaml).unwrap_err();
        assert_eq!(error.code, "TYP002", "{yaml}");
        assert!(r.resolve("Bad").is_err());
        assert!(r.sources().is_empty());
    }
    let mut r = ContractRegistry::new();
    let error=r.load("version: 2\ntypes: {Bad: {base: Text, enum: [x], display: {enumTones: {outside: ok}}}} ").unwrap_err();
    assert!(error.message.contains("Bad") && error.message.contains("outside"));
    // Environment packages keep their independent version-1 declaration.
    assert!(wes_core::environments::Package::parse("version: 2\nenvironments: {} ").is_err());
    let mut r = ContractRegistry::new();
    r.load("version: 1\ntypes: {Legacy: {base: Text, enum: [x]}} ")
        .unwrap();
}
#[test]
fn exact_numbers_and_bool_keys_are_strings_and_tones_survive_beyond_preview() {
    let mut r = ContractRegistry::new();
    r.load("version: 2\ntypes: {I: {base: Int, enum: [9223372036854775807], display: {enumTones: {'9223372036854775807': warn}}}, D: {base: Decimal, enum: [1.23000000000000000001], display: {enumTones: {'1.23000000000000000001': meta}}}, B: {base: Bool, enum: [true, false], display: {enumTones: {'true': ok, 'false': bad}}}} ").unwrap();
    assert_eq!(
        metadata(&r, "I")["fields"][""]["tones"],
        json!({"9223372036854775807":"warn"})
    );
    assert_eq!(
        metadata(&r, "D")["fields"][""]["tones"],
        json!({"1.23000000000000000001":"meta"})
    );
    assert_eq!(
        metadata(&r, "B")["fields"][""]["tones"],
        json!({"true":"ok","false":"bad"})
    );
    let members = (0..200)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join(",");
    r.load(&format!("version: 2\ntypes: {{Many: {{base: Text, enum: [{members}], display: {{enumTones: {{s199: bad}}}}}}}}")).unwrap();
    let m = metadata(&r, "Many");
    let d = &m["fields"][""];
    assert_eq!(d["complete"], false);
    assert_eq!(d["total"], 200);
    assert_eq!(d["members"].as_array().unwrap().len(), 64);
    assert_eq!(d["tones"]["s199"], "bad");
}
#[test]
fn tone_limits_include_inheritance_and_descriptor_bytes_and_imports_detect_display_conflicts() {
    let members = (0..65)
        .map(|i| format!("s{i}"))
        .collect::<Vec<_>>()
        .join(",");
    let tones = (0..65)
        .map(|i| format!("s{i}: ok"))
        .collect::<Vec<_>>()
        .join(",");
    let mut r = ContractRegistry::new();
    assert_eq!(r.load(&format!("version: 2\ntypes:\n Bad:\n  base: Text\n  enum: [{members}]\n  display:\n   enumTones: {{{tones}}}\n")).unwrap_err().code,"TYP002");
    let long = "ğ".repeat(9000);
    let q = serde_json::to_string(&long).unwrap();
    assert_eq!(r.load(&format!("version: 2\ntypes:\n Bad:\n  base: Text\n  enum: [{q}]\n  display:\n   enumTones: {{{q}: ok}}\n")).unwrap_err().code,"TYP002");
    let tones = (0..64)
        .map(|i| format!("s{i}: ok"))
        .collect::<Vec<_>>()
        .join(",");
    r.load(&format!("version: 2\ntypes:\n Parent:\n  base: Text\n  enum: [{members}]\n  display:\n   enumTones: {{{tones}}}\n")).unwrap();
    assert_eq!(
        r.load("version: 2\ntypes: {Bad: {base: Parent, display: {enumTones: {s64: ok}}}} ")
            .unwrap_err()
            .code,
        "TYP002"
    );
    let package = "version: 2\ntypes: {Status: {base: Text, enum: [ready], display: {enumTones: {ready: ok}}}}";
    r.import_package(package).unwrap();
    assert!(r.import_package(package).unwrap().is_empty());
    let old = r.resolve("Status").unwrap().digest().to_owned();
    assert_eq!(
        r.import_package(&package.replace("ready: ok", "ready: bad"))
            .unwrap_err()
            .code,
        "TYP004"
    );
    assert_eq!(r.resolve("Status").unwrap().digest(), old);
}

#[test]
fn complete_tone_map_precedes_member_preview_in_the_utf8_descriptor_budget() {
    let members = (0..64)
        .map(|i| format!("'{}{}'", "ğ".repeat(100), i))
        .collect::<Vec<_>>()
        .join(",");
    let tones = (0..64)
        .map(|i| format!("'{}{}': ok", "ğ".repeat(100), i))
        .collect::<Vec<_>>()
        .join(",");
    let mut r = ContractRegistry::new();
    r.load(&format!("version: 2\ntypes:\n S:\n  base: Text\n  enum: [{members}]\n  display:\n   enumTones: {{{tones}}}\n")).unwrap();
    let m = metadata(&r, "S");
    let d = &m["fields"][""];
    assert_eq!(d["tones"].as_object().unwrap().len(), 64);
    assert!(d["members"].as_array().unwrap().len() < 64);
    assert_eq!(d["complete"], false);
    assert!(serde_json::to_vec(d).unwrap().len() <= 16 * 1024);
}
