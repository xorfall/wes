use serde_json::{Value, json};
use wes_adapters::api_library::{Library, PackageKey, digest, draft, validate_descriptor};
fn candidate() -> Value {
    json!({"draftVersion":1,"diagnostics":["müşteri 🚀"],"provider":"items","types":{"Item":{"base":"Record","fields":{"id":{"type":"Int","optional":true}}}},"operations":[{"path":["listItems"],"method":"GET","route":"/items","summary":"müşteri 🚀","auth":[],"parameters":[],"responses":[{"status":null,"mediaType":null,"type":"List<Item>"}]}],"problems":[]})
}
fn ready() -> Value {
    let mut v = candidate();
    v["operations"][0]["responses"][0]["status"] = json!(200);
    v["operations"][0]["responses"][0]["mediaType"] = json!("application/json");
    v
}
fn key() -> PackageKey {
    PackageKey {
        service: "items".into(),
        api_version: "describe".into(),
        scope: "synthetic".into(),
    }
}
#[test]
fn missing_transport_preserves_shape_and_anchors_unicode_offsets() {
    let text = serde_json::to_string_pretty(&candidate()).unwrap();
    let result = draft::validate(&text);
    assert!(!result.valid);
    assert!(result.descriptor.is_none());
    assert_eq!(result.hash, digest(text.as_bytes()));
    assert!(result.preview.unwrap()["types"]["Item"].is_object());
    for code in ["DRAFT_STATUS", "DRAFT_MEDIA_UNKNOWN"] {
        let d = result.diagnostics.iter().find(|d| d.code == code).unwrap();
        let target = if code == "DRAFT_STATUS" {
            "\"status\": "
        } else {
            "\"mediaType\": "
        };
        let byte = text.find(target).unwrap() + target.len();
        assert_eq!(d.from, text[..byte].encode_utf16().count());
        assert_eq!(d.to - d.from, 4);
        assert_eq!(
            d.severity,
            if code == "DRAFT_STATUS" {
                "error"
            } else {
                "warning"
            }
        );
    }
    let mut missing = candidate();
    missing["operations"][0]["responses"][0]
        .as_object_mut()
        .unwrap()
        .remove("mediaType");
    let text = serde_json::to_string_pretty(&missing).unwrap();
    let result = draft::validate(&text);
    let d = result
        .diagnostics
        .iter()
        .find(|d| d.code == "DRAFT_MEDIA_UNKNOWN")
        .unwrap();
    let bytes: Vec<u16> = text.encode_utf16().collect();
    assert_eq!(bytes[d.from], b'{' as u16);
}
#[test]
fn strict_contract_requiredness_bodyless_and_budget_rules_still_apply() {
    assert!(draft::validate(&ready().to_string()).valid);
    for mutation in 0..9 {
        let mut v = ready();
        match mutation {
            0 => v["operations"][0]["responses"][0]["status"] = json!(600),
            1 => v["operations"][0]["responses"][0]["status"] = json!(204),
            2 => v["operations"][0]["responses"][0]["type"] = json!("Missing"),
            3 => {
                v["operations"][0]["parameters"] = json!([{"name":"q","wire":"q","type":"Text","location":"query","encoding":"scalar","required":"unknown"}])
            }
            4 => {
                v["problems"] =
                    json!([{"target":"#","message":"Mandatory rule needs confirmation"}])
            }
            5 => v["source"] = json!({"provenance":{"status":"current"}}),
            6 => v["types"]["Broken"] = json!({"base":"Int","min":10,"max":1}),
            7 => v["operations"][0]["responses"][0]["mediaType"] = json!("invalid media"),
            _ => v["operations"][0]["responses"][0]["mediaType"] = json!("invalid/media/type"),
        }
        assert!(
            !draft::validate(&v.to_string()).valid,
            "mutation {mutation}"
        );
    }
    let mut empty = ready();
    empty["operations"][0]["responses"][0] = json!({"status":204,"type":null,"mediaType":null});
    assert!(draft::validate(&empty.to_string()).valid);
    for bad in [
        "{\"draftVersion\":1,\"draftVersion\":1}".to_string(),
        format!("{}0{}", "[".repeat(140), "]".repeat(140)),
        " ".repeat(1024 * 1024 + 1),
    ] {
        assert!(!draft::validate(&bad).valid);
    }
    let r = draft::validate("{\n  \"x\": }");
    assert_eq!(r.diagnostics[0].code, "DRAFT_JSON");
    assert_eq!(r.diagnostics[0].line, 2);
}
#[test]
fn invalid_saves_cas_review_and_provenance_survive_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let evidence = json!({"provenance":{"version":1,"status":"current","entries":[{"target":"#/types/Item","basis":"example"}]}});
    let first = store
        .create_draft(
            key(),
            candidate().to_string(),
            evidence.clone(),
            b"synthetic source",
            "synthetic".into(),
        )
        .unwrap();
    assert_eq!(first["draft"]["valid"], false);
    assert!(store.index.packages.is_empty());
    assert!(first.get("descriptorPath").is_none());
    let rev = first["draft"]["revision"].as_str().unwrap();
    assert!(store.review_draft(&key(), rev).is_err());
    let broken = "{\n  \"half-written 🚀\": ";
    let second = store.save_draft(key(), rev, broken.into()).unwrap();
    assert_eq!(second["text"], broken);
    assert_eq!(second["evidence"]["source"], evidence);
    assert_eq!(second["evidence"]["status"], "stale");
    assert!(store.save_draft(key(), rev, ready().to_string()).is_err());
    let good = store
        .save_draft(
            key(),
            second["draft"]["revision"].as_str().unwrap(),
            ready().to_string(),
        )
        .unwrap();
    assert_eq!(good["draft"]["valid"], true);
    assert_eq!(good["draft"]["accepted"], false);
    validate_descriptor(&std::fs::read(good["descriptorPath"].as_str().unwrap()).unwrap()).unwrap();
    let reviewed = store
        .review_draft(&key(), good["draft"]["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(reviewed["draft"]["accepted"], true);
    let text = format!("{}\n", ready());
    let next = store
        .save_draft(key(), good["draft"]["revision"].as_str().unwrap(), text)
        .unwrap();
    assert_eq!(next["draft"]["accepted"], false);
    assert!(
        store
            .review_draft(&key(), good["draft"]["revision"].as_str().unwrap())
            .is_err()
    );
    drop(store);
    let store = Library::open(&root).unwrap();
    assert_eq!(store.drafts().unwrap().len(), 4);
    assert_eq!(
        store
            .inspect_draft(&key(), second["draft"]["revision"].as_str().unwrap())
            .unwrap()["text"],
        broken
    );
    assert_eq!(
        store
            .inspect_draft(&key(), good["draft"]["revision"].as_str().unwrap())
            .unwrap()["draft"]["accepted"],
        true
    );
}
#[test]
fn manual_correction_is_recorded_at_actual_paths_and_does_not_mutate_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Library::open(&temp.path().canonicalize().unwrap().join("library")).unwrap();
    let source =
        json!({"sha256":"synthetic","provenance":{"version":1,"status":"current","entries":[]}});
    let initial = store
        .create_draft(
            key(),
            candidate().to_string(),
            source.clone(),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let result = store
        .save_draft(
            key(),
            initial["draft"]["revision"].as_str().unwrap(),
            ready().to_string(),
        )
        .unwrap();
    assert_eq!(result["evidence"]["source"], source);
    assert_eq!(
        result["evidence"]["manualTargets"],
        json!([
            "#/operations/0/responses/0/mediaType",
            "#/operations/0/responses/0/status"
        ])
    );
    assert_eq!(
        result["descriptor"]["source"]["provenance"]["status"],
        "stale"
    );
}
#[cfg(unix)]
#[test]
fn draft_artifacts_refuse_symlinks_and_tampering() {
    use std::os::unix::fs::symlink;
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let result = store
        .create_draft(
            key(),
            candidate().to_string(),
            json!({}),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let rev = result["draft"]["revision"].as_str().unwrap();
    let path = root.join(format!("drafts/{rev}.json"));
    let original = std::fs::read(&path).unwrap();
    std::fs::write(&path, b"{}").unwrap();
    assert!(store.inspect_draft(&key(), rev).is_err());
    std::fs::remove_file(&path).unwrap();
    let outside = temp.path().join("outside");
    std::fs::write(&outside, original).unwrap();
    symlink(&outside, &path).unwrap();
    assert!(store.inspect_draft(&key(), rev).is_err());
}

#[test]
fn executable_path_cannot_be_rebound_to_an_unrelated_valid_descriptor() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let first = store
        .create_draft(
            key(),
            ready().to_string(),
            json!({}),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let mut changed = ready();
    changed["operations"][0]["route"] = json!("/other");
    let second = store
        .save_draft(
            key(),
            first["draft"]["revision"].as_str().unwrap(),
            changed.to_string(),
        )
        .unwrap();
    let mut index: Value =
        serde_json::from_slice(&std::fs::read(root.join("draft-index.json")).unwrap()).unwrap();
    index[0]["descriptorRevision"] = second["draft"]["descriptorRevision"].clone();
    std::fs::write(
        root.join("draft-index.json"),
        serde_json::to_vec(&index).unwrap(),
    )
    .unwrap();
    assert!(
        store
            .inspect_draft(&key(), first["draft"]["revision"].as_str().unwrap())
            .unwrap_err()
            .to_string()
            .contains("does not match")
    );
}

#[test]
fn behavior_advisories_are_visible_with_errors_and_survive_descriptor_materialization() {
    let message = "createItem: Writes are simulated; changes are not persisted.";
    for complete in [false, true] {
        let mut v = if complete { ready() } else { candidate() };
        v["diagnostics"] = json!([message]);
        let text = serde_json::to_string_pretty(&v).unwrap();
        let checked = draft::validate(&text);
        assert_eq!(checked.valid, complete);
        let warnings: Vec<_> = checked
            .diagnostics
            .iter()
            .filter(|d| d.message == message)
            .collect();
        assert_eq!(warnings.len(), 1);
        assert_eq!(warnings[0].severity, "warning");
        assert_eq!(warnings[0].target, "#/diagnostics/0");
        let utf16: Vec<_> = text.encode_utf16().collect();
        assert_eq!(
            String::from_utf16(&utf16[warnings[0].from..warnings[0].to]).unwrap(),
            json!(message).to_string()
        );
        if let Some(descriptor) = checked.descriptor {
            assert_eq!(descriptor["diagnostics"], json!([message]));
            assert!(
                validate_descriptor(&serde_json::to_vec(&descriptor).unwrap())
                    .unwrap()
                    .contains(&message.into())
            );
        } else {
            assert!(
                checked
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "DRAFT_STATUS" && d.severity == "error")
            );
        }
    }
    let mut v = ready();
    v["diagnostics"] = json!([message]);
    v["problems"] = json!([{"target":"#", "message":"Mandatory cross-field rule"}]);
    assert!(!draft::validate(&v.to_string()).valid);
    for malformed in [Value::Null, json!("note"), json!([{"message":"note"}])] {
        v["diagnostics"] = malformed;
        assert!(
            draft::validate(&v.to_string())
                .diagnostics
                .iter()
                .any(|d| d.code == "DRAFT_DIAGNOSTICS" && d.severity == "error")
        );
    }
}

#[test]
fn explicit_problem_reclassification_creates_a_revision_with_manual_provenance() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let mut v = ready();
    v["problems"] = json!([{"target":"#/operations/0", "message":"Writes are simulated."}]);
    let old_text = v.to_string();
    let first = store
        .create_draft(
            key(),
            old_text.clone(),
            json!({}),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let old = first["draft"]["revision"].as_str().unwrap();
    v["problems"] = json!([]);
    v["diagnostics"] = json!(["#/operations/0: Writes are simulated."]);
    let saved = store.save_draft(key(), old, v.to_string()).unwrap();
    assert_eq!(saved["draft"]["valid"], true);
    assert_eq!(saved["draft"]["accepted"], false);
    assert!(
        saved["evidence"]["manualTargets"]
            .as_array()
            .unwrap()
            .contains(&json!("#/problems/0"))
    );
    assert_eq!(saved["descriptor"]["diagnostics"], v["diagnostics"]);
    drop(store);
    let store = Library::open(&root).unwrap();
    assert_eq!(store.inspect_draft(&key(), old).unwrap()["text"], old_text);
    let restored = store
        .inspect_draft(&key(), saved["draft"]["revision"].as_str().unwrap())
        .unwrap();
    assert_eq!(restored["draft"]["valid"], true);
    assert_eq!(restored["descriptor"]["diagnostics"], v["diagnostics"]);
}

#[test]
fn unknown_requiredness_and_response_shape_are_usable_without_fabricated_guarantees() {
    for location in ["query", "header", "body", "path"] {
        let mut v = ready();
        if location == "body" {
            v["operations"][0]["method"] = json!("POST");
        }
        if location == "path" {
            v["operations"][0]["route"] = json!("/items/{q}");
        }
        v["operations"][0]["parameters"] = json!([{"name":"q","wire":if location == "body" {"body"} else {"q"},"type":"Text","location":location,"encoding":if location == "body" {"json"} else {"scalar"},"required":null}]);
        v["operations"][0]["responses"][0]
            .as_object_mut()
            .unwrap()
            .remove("type");
        let checked = draft::validate(&v.to_string());
        assert!(checked.valid, "{location}: {:?}", checked.diagnostics);
        assert_eq!(
            checked.preview.as_ref().unwrap()["operations"][0]["parameters"][0]["required"],
            Value::Null
        );
        let descriptor = checked.descriptor.unwrap();
        assert_eq!(
            descriptor["operations"][0]["parameters"][0]["required"],
            location == "path"
        );
        assert_eq!(descriptor["operations"][0]["responses"]["200"], "Unknown");
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|d| d.code == "DRAFT_REQUIREDNESS_UNKNOWN" && d.severity == "warning")
        );
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|d| d.code == "DRAFT_UNKNOWN_RESPONSE" && d.severity == "warning")
        );
        assert!(
            validate_descriptor(&serde_json::to_vec(&descriptor).unwrap())
                .unwrap()
                .iter()
                .any(|d| d.contains("Requiredness"))
        );
        // Unknown response media remains a warning; request encoding remains definite.
        v["operations"][0]["responses"][0]["mediaType"] = Value::Null;
        assert!(draft::validate(&v.to_string()).valid);
    }
    let mut v = ready();
    v["operations"][0]["responses"][0]["type"] = json!("Unknown");
    let valid = draft::validate(&v.to_string());
    assert!(valid.valid);
    // Previously valid explicit Unknown descriptors retain their original materialized bytes.
    assert_eq!(valid.descriptor.unwrap()["diagnostics"], v["diagnostics"]);
    v["operations"][0]["responses"][0]["status"] = json!(204);
    assert!(!draft::validate(&v.to_string()).valid);
    v["operations"][0]["responses"][0] = json!({"status":204,"mediaType":null,"type":null});
    assert!(draft::validate(&v.to_string()).valid);
    v["operations"][0]["route"] = json!("/items/{id}");
    v["operations"][0]["parameters"] = json!([{"name":"id","wire":"id","type":"Int","location":"path","encoding":"scalar","required":false}]);
    assert!(!draft::validate(&v.to_string()).valid); // explicit contradictory declarations are not rewritten
}

#[test]
fn newly_valid_retained_text_materializes_only_on_explicit_save() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().canonicalize().unwrap().join("library");
    let mut store = Library::open(&root).unwrap();
    let mut text = ready();
    text["operations"][0]["responses"][0]
        .as_object_mut()
        .unwrap()
        .remove("type");
    let first = store
        .create_draft(
            key(),
            text.to_string(),
            json!({}),
            b"synthetic",
            "fixture".into(),
        )
        .unwrap();
    let old = first["draft"]["revision"].as_str().unwrap();
    // Model the catalog entry retained by an older validator which could not publish this text.
    let index_path = root.join("draft-index.json");
    let mut index: Value = serde_json::from_slice(&std::fs::read(&index_path).unwrap()).unwrap();
    index[0]["valid"] = json!(false);
    index[0]["descriptorRevision"] = Value::Null;
    std::fs::write(&index_path, serde_json::to_vec(&index).unwrap()).unwrap();
    assert!(
        store
            .inspect_draft(&key(), old)
            .unwrap()
            .get("descriptorPath")
            .is_none()
    );
    let next = store.save_draft(key(), old, text.to_string()).unwrap();
    assert_ne!(next["draft"]["revision"], old);
    assert!(next.get("descriptorPath").is_some());
    assert_eq!(next["text"], first["text"]);
    assert_eq!(next["evidence"], first["evidence"]);
    assert!(
        store
            .inspect_draft(&key(), old)
            .unwrap()
            .get("descriptorPath")
            .is_none()
    );
}

#[test]
fn draft_materialization_preserves_source_fields() {
    let original = ready();
    let mut expected = original.clone();
    let root = expected.as_object_mut().unwrap();
    root.remove("draftVersion");
    root.remove("problems");
    root.insert("version".into(), json!(1));
    root["operations"][0]["responses"] = json!({"200":"List<Item>"});
    let actual = draft::validate(&original.to_string()).descriptor.unwrap();
    assert_eq!(
        serde_json::to_vec_pretty(&actual).unwrap(),
        serde_json::to_vec_pretty(&expected).unwrap()
    );
}

#[test]
fn unknown_response_media_warns_without_inventing_facts_or_relaxing_requests() {
    for omitted in [false, true] {
        let mut v = ready();
        v["operations"][0]["responses"][0]["mediaType"] = Value::Null;
        if omitted {
            v["operations"][0]["responses"][0]
                .as_object_mut()
                .unwrap()
                .remove("mediaType");
        }
        let checked = draft::validate(&v.to_string());
        assert!(checked.valid, "{:?}", checked.diagnostics);
        assert!(checked.preview.unwrap()["operations"][0]["responses"][0]["mediaType"].is_null());
        assert!(
            checked
                .diagnostics
                .iter()
                .any(|d| d.code == "DRAFT_MEDIA_UNKNOWN" && d.severity == "warning")
        );
        assert!(
            checked.descriptor.unwrap()["diagnostics"]
                .as_array()
                .unwrap()
                .iter()
                .any(|v| v.as_str().unwrap().contains("media type is unknown"))
        );
        v["operations"][0]["parameters"] = json!([{"name":"body","wire":"body","location":"body","type":"Item","encoding":"unknown","required":true}]);
        assert!(!draft::validate(&v.to_string()).valid);
    }
    for media in [json!(42), json!({}), json!(""), json!("text/event-stream")] {
        let mut v = ready();
        v["operations"][0]["responses"][0]["mediaType"] = media;
        assert!(!draft::validate(&v.to_string()).valid);
    }
}

#[test]
fn constraint_notes_survive_library_export_and_manual_edits_without_forged_evidence() {
    let temp = tempfile::tempdir().unwrap();
    let mut library = Library::open(&temp.path().canonicalize().unwrap().join("library")).unwrap();
    let mut v = ready();
    v["notes"] = json!([{"kind":"constraint","target":"#/types/Item","description":"The item limit must be lower than the account limit.","enforcement":"not-checked-locally"}]);
    let source = json!({"sha256":"synthetic","provenance":{"version":1,"status":"current","entries":[{"target":"#/notes/0","basis":"documented","source":"sha256:synthetic","lines":[{"start":1,"end":1}]}]}});
    let saved = library
        .create_draft(
            key(),
            v.to_string(),
            source.clone(),
            b"synthetic",
            "synthetic".into(),
        )
        .unwrap();
    assert_eq!(saved["draft"]["valid"], true);
    let path = saved["descriptorPath"].as_str().unwrap();
    let exported: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(exported["notes"], v["notes"]);
    assert_eq!(exported["source"]["provenance"], source["provenance"]);
    let revision = saved["draft"]["revision"].as_str().unwrap();
    v["notes"][0]["description"] = json!("Reviewed constraint wording.");
    let next = library.save_draft(key(), revision, v.to_string()).unwrap();
    assert_eq!(next["evidence"]["source"], source);
    assert_eq!(next["evidence"]["status"], "stale");
    let exported: Value =
        serde_json::from_slice(&std::fs::read(next["descriptorPath"].as_str().unwrap()).unwrap())
            .unwrap();
    assert_eq!(exported["source"]["provenance"]["status"], "stale");
    for mutation in 0..5 {
        let mut bad = v.clone();
        match mutation {
            0 => bad["notes"][0]["enforcement"] = json!("locally-checked"),
            1 => bad["notes"][0]["target"] = json!("#/missing"),
            2 => bad["notes"][0]["description"] = json!("a".repeat(2049)),
            3 => bad["types"]["Item"]["fields"]["id"]["type"] = json!("Missing"),
            _ => {
                bad["problems"] = json!([{"target":"#","message":"cross-field constraint: old unresolved problem"}])
            }
        }
        assert!(
            !draft::validate(&bad.to_string()).valid,
            "mutation {mutation}"
        );
    }
}

#[test]
fn provider_information_example_keeps_json_transport_and_visible_unchecked_rule() {
    let text = include_str!("../../../examples/provider-information/draft.json");
    let checked = draft::validate(text);
    assert!(checked.valid, "{:?}", checked.diagnostics);
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == "DRAFT_CONSTRAINT_NOTE")
    );
    assert!(
        checked
            .diagnostics
            .iter()
            .any(|d| d.code == "DRAFT_MEDIA_UNKNOWN")
    );
    let d = checked.descriptor.unwrap();
    assert_eq!(d["operations"][0]["parameters"][0]["encoding"], "json");
    assert_eq!(
        d["notes"][0]["description"],
        "budget must be less than limit."
    );
    validate_descriptor(&serde_json::to_vec(&d).unwrap()).unwrap();
}

#[test]
fn descriptor_to_draft_does_not_invent_response_media() {
    let descriptor = draft::validate(&ready().to_string()).descriptor.unwrap();
    let (text, _) = draft::from_descriptor(&serde_json::to_vec(&descriptor).unwrap()).unwrap();
    let reopened: Value = serde_json::from_str(&text).unwrap();
    assert!(reopened["operations"][0]["responses"][0]["mediaType"].is_null());
    assert!(draft::validate(&text).valid);
}

#[test]
fn drafts_admit_error_statuses_text_and_bytes_without_inventing_success() {
    for (media, kind) in [
        ("text/plain", "Text"),
        ("application/octet-stream", "Bytes"),
        ("application/problem+json", "Unknown"),
    ] {
        let mut v = ready();
        v["operations"][0]["responses"] = json!([{"status":400,"mediaType":media,"type":kind}]);
        let checked = draft::validate(&v.to_string());
        assert!(checked.valid, "{:?}", checked.diagnostics);
        let descriptor = checked.descriptor.unwrap();
        assert_eq!(descriptor["version"], 1);
        assert_eq!(descriptor["operations"][0]["responses"]["400"], kind);
    }
}

#[test]
fn documentation_round_trips_without_changing_execution_authority() {
    let mut value = ready();
    value["operations"][0]["description"] =
        json!("A **documented** operation.\n\n<script>inert</script>");
    value["operations"][0]["responseDescriptions"] =
        json!({"200":"Matching items", "400":"Invalid input", "default":"Other errors"});
    value["operations"][0]["parameters"] = json!([{"name":"limit","wire":"limit","location":"query","type":"Int","required":false,"encoding":"scalar","description":"Maximum items."}]);
    value["types"]["Item"]["description"] = json!("An item.");
    value["types"]["Item"]["fields"]["id"]["description"] = json!("Stable id.");
    let validation = draft::validate(&value.to_string());
    assert!(validation.valid, "{:?}", validation.diagnostics);
    let doc = validation.descriptor.unwrap();
    assert_eq!(
        doc["operations"][0]["responses"],
        json!({"200":"List<Item>"})
    );
    for at in [
        "/operations/0/description",
        "/operations/0/responseDescriptions",
        "/operations/0/parameters/0/description",
        "/types/Item/description",
        "/types/Item/fields/id/description",
    ] {
        assert_eq!(doc.pointer(at), value.pointer(at));
    }
    let (text, _) = draft::from_descriptor(&serde_json::to_vec(&doc).unwrap()).unwrap();
    let round_trip = draft::validate(&text);
    assert!(round_trip.valid);
    let restored = round_trip.descriptor.unwrap();
    assert_eq!(restored["operations"], doc["operations"]);
    assert_eq!(restored["types"], doc["types"]);
    // The existing descriptor format does not retain mediaType. Its known advisory
    // is independent of documentation and is not suppressed by this round trip.
    assert!(
        restored["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v.as_str().unwrap().contains("media type is unknown"))
    );
    for at in [
        "/operations/0/description",
        "/operations/0/responseDescriptions/200",
        "/operations/0/parameters/0/description",
        "/types/Item/description",
        "/types/Item/fields/id/description",
    ] {
        let mut invalid = value.clone();
        *invalid.pointer_mut(at).unwrap() = json!({"html":"not a string"});
        assert!(!draft::validate(&invalid.to_string()).valid, "{at}");
    }
    value["operations"][0]["parameters"][0]["encoding"] = json!("invented");
    assert!(!draft::validate(&value.to_string()).valid);
}

#[test]
fn an_explicit_anonymous_auth_option_does_not_get_an_unknown_auth_warning() {
    let bytes = include_bytes!("../../../examples/http-auth-alternatives/service.json");
    let (draft, _) = wes_adapters::api_library::draft::from_descriptor(bytes).unwrap();
    let checked = wes_adapters::api_library::draft::validate(&draft);
    assert!(checked.valid, "{:?}", checked.diagnostics);
    assert!(
        !checked
            .diagnostics
            .iter()
            .any(|issue| issue.code == "DRAFT_AUTH"),
        "{:?}",
        checked.diagnostics
    );
}
