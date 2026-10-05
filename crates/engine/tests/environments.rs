use wes_core::environments::{CapturedSource, CapturedSources, Package, Revision, SourceKey};
use wes_engine::environments::{Limits, Registry};

fn package(body: &str) -> Package {
    Package::parse(&format!(
        "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n{body}"
    ))
    .unwrap()
}
fn evidence(p: &Package, payload: &str) -> CapturedSources {
    let mut result = CapturedSources::default();
    for key in p.required_sources() {
        result
            .insert(key, CapturedSource::new("fixture/v1", payload).unwrap())
            .unwrap();
    }
    result
}
fn apply(r: &mut Registry, p: &Package) {
    let plan = r
        .plan(p, &evidence(p, "synthetic descriptor evidence"))
        .unwrap();
    r.apply(plan).unwrap();
}
fn import() -> &'static str {
    "{source: {kind: spec, file: fixture.json}, bind: {target: local}}"
}
fn base_and_dev() -> Package {
    package(&format!(
        "  base: {{imports: {{api: {}}}}}\n  dev: {{extends: {{env: base, track: latest}}}}",
        import()
    ))
}

#[test]
fn plans_are_inert_and_all_descendants_publish_together() {
    let p = base_and_dev();
    let mut r = Registry::default();
    let plan = r.plan(&p, &evidence(&p, "one")).unwrap();
    assert_eq!(r.names().count(), 0);
    assert_eq!(plan.changes().len(), 2);
    assert!(plan.inspect("dev").unwrap().imports().contains_key("api"));
    r.apply(plan).unwrap();
    assert_eq!(r.names().collect::<Vec<_>>(), ["base", "dev"]);
    let selection = r.select("dev").unwrap();
    let bound = r.bind(&selection, "api").unwrap();
    assert_eq!(bound.import().origin().environment, "base");
    assert_eq!(bound.environment().name(), "dev");
}

#[test]
fn tracking_updates_pins_and_old_bindings_do_not_move() {
    let mut r = Registry::default();
    apply(&mut r, &base_and_dev());
    let pin = r.inspect("base").unwrap().revision();
    let pinned = package(&format!(
        "  base: {{imports: {{api: {}}}}}\n  dev: {{extends: {{env: base, track: latest}}}}\n  prod: {{extends: {{env: base, revision: '{pin}'}}}}",
        import()
    ));
    apply(&mut r, &pinned);
    let selected = r.select("dev").unwrap();
    let old_dev = r.bind(&selected, "api").unwrap();
    let old_prod = r.select("prod").unwrap();
    let updated = package(&format!(
        "  base: {{imports: {{api: {}, extra: {}}}}}\n  dev: {{extends: {{env: base, track: latest}}}}\n  prod: {{extends: {{env: base, revision: '{pin}'}}}}",
        import(),
        import()
    ));
    let plan = r
        .plan(
            &updated,
            &evidence(&updated, "synthetic descriptor evidence"),
        )
        .unwrap();
    assert_eq!(
        plan.changes()
            .iter()
            .map(|c| c.name.as_str())
            .collect::<Vec<_>>(),
        ["base", "dev"]
    );
    r.apply(plan).unwrap();
    assert!(r.inspect("dev").unwrap().imports().contains_key("extra"));
    assert!(!r.inspect("prod").unwrap().imports().contains_key("extra"));
    assert_eq!(r.select("prod").unwrap(), old_prod);
    assert_eq!(r.bind(&selected, "api").unwrap_err().code, "ENV008");
    assert!(!old_dev.environment().imports().contains_key("extra"));
    assert!(r.bind(&old_prod, "api").is_ok());
}

#[test]
fn pin_freezes_transitive_parent_closure_and_captured_evidence() {
    let mut r = Registry::default();
    let first = package(&format!(
        "  grand: {{imports: {{api: {}}}}}\n  base: {{extends: {{env: grand, track: latest}}}}",
        import()
    ));
    apply(&mut r, &first);
    let pin = r.inspect("base").unwrap().revision();
    let p = package(&format!(
        "  grand: {{imports: {{api: {}}}}}\n  base: {{extends: {{env: grand, track: latest}}}}\n  prod: {{extends: {{env: base, revision: '{pin}'}}}}",
        import()
    ));
    apply(&mut r, &p);
    let prod = r.select("prod").unwrap();
    let plan = r.plan(&p, &evidence(&p, "new evidence")).unwrap();
    r.apply(plan).unwrap();
    assert_eq!(r.select("prod").unwrap(), prod);
    assert_eq!(
        r.inspect("prod").unwrap().imports()["api"].source().bytes(),
        "synthetic descriptor evidence"
    );
    assert_eq!(
        r.inspect("base").unwrap().imports()["api"].source().bytes(),
        "new evidence"
    );
}

#[test]
fn child_config_rebinds_slots_but_never_literal_destinations() {
    let p = package(
        r#"  base:
    abstract: true
    parameters: {url: {type: Text}, timeout: {type: Int, default: 5000}}
    imports:
      api:
        source: {kind: spec, file: api.json}
        bind: {target: local, endpoint: {config: url}, timeout_ms: {config: timeout}}
      fixed:
        source: {kind: spec, file: fixed.json}
        bind: {target: local, endpoint: 'https://literal.example.invalid'}
  dev:
    extends: {env: base, track: latest}
    config: {url: 'https://dev.example.invalid'}
  prod:
    extends: {env: base, track: latest}
    config: {url: 'https://prod.example.invalid', timeout: 9000}
"#,
    );
    let mut r = Registry::default();
    apply(&mut r, &p);
    for (name, endpoint, timeout) in [
        ("dev", "https://dev.example.invalid", 5000),
        ("prod", "https://prod.example.invalid", 9000),
    ] {
        let e = r.inspect(name).unwrap();
        assert_eq!(e.imports()["api"].endpoint(), Some(endpoint));
        assert_eq!(e.imports()["api"].timeout_ms(), Some(timeout));
        assert_eq!(
            e.imports()["fixed"].endpoint(),
            Some("https://literal.example.invalid")
        );
        assert_eq!(e.imports()["api"].origin().environment, "base");
    }
    assert_eq!(
        r.bind(&r.select("base").unwrap(), "api").unwrap_err().code,
        "ENV005"
    );
}

#[test]
fn parent_secret_assignments_do_not_inherit_into_child() {
    let base = r#"  base:
    secretSlots: {token: {required: true}}
    secretRefs: {token: base/credential}
    imports:
      api:
        source: {kind: spec, file: api.json}
        bind: {target: local, credentials: {authorization: {secret: token}}}
"#;
    let p = package(&format!(
        "{base}  dev: {{extends: {{env: base, track: latest}}}}"
    ));
    let mut r = Registry::default();
    assert_eq!(
        r.plan(&p, &evidence(&p, "fixture")).unwrap_err().code,
        "ENV003"
    );
    let p = package(&format!(
        "{base}  dev: {{extends: {{env: base, track: latest}}, secretRefs: {{token: dev/credential}}}}"
    ));
    apply(&mut r, &p);
    assert_eq!(
        r.inspect("dev").unwrap().imports()["api"].credential_refs()["authorization"],
        "dev/credential"
    );
    assert_eq!(
        r.inspect("base").unwrap().secret_refs()["token"],
        "base/credential"
    );
}

#[test]
fn override_and_hide_are_explicit_and_complete() {
    let p = package(&format!(
        "  base: {{imports: {{api: {}, hidden: {}}}}}\n  dev:\n    extends: {{env: base, track: latest}}\n    overrides: {{imports: {{api: {{source: {{kind: spec, file: replacement.json}}, bind: {{target: local, endpoint: 'https://dev.example.invalid'}}}}}}}}\n    hide: {{imports: [hidden]}}",
        import(),
        import()
    ));
    let mut r = Registry::default();
    apply(&mut r, &p);
    let dev = r.inspect("dev").unwrap();
    assert_eq!(dev.imports().len(), 1);
    assert_eq!(dev.imports()["api"].origin().environment, "dev");
    assert_eq!(
        dev.imports()["api"].source().bytes(),
        "synthetic descriptor evidence"
    );
    assert_eq!(
        dev.imports()["api"].endpoint(),
        Some("https://dev.example.invalid")
    );
}

#[test]
fn parent_addition_conflicting_with_child_rejects_entire_publication() {
    let first = package(&format!(
        "  base: {{}}\n  dev: {{extends: {{env: base, track: latest}}, imports: {{api: {}}}}}",
        import()
    ));
    let mut r = Registry::default();
    apply(&mut r, &first);
    let before = r.select("base").unwrap();
    let conflict = package(&format!(
        "  base: {{imports: {{api: {}}}}}\n  dev: {{extends: {{env: base, track: latest}}, imports: {{api: {}}}}}",
        import(),
        import()
    ));
    assert_eq!(
        r.plan(&conflict, &evidence(&conflict, "fixture"))
            .unwrap_err()
            .code,
        "ENV004"
    );
    assert_eq!(r.select("base").unwrap(), before);
    assert!(r.inspect("base").unwrap().imports().is_empty());
}

#[test]
fn missing_parents_cycles_and_unavailable_pins_never_fallback() {
    let r = Registry::default();
    for body in [
        "  dev: {extends: {env: absent, track: latest}}".to_owned(),
        "  dev: {extends: {env: dev, track: latest}}".to_owned(),
        "  a: {extends: {env: b, track: latest}}\n  b: {extends: {env: a, track: latest}}"
            .to_owned(),
        format!(
            "  base: {{}}\n  dev: {{extends: {{env: base, revision: 'sha256:{}'}}}}",
            "0".repeat(64)
        ),
    ] {
        let p = package(&body);
        assert_eq!(r.plan(&p, &evidence(&p, "")).unwrap_err().code, "ENV002");
        assert_eq!(r.names().count(), 0);
    }
}

#[test]
fn wrong_config_unknown_slots_missing_requirements_and_invalid_overrides_reject() {
    let r = Registry::default();
    for body in [
        "  dev: {parameters: {x: {type: Text}}}".to_owned(),
        "  dev: {parameters: {x: {type: Int}}, config: {x: text}}".to_owned(),
        "  dev: {config: {x: 1}}".to_owned(),
        "  base: {parameters: {x: {type: Text, default: a}}}\n  dev: {extends: {env: base, track: latest}, parameters: {x: {type: Int, default: 1}}}".to_owned(),
        "  dev: {secretRefs: {x: missing/slot}}".to_owned(),
        "  dev: {hide: {imports: [absent]}}".to_owned(),
        format!("  dev: {{overrides: {{imports: {{absent: {}}}}}}}", import()),
        "  base: {abstract: true, secretSlots: {token: {required: true}}}\n  dev: {abstract: true, extends: {env: base, track: latest}, secretSlots: {token: {required: false}}}".to_owned(),
    ] {
        let p = package(&body);
        assert!(r.plan(&p, &evidence(&p, "fixture")).is_err(), "accepted {body}");
    }
}

#[test]
fn plans_are_owner_bound_revision_checked_and_old_plans_do_not_mutate() {
    let p = base_and_dev();
    let mut r = Registry::default();
    let pending = r.plan(&p, &evidence(&p, "one")).unwrap();
    let stale = r.plan(&p, &evidence(&p, "two")).unwrap();
    r.apply(pending).unwrap();
    let before = r.select("dev").unwrap();
    assert_eq!(r.apply(stale).unwrap_err().code, "ENV008");
    assert_eq!(r.select("dev").unwrap(), before);
    let foreign = Registry::default()
        .plan(&p, &evidence(&p, "three"))
        .unwrap();
    assert_eq!(r.apply(foreign).unwrap_err().code, "ENV008");
    assert_eq!(r.select("dev").unwrap(), before);
}

#[test]
fn no_op_apply_does_not_grow_history_or_invalidate_selections() {
    let p = base_and_dev();
    let mut r = Registry::default();
    apply(&mut r, &p);
    let before = r.select("dev").unwrap();
    let count = r.revision_count();
    let plan = r
        .plan(&p, &evidence(&p, "synthetic descriptor evidence"))
        .unwrap();
    assert!(plan.changes().is_empty());
    assert!(r.apply(plan).unwrap().is_empty());
    assert_eq!(r.revision_count(), count);
    assert_eq!(r.select("dev").unwrap(), before);
}

#[test]
fn yaml_order_comments_and_scalar_spelling_do_not_change_semantic_revision() {
    let first = package(
        "  dev: {parameters: {a: {type: Int, default: 1}, b: {type: Text, default: hello}}}",
    );
    let second = Package::parse("# reordered\nenvironments: {dev: {parameters: {b: {default: 'hello', type: Text}, a: {default: +1, type: Int}}}}\ntargets: {local: {kind: local}}\nversion: 1").unwrap();
    let r = Registry::default();
    assert_eq!(
        r.plan(&first, &evidence(&first, ""))
            .unwrap()
            .inspect("dev")
            .unwrap()
            .revision(),
        r.plan(&second, &evidence(&second, ""))
            .unwrap()
            .inspect("dev")
            .unwrap()
            .revision()
    );
}

#[test]
fn revision_distinguishes_evidence_config_types_origins_and_reference_changes() {
    let p = base_and_dev();
    let r = Registry::default();
    let a = r.plan(&p, &evidence(&p, "first")).unwrap();
    let b = r.plan(&p, &evidence(&p, "second")).unwrap();
    assert_ne!(
        a.inspect("dev").unwrap().revision(),
        b.inspect("dev").unwrap().revision()
    );
    let mut revisions = std::collections::BTreeSet::<Revision>::new();
    for def in [
        "parameters: {x: {type: Int, default: 1}}",
        "parameters: {x: {type: Text, default: '1'}}",
        "parameters: {x: {type: Bool, default: true}}",
        "secretSlots: {token: {required: true}}, secretRefs: {token: dev/a}",
        "secretSlots: {token: {required: true}}, secretRefs: {token: dev/b}",
    ] {
        let p = package(&format!("  dev: {{{def}}}"));
        revisions.insert(
            r.plan(&p, &evidence(&p, ""))
                .unwrap()
                .inspect("dev")
                .unwrap()
                .revision(),
        );
    }
    assert_eq!(revisions.len(), 5);
}

#[test]
fn image_restoration_reconstructs_data_not_old_plan_authority() {
    let p = base_and_dev();
    let mut original = Registry::default();
    apply(&mut original, &p);
    let plan = original.plan(&p, &evidence(&p, "changed")).unwrap();
    let mut restored = Registry::restore(original.image(), Limits::default()).unwrap();
    assert_eq!(
        original.select("dev").unwrap().revision(),
        restored.select("dev").unwrap().revision()
    );
    assert_eq!(restored.apply(plan).unwrap_err().code, "ENV008");
    assert_eq!(
        restored
            .bind(&original.select("dev").unwrap(), "api")
            .unwrap_err()
            .code,
        "ENV008"
    );
    assert_eq!(
        restored
            .bind(&restored.select("dev").unwrap(), "api")
            .unwrap()
            .import()
            .source()
            .bytes(),
        "synthetic descriptor evidence"
    );
}

#[test]
fn history_and_depth_budgets_reject_without_partial_state() {
    let p = base_and_dev();
    let mut r = Registry::new(Limits {
        revisions: 2,
        history_bytes: 1024 * 1024,
    });
    apply(&mut r, &p);
    let before = r.select("base").unwrap();
    assert_eq!(
        r.plan(&p, &evidence(&p, "changed")).unwrap_err().code,
        "ENV007"
    );
    assert_eq!(r.select("base").unwrap(), before);
    assert!(
        Registry::restore(
            r.image(),
            Limits {
                revisions: 1,
                history_bytes: usize::MAX
            }
        )
        .is_err()
    );
    let tiny = Registry::new(Limits {
        revisions: 100,
        history_bytes: 1,
    });
    assert_eq!(
        tiny.plan(&p, &evidence(&p, "a")).unwrap_err().code,
        "ENV007"
    );
    let chain = (0..33)
        .map(|i| {
            if i == 32 {
                format!("  e{i}: {{}}")
            } else {
                format!("  e{i}: {{extends: {{env: e{}, track: latest}}}}", i + 1)
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    let p = package(&chain);
    assert!(Registry::default().plan(&p, &evidence(&p, "")).is_err());
}

#[test]
fn removal_drops_active_visibility_but_retains_history_and_captured_bindings() {
    let mut r = Registry::default();
    apply(&mut r, &base_and_dev());
    let selected = r.select("dev").unwrap();
    let bound = r.bind(&selected, "api").unwrap();
    apply(&mut r, &package("  base: {}"));
    assert!(r.select("dev").is_err());
    assert!(r.bind(&selected, "api").is_err());
    assert!(r.retained_revision("dev", selected.revision()).is_some());
    assert_eq!(
        bound.import().source().bytes(),
        "synthetic descriptor evidence"
    );
}

#[test]
fn source_evidence_excess_and_missing_inputs_reject_without_publication() {
    let r = Registry::default();
    let p = base_and_dev();
    assert_eq!(
        r.plan(&p, &CapturedSources::default()).unwrap_err().code,
        "ENV006"
    );
    let mut sources = evidence(&p, "fixture");
    sources
        .insert(
            SourceKey::new("spec", "extra.json").unwrap(),
            CapturedSource::new("fixture/v1", "excess").unwrap(),
        )
        .unwrap();
    assert_eq!(r.plan(&p, &sources).unwrap_err().code, "ENV006");
}

#[test]
fn diagnostics_and_debug_do_not_include_engine_owned_config_payloads() {
    let marker = "synthetic-sensitive-payload";
    let p = package(&format!(
        "  base: {{parameters: {{x: {{type: Text, default: '{marker}'}}}}, imports: {{api: {}}}}}",
        import()
    ));
    let mut r = Registry::default();
    let plan = r.plan(&p, &evidence(&p, marker)).unwrap();
    assert!(!format!("{plan:?}").contains(marker));
    r.apply(plan).unwrap();
    let bound = r.bind(&r.select("base").unwrap(), "api").unwrap();
    assert!(!format!("{r:?} {bound:?} {:?}", r.image()).contains(marker));
}

#[test]
fn pinned_ancestor_cannot_introduce_a_historical_name_cycle() {
    let p = package("  a: {}\n  b: {extends: {env: a, track: latest}}");
    let mut r = Registry::default();
    apply(&mut r, &p);
    let b = r.inspect("b").unwrap().revision();
    let candidate = package(&format!(
        "  a: {{extends: {{env: b, revision: '{b}'}}}}\n  b: {{}}"
    ));
    assert_eq!(
        r.plan(&candidate, &evidence(&candidate, ""))
            .unwrap_err()
            .code,
        "ENV002"
    );
}

#[test]
fn an_identical_foreign_registry_cannot_accept_another_clients_selection() {
    let p = base_and_dev();
    let mut a = Registry::default();
    apply(&mut a, &p);
    let mut b = Registry::default();
    apply(&mut b, &p);
    assert_eq!(
        a.select("dev").unwrap().revision(),
        b.select("dev").unwrap().revision()
    );
    assert_eq!(
        b.bind(&a.select("dev").unwrap(), "api").unwrap_err().code,
        "ENV008"
    );
}

#[test]
fn source_capture_is_owned_and_later_input_changes_cannot_change_a_plan() {
    let p = base_and_dev();
    let mut r = Registry::default();
    let mut bytes = "captured-before-edit".to_owned();
    let sources = evidence(&p, &bytes);
    let plan = r.plan(&p, &sources).unwrap();
    bytes.clear();
    bytes.push_str("changed-after-plan");
    drop(sources);
    drop(p);
    r.apply(plan).unwrap();
    assert_eq!(
        r.inspect("dev").unwrap().imports()["api"].source().bytes(),
        "captured-before-edit"
    );
}

#[test]
fn a_pin_can_be_resolved_from_matching_evidence_in_a_fresh_registry() {
    let p = base_and_dev();
    let sources = evidence(&p, "portable bytes");
    let initial = Registry::default().plan(&p, &sources).unwrap();
    let pin = initial.inspect("base").unwrap().revision();
    let p = package(&format!(
        "  base: {{imports: {{api: {}}}}}\n  prod: {{extends: {{env: base, revision: '{pin}'}}}}",
        import()
    ));
    let fresh = Registry::default()
        .plan(&p, &evidence(&p, "portable bytes"))
        .unwrap();
    assert_eq!(
        fresh.inspect("prod").unwrap().parent().unwrap().revision(),
        pin
    );
    assert!(
        Registry::default()
            .plan(&p, &evidence(&p, "wrong bytes"))
            .is_err()
    );
}

#[test]
fn environment_changes_have_shared_human_format_without_debug_options() {
    use wes_engine::environments::Change;
    let revision = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
        .parse()
        .unwrap();
    let change = Change {
        name: "fixture".into(),
        before: None,
        after: Some(revision),
        added_imports: vec!["app".into(), "edge".into()],
        removed_imports: vec![],
        rebound_imports: vec![],
    };
    let text = change.to_string();
    assert!(text.contains("fixture: new (revision sha256:"));
    assert!(text.contains("imports added: app, edge"));
    assert!(!text.contains("Some("));
    assert!(!text.contains("None"));
    assert!(!text.contains("removed"));
}
