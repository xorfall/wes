use wes_core::environments::{
    CapturedSource, CapturedSources, ConfigType, ConfigValue, MAX_SOURCE_BYTES, Package, Revision,
    SourceKey,
};

#[test]
fn docker_destinations_are_exclusive_typed_and_part_of_revision_evidence() {
    use wes_core::environments::{DockerDestination, EffectiveEnvironment, TargetKind};
    let recipe = |fields: &str| {
        format!(
            "version: 1\ntargets: {{app: {{kind: docker, socket: /not/opened.sock, inherit: container, {fields}}}}}\nenvironments: {{qa: {{targets: [app]}}}}"
        )
    };
    for fields in [
        "",
        "container: app, compose: {project: shop, service: api}",
        "compose: {project: shop}",
        "compose: {project: shop, service: api, replica: 0}",
        "compose: {project: shop, service: api, replica: 1.5}",
        "compose: {project: shop, service: api, replica: 4294967296}",
        "compose: {project: 'shop*', service: api}",
        "compose: {project: shop, service: api, unknown: yes}",
        "container: app, shell: sh",
    ] {
        assert!(Package::parse(&recipe(fields)).is_err(), "{fields}");
    }
    let base = Package::parse(&recipe(
        "compose: {project: shop, service: api, replica: 2}, shell: /bin/bash",
    ))
    .unwrap();
    assert!(matches!(
        base.targets()["app"].kind(),
        TargetKind::Docker {
            destination: DockerDestination::Compose {
                replica: Some(2),
                ..
            },
            shell: Some(_),
            ..
        }
    ));
    let revision = |p: &Package| {
        EffectiveEnvironment::resolve(p, "qa", None, &CapturedSources::default())
            .unwrap()
            .revision()
    };
    for fields in [
        "compose: {project: other, service: api, replica: 2}, shell: /bin/bash",
        "compose: {project: shop, service: web, replica: 2}, shell: /bin/bash",
        "compose: {project: shop, service: api, replica: 1}, shell: /bin/bash",
        "compose: {project: shop, service: api}, shell: /bin/bash",
        "compose: {project: shop, service: api, replica: 2}",
        "container: api, shell: /bin/bash",
    ] {
        assert_ne!(
            revision(&base),
            revision(&Package::parse(&recipe(fields)).unwrap()),
            "{fields}"
        );
    }
}

#[test]
fn package_parses_typed_parameters_import_slots_and_local_targets_without_io() {
    let package = Package::parse(
        r#"
version: 1
targets: {local: {kind: local}}
environments:
  base:
    abstract: true
    parameters:
      endpoint: {type: Text}
      retries: {type: Int, default: 3}
      enabled: {type: Bool, default: false}
    secretSlots: {token: {required: true}}
    imports:
      api:
        source: {kind: spec, file: ./not-read.json}
        bind:
          target: local
          endpoint: {config: endpoint}
          credentials: {authorization: {secret: token}}
  dev:
    extends: {env: base, track: latest}
    config: {endpoint: 'https://dev.example.invalid'}
    secretRefs: {token: dev/api-token}
"#,
    )
    .unwrap();
    let base = &package.definitions()["base"];
    assert!(base.abstract_environment);
    assert_eq!(base.parameters["endpoint"].kind, ConfigType::Text);
    assert_eq!(
        base.parameters["retries"].default,
        Some(ConfigValue::Int(3))
    );
    assert_eq!(
        base.parameters["enabled"].default,
        Some(ConfigValue::Bool(false))
    );
    assert_eq!(base.imports["api"].source.location(), "./not-read.json");
    assert_eq!(base.imports["api"].credentials["authorization"], "token");
    assert_eq!(package.required_sources().len(), 1);
}

#[test]
fn strict_yaml_excludes_duplicate_keys_null_tags_anchors_merges_and_multiple_documents() {
    for source in [
        "version: 1\nversion: 1\nenvironments: {}",
        "version: 1\nenvironments: {dev: {}, dev: {}}",
        "version: 1\nenvironments: {dev: null}",
        "version: 1\nenvironments: {dev: Null}",
        "version: 1\nenvironments: {dev: !!map {}}",
        "version: 1\nenvironments: {dev: &unused {}}",
        "version: 1\nenvironments: {dev: {abstract: &unused true}}",
        "version: 1\nenvironments: {dev: &base {}, prod: *base}",
        "version: 1\nenvironments: {dev: {<<: {abstract: true}}}",
        "version: 1\nenvironments: {}\n---\nversion: 1\nenvironments: {}",
        "version: !!int 1\nenvironments: {}",
    ] {
        assert!(Package::parse(source).is_err(), "accepted {source}");
    }
}

#[test]
fn unknown_versions_fields_shapes_and_unsupported_transport_fail_closed() {
    for source in [
        "version: 2\nenvironments: {}",
        "version: '1'\nenvironments: {}",
        "version: 1\nenvironments: {}\nsecret: hidden",
        "version: 1\nenvironments: {dev: {unknown: true}}",
        "version: 1\nenvironments: {dev: {config: {nested: {field: value}}}}",
        "version: 1\nenvironments: {dev: {config: {array: [a, b]}}}",
        "version: 1\nenvironments: {dev: {config: {n: 1.5}}}",
        "version: 1\nenvironments: {dev: {config: {n: 9223372036854775808}}}",
        "version: 1\nenvironments: {dev: {parameters: {n: {type: Decimal}}}}",
        "version: 1\nenvironments: {dev: {parameters: {n: {type: Int, default: '1'}}}}",
        "version: 1\ntargets: {remote: {kind: ssh}}\nenvironments: {}",
        "version: 1\ntargets: {local: {kind: local, inherit: host}}\nenvironments: {}",
        "version: 1\nenvironments: {dev: {extends: {env: base}}}",
        "version: 1\nenvironments: {dev: {extends: {env: base, track: newest}}}",
        "version: 1\nenvironments: {dev: {extends: base}}",
        "version: 1\nenvironments: {dev: {secretRefs: {token: ' '}}}",
    ] {
        assert!(Package::parse(source).is_err(), "accepted {source}");
    }
}

#[test]
fn import_schema_disallows_literal_credentials_ambiguous_sources_and_untyped_interpolation() {
    for import in [
        "source: {kind: spec, file: x, bin: y}, bind: {target: local}",
        "source: {kind: unknown, file: x}, bind: {target: local}",
        "source: {kind: spec, file: x}, bind: {target: local, credentials: {token: literal}}",
        "source: {kind: spec, file: x}, bind: {target: local, credentials: {token: {value: literal}}}",
        "source: {kind: process, bin: x}, bind: {target: local, endpoint: 'https://example.invalid'}",
        "source: {kind: spec, file: x}, bind: {target: local, endpoint: {config: endpoint, value: literal}}",
        "source: {kind: spec, file: x}, bind: {target: local, endpoint: {shell: whoami}}",
    ] {
        assert!(
            Package::parse(&format!(
                "version: 1\nenvironments: {{dev: {{imports: {{api: {{{import}}}}}}}}}"
            ))
            .is_err()
        );
    }
}

#[test]
fn identifiers_are_case_sensitive_bounded_and_not_silently_normalized() {
    let p = Package::parse("version: 1\nenvironments: {Dev: {}, dev: {}, team-a.dev: {}}").unwrap();
    assert_eq!(p.definitions().len(), 3);
    for name in ["-dev", "a/b", "a b", "_dev", "", "çevre"] {
        assert!(Package::parse(&format!("version: 1\nenvironments: {{'{name}': {{}}}}")).is_err());
    }
    let long = "a".repeat(129);
    assert!(Package::parse(&format!("version: 1\nenvironments: {{{long}: {{}}}}")).is_err());
}

#[test]
fn source_and_declaration_budgets_fail_before_unbounded_retention() {
    assert!(Package::parse(&" ".repeat(1_048_577)).is_err());
    let envs = (0..129)
        .map(|i| format!("e{i}: {{}}"))
        .collect::<Vec<_>>()
        .join(",");
    assert_eq!(
        Package::parse(&format!("version: 1\nenvironments: {{{envs}}}"))
            .unwrap_err()
            .code,
        "ENV007"
    );
    let text = "x".repeat(65_537);
    assert_eq!(
        Package::parse(&format!(
            "version: 1\nenvironments: {{dev: {{config: {{x: '{text}'}}}}}}"
        ))
        .unwrap_err()
        .code,
        "ENV007"
    );
    assert!(CapturedSource::new("spec/json/v1", &"x".repeat(MAX_SOURCE_BYTES + 1)).is_err());
}

#[test]
fn revisions_require_exact_lowercase_sha256_syntax() {
    let valid = format!("sha256:{}", "a0".repeat(32));
    assert_eq!(valid.parse::<Revision>().unwrap().to_string(), valid);
    for invalid in [
        "",
        "latest",
        "sha256:abc",
        &valid.to_uppercase(),
        &format!("sha256:{}", "é".repeat(64)),
    ] {
        assert!(invalid.parse::<Revision>().is_err());
    }
}

#[test]
fn evidence_must_be_complete_exact_and_immutable_for_each_source() {
    let p = Package::parse("version: 1\nenvironments: {base: {abstract: true, imports: {api: {source: {kind: spec, file: x}, bind: {target: local}}}}}").unwrap();
    let mut sources = CapturedSources::default();
    assert!(sources.validate(&p).is_err());
    let key = SourceKey::new("spec", "x").unwrap();
    sources
        .insert(
            key.clone(),
            CapturedSource::new("spec/json/v1", "first").unwrap(),
        )
        .unwrap();
    let bytes = sources.charge();
    sources
        .insert(
            key.clone(),
            CapturedSource::new("spec/json/v1", "first").unwrap(),
        )
        .unwrap();
    assert_eq!(sources.charge(), bytes);
    assert!(
        sources
            .insert(
                key.clone(),
                CapturedSource::new("spec/json/v1", "changed").unwrap()
            )
            .is_err()
    );
    assert_eq!(sources.get(&key).unwrap().bytes(), "first");
    sources.validate(&p).unwrap();
    sources
        .insert(
            SourceKey::new("spec", "extra").unwrap(),
            CapturedSource::new("spec/json/v1", "unrelated").unwrap(),
        )
        .unwrap();
    assert!(sources.validate(&p).is_err());
}

#[test]
fn debug_and_parse_errors_do_not_echo_configuration_or_descriptor_payloads() {
    let marker = "synthetic-sensitive-payload";
    let p = Package::parse(&format!(
        "version: 1\nenvironments: {{base: {{abstract: true, config: {{value: '{marker}'}}}}}}"
    ))
    .unwrap();
    assert!(!format!("{p:?}").contains(marker));
    assert!(!format!("{:?}", p.definitions()["base"]).contains(marker));
    let captured = CapturedSource::new("spec/json/v1", marker).unwrap();
    assert!(!format!("{captured:?}").contains(marker));
    let e = Package::parse(&format!("version: 1\nenvironments: {{base: [{marker}")).unwrap_err();
    assert!(!format!("{e:?} {e}").contains(marker));
}

#[test]
fn aggregate_evidence_budget_is_checked_before_insertion() {
    let mut sources = CapturedSources::default();
    let bytes = "x".repeat(MAX_SOURCE_BYTES);
    for i in 0..40 {
        let before = sources.charge();
        let key = SourceKey::new("spec", &format!("{i}.json")).unwrap();
        if sources
            .insert(
                key.clone(),
                CapturedSource::new("spec/json/v1", &bytes).unwrap(),
            )
            .is_err()
        {
            assert_eq!(sources.charge(), before);
            assert!(sources.get(&key).is_none());
            assert_eq!(i, 31);
            return;
        }
    }
    panic!("aggregate budget did not reject");
}

#[test]
fn source_digest_pins_exact_descriptor_bytes_and_is_not_an_api_version() {
    use sha2::{Digest, Sha256};
    let bytes = "{\"version\":2}";
    let digest = format!("{:x}", Sha256::digest(bytes.as_bytes()));
    let yaml = format!(
        "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments:\n  dev:\n    imports:\n      api:\n        source: {{kind: spec, file: api.json, sha256: '{digest}'}}\n        bind: {{target: local, endpoint: 'http://127.0.0.1:1'}}\n"
    );
    let package = Package::parse(&yaml).unwrap();
    assert_eq!(
        package.definitions()["dev"].imports["api"]
            .source_sha256
            .as_deref(),
        Some(digest.as_str())
    );
    assert!(Package::parse(&yaml.replace(&digest, "invalid")).is_err());
    assert!(
        Package::parse(&yaml.replace(
            "kind: spec, file: api.json",
            "kind: process, bin: /bin/echo"
        ))
        .is_err()
    );
}
#[test]
fn spec_sources_require_one_matching_file_or_http_url_field() {
    for (source, valid) in [
        (
            "{kind: spec, url: 'https://example.invalid/api.json?version=2'}",
            true,
        ),
        ("{kind: spec, file: api.json}", true),
        (
            "{kind: spec, url: 'https://example.invalid/api.json', file: api.json}",
            false,
        ),
        ("{kind: spec}", false),
        ("{kind: spec, url: api.json}", false),
        ("{kind: spec, url: 'file:///tmp/api.json'}", false),
        ("{kind: process, url: 'https://example.invalid/bin'}", false),
    ] {
        let yaml = format!(
            "version: 1\ntargets: {{local: {{kind: local}}}}\nenvironments: {{dev: {{imports: {{api: {{source: {source}, bind: {{target: local}}}}}}}}}}\n"
        );
        assert_eq!(Package::parse(&yaml).is_ok(), valid, "{source}");
    }
}

#[test]
fn independent_targets_are_captured_inherited_bounded_and_revision_sensitive() {
    use std::sync::Arc;
    use wes_core::environments::EffectiveEnvironment;
    let yaml = "version: 1\ntargets: {host: {kind: local, cwd: /one}}\nenvironments: {base: {targets: [host]}, child: {extends: {env: base, track: latest}}}";
    let package = Package::parse(yaml).unwrap();
    assert!(package.required_sources().is_empty());
    let sources = CapturedSources::default();
    let base = Arc::new(EffectiveEnvironment::resolve(&package, "base", None, &sources).unwrap());
    let child =
        EffectiveEnvironment::resolve(&package, "child", Some(base.clone()), &sources).unwrap();
    assert_eq!(
        child.execution_targets()["host"].as_ref().unwrap().cwd(),
        Some("/one")
    );
    let changed = Package::parse(&yaml.replace("/one", "/two")).unwrap();
    let changed = EffectiveEnvironment::resolve(&changed, "base", None, &sources).unwrap();
    assert_ne!(base.revision(), changed.revision());
    assert!(!base.same_execution(&changed));
    let missing = Package::parse(&yaml.replace("targets: [host]", "targets: [absent]")).unwrap();
    assert!(
        EffectiveEnvironment::resolve(&missing, "base", None, &sources)
            .unwrap_err()
            .message
            .contains("undefined execution target")
    );
    assert!(Package::parse(&yaml.replace("targets: [host]", "targets: [host, host]")).is_err());
    // Empty optional membership does not invalidate existing revision evidence.
    let empty = Package::parse("version: 1\nenvironments: {plain: {}}").unwrap();
    let explicit = Package::parse("version: 1\nenvironments: {plain: {targets: []}}").unwrap();
    assert_eq!(
        EffectiveEnvironment::resolve(&empty, "plain", None, &sources)
            .unwrap()
            .revision(),
        EffectiveEnvironment::resolve(&explicit, "plain", None, &sources)
            .unwrap()
            .revision()
    );
}

#[test]
fn terminal_target_names_never_choose_between_different_captured_destinations() {
    use std::sync::Arc;
    use wes_core::environments::EffectiveEnvironment;
    let yaml = "version: 1\ntargets: {host: {kind: local, cwd: /old}}\nenvironments: {base: {imports: {api: {source: {kind: spec, file: x}, bind: {target: host}}}}, child: {extends: {env: base, track: latest}, targets: [host]}}";
    let package = Package::parse(yaml).unwrap();
    let mut sources = CapturedSources::default();
    sources
        .insert(
            SourceKey::new("spec", "x").unwrap(),
            CapturedSource::new("spec/json/v1", "{}").unwrap(),
        )
        .unwrap();
    let parent = Arc::new(EffectiveEnvironment::resolve(&package, "base", None, &sources).unwrap());
    let unchanged =
        EffectiveEnvironment::resolve(&package, "child", Some(parent.clone()), &sources).unwrap();
    assert_eq!(unchanged.execution_targets().len(), 1);
    assert!(unchanged.execution_targets()["host"].is_ok());
    let newer = Package::parse(&yaml.replace("/old", "/new")).unwrap();
    let changed = EffectiveEnvironment::resolve(&newer, "child", Some(parent), &sources).unwrap();
    assert!(changed.execution_targets()["host"].is_err());
    // Inherited explicit attachments also refuse silent replacement during resolution.
    let attachments = yaml.replace(
        "imports: {api: {source: {kind: spec, file: x}, bind: {target: host}}}",
        "targets: [host]",
    );
    let original = Package::parse(&attachments).unwrap();
    let parent =
        Arc::new(EffectiveEnvironment::resolve(&original, "base", None, &sources).unwrap());
    let newer = Package::parse(&attachments.replace("/old", "/new")).unwrap();
    assert!(EffectiveEnvironment::resolve(&newer, "child", Some(parent), &sources).is_err());
}

#[test]
fn http_transport_is_typed_and_changes_captured_binding_revision() {
    use wes_core::environments::EffectiveEnvironment;
    let yaml = "version: 1\ntargets: {local: {kind: local}}\nenvironments: {qa: {imports: {http: {source: {kind: builtin, name: http}, bind: {target: local, transport: internal}}}}}";
    let package = Package::parse(yaml).unwrap();
    let mut sources = CapturedSources::default();
    for key in package.required_sources() {
        sources
            .insert(key, CapturedSource::new("builtin/v1", "http").unwrap())
            .unwrap();
    }
    let first = EffectiveEnvironment::resolve(&package, "qa", None, &sources).unwrap();
    let second = EffectiveEnvironment::resolve(
        &Package::parse(&yaml.replace("transport: internal", "transport: curl")).unwrap(),
        "qa",
        None,
        &sources,
    )
    .unwrap();
    assert_ne!(first.revision(), second.revision());
    assert!(!first.same_execution(&second));
    for bad in [
        yaml.replace("transport: internal", "transport: native"),
        yaml.replace("name: http", "name: sh"),
        yaml.replace("name: http", "name: docker"),
    ] {
        assert!(Package::parse(&bad).is_err());
    }
}

#[test]
fn auth_choices_are_canonical_revisioned_environment_data() {
    use wes_core::environments::EffectiveEnvironment;
    let base = include_str!("../../../examples/http-auth-alternatives/environments.yaml");
    let package = Package::parse(base).unwrap();
    let source = SourceKey::new("spec", "service.json").unwrap();
    let mut sources = CapturedSources::default();
    sources
        .insert(
            source,
            CapturedSource::new(
                "spec/json/v1",
                include_str!("../../../examples/http-auth-alternatives/service.json"),
            )
            .unwrap(),
        )
        .unwrap();
    let resolve = |text: &str| {
        EffectiveEnvironment::resolve(&Package::parse(text).unwrap(), "keypair", None, &sources)
            .unwrap()
    };
    let a = resolve(base);
    let b = resolve(&base.replace("[apiKey, apiSecret]", "[apiSecret, apiKey]"));
    assert_eq!(a.revision(), b.revision());
    assert_eq!(
        package.definitions()["keypair"].imports["demo"].auth["listItems"],
        vec!["apiKey", "apiSecret"]
    );
    let changed = resolve(&base.replace("[apiKey, apiSecret]", "[basic]"));
    assert_ne!(a.revision(), changed.revision());
    assert!(!a.same_execution(&changed));
    for bad in ["[apiKey, apiKey]", "apiKey", "[null]"] {
        assert!(
            Package::parse(&base.replace("[apiKey, apiSecret]", bad)).is_err(),
            "{bad}"
        );
    }
    assert!(Package::parse(&base.replace("auth: {listItems:", "auth: {'list items':")).is_ok());
}

#[test]
fn output_confidentiality_is_validated_and_part_of_captured_revision() {
    use wes_core::flow::OutputPolicy;
    let parse = |output: &str| {
        Package::parse(&"version: 1\ntargets: {local: {kind: local}}\nenvironments: {qa: {imports: {echo: {source: {kind: process, bin: fixture}, bind: {target: local, output: OUTPUT}}}}}".replace("OUTPUT",output))
    };
    let mut revisions = std::collections::BTreeSet::new();
    for (text, policy) in [
        ("public", OutputPolicy::Public),
        ("private", OutputPolicy::Private),
        (
            "confidential-temporary",
            OutputPolicy::ConfidentialTemporary,
        ),
        ("confidential", OutputPolicy::Confidential),
    ] {
        let package = parse(text).unwrap();
        assert_eq!(
            package.definitions()["qa"].imports["echo"].output_policy,
            policy
        );
        let mut sources = CapturedSources::default();
        for source in package.required_sources() {
            sources
                .insert(
                    source,
                    CapturedSource::new("process/v1", "fixture").unwrap(),
                )
                .unwrap();
        }
        revisions.insert(
            wes_core::environments::EffectiveEnvironment::resolve(&package, "qa", None, &sources)
                .unwrap()
                .revision(),
        );
    }
    assert_eq!(revisions.len(), 4);
    assert!(parse("confidentail").is_err());
}
