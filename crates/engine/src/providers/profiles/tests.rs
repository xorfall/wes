use super::*;
use crate::streams::{StreamFuture, StreamSink, StreamingInvoker};
use std::sync::atomic::{AtomicUsize, Ordering};
use wes_core::{
    capability::Parameter,
    contracts::ContractRegistry,
    environments::{CapturedSource, CapturedSources, Package},
};

fn int(n: i64) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Int),
        Data::Int(n),
        Default::default(),
    )
    .unwrap()
}
fn text(s: &str) -> Value {
    Value::new(
        Shape::Primitive(Primitive::Text),
        Data::Text(s.into()),
        Default::default(),
    )
    .unwrap()
}
fn declaration(name: &str, default: bool) -> (ProfileDeclaration, ControlContracts) {
    let registry = ContractRegistry::new();
    (
        ProfileDeclaration {
            name: name.into(),
            kind: "tcp".into(),
            default,
            stop: int(7),
            ready_wait: Duration::from_secs(5),
            finalization: FinalizationLimits {
                tail: Duration::from_secs(5),
                ..Default::default()
            },
            options: [
                (
                    "duration".into(),
                    ProfileOption {
                        parameter: "duration_ms".into(),
                        contract: registry.resolve("Int").unwrap(),
                        restriction: Restriction::Maximum(30000),
                    },
                ),
                (
                    "filter".into(),
                    ProfileOption {
                        parameter: "condition".into(),
                        contract: registry.resolve("Text").unwrap(),
                        restriction: Restriction::BackendCondition,
                    },
                ),
            ]
            .into(),
        },
        ControlContracts {
            input: registry.resolve("Int").unwrap(),
            output: registry.resolve("Bool").unwrap(),
        },
    )
}
struct Port {
    profile: Arc<SourceProfile>,
    entries: Arc<AtomicUsize>,
}
impl Invoker for Port {
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.entries.fetch_add(1, Ordering::SeqCst);
        panic!("lookup must not invoke business work")
    }
}
impl StreamingInvoker for Port {
    fn source_profile(&self, _: &Capability) -> Option<Arc<SourceProfile>> {
        Some(self.profile.clone())
    }
    fn control_contracts(&self, _: &Capability) -> Option<ControlContracts> {
        Some(self.profile.controls.clone())
    }
    fn records_bytes(&self, _: &Capability) -> bool {
        true
    }
    fn subscribe(&self, _: Call, _: StreamSink, _: CancellationToken) -> StreamFuture {
        self.entries.fetch_add(1, Ordering::SeqCst);
        panic!("lookup must not start capture")
    }
}
fn register(providers: &mut Providers, alias: &str, name: &str, default: bool) -> Arc<AtomicUsize> {
    let (declaration, controls) = declaration(name, default);
    let entries = Arc::new(AtomicUsize::new(0));
    let port = Arc::new(Port {
        profile: Arc::new(SourceProfile::new(declaration, controls).unwrap()),
        entries: entries.clone(),
    });
    let mut source = Capability::new(["start"], Shape::Primitive(Primitive::Int), Safety::Unsafe);
    source.streaming = true;
    source.parameters = vec![
        Parameter::new("duration_ms", Shape::Primitive(Primitive::Int), false),
        Parameter::new("condition", Shape::Primitive(Primitive::Text), false),
    ];
    let work = Capability::new(["work"], Shape::Primitive(Primitive::Int), Safety::Safe);
    providers.register_ports(
        ProviderDescription::new(alias, [source, work], vec![]).unwrap(),
        port.clone(),
        Some(port),
    );
    entries
}
fn business(providers: &Providers, alias: &str) -> BoundCall {
    let provider = providers.catalogue().provider(alias).unwrap().clone();
    let capability = provider.capability(&["work".into()]).unwrap().clone();
    providers
        .bind_finite(Invocation {
            provider,
            capability,
            inputs: Default::default(),
            cautions: Default::default(),
            interactive: false,
            guards: Default::default(),
        })
        .unwrap()
}
fn local() -> Providers {
    Providers::local(LocalScope::new("synthetic").unwrap())
}
fn scoped(providers: &mut Providers, alias: &str, target: &str, target_name: &str) {
    let package = Package::parse(&format!("version: 1\ntargets: {{{target_name}: {{{target}}}}}\nenvironments: {{lab: {{imports: {{{alias}: {{source: {{kind: spec, file: synthetic.json}}, bind: {{target: {target_name}}}}}}}}}}}")).unwrap();
    let mut sources = CapturedSources::default();
    for key in package.required_sources() {
        sources
            .insert(key, CapturedSource::new("synthetic/v1", "{}").unwrap())
            .unwrap();
    }
    let mut registry = crate::environments::Registry::default();
    registry
        .apply(registry.plan(&package, &sources).unwrap())
        .unwrap();
    let binding = registry.inspect("lab").unwrap().bind(alias).unwrap();
    let authority = crate::environments::Authority::default();
    authority.establish_namespace(&Uuid::new_v4().to_string());
    authority.enable("lab").unwrap();
    let original = providers.providers[alias].clone();
    providers.providers.insert(
        alias.into(),
        Arc::new(Provider {
            scope: ExecutionScope::Environment {
                binding,
                authority: Some(authority),
            },
            ..(*original).clone()
        }),
    );
}

#[test]
fn lookup_selects_explicit_defaults_and_freezes_typed_options_without_any_entry() {
    let mut providers = local();
    let entries = register(&mut providers, "observer", "lab-tcp", true);
    let call = business(&providers, "observer");
    let private = int(500).with_provenance(
        Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
    );
    let options = [
        ("duration".into(), private),
        ("filter".into(), text("tcp port 4242")),
    ]
    .into();
    let resolved = providers
        .resolve_source_profile(
            &ProfileSelector::Default("tcp".into()),
            Some(&call),
            &options,
            Some(Duration::from_secs(1)),
        )
        .unwrap();
    assert_eq!(resolved.name(), "lab-tcp");
    assert!(resolved.records_bytes());
    assert_eq!(
        resolved.source.invocation.inputs["duration_ms"].literal_values()[0].data(),
        &Data::Int(500)
    );
    assert!(resolved.source.captured_policy().is_private());
    assert!(resolved.source.dependencies().next().is_none());
    let task = resolved.into_companion(call).unwrap();
    assert!(
        !crate::tasks::BoundTask::Companion(Box::new(task))
            .traits()
            .repeatable
    );
    assert_eq!(entries.load(Ordering::SeqCst), 0);
}

#[test]
fn missing_opt_in_default_and_ambiguous_aliases_never_choose_an_arbitrary_source() {
    let mut providers = local();
    register(&mut providers, "one", "lab", false);
    let call = business(&providers, "one");
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Default("tcp".into()),
            Some(&call),
            &Default::default(),
            None
        ),
        Err(ProfileError::NotFound)
    ));
    assert!(
        providers
            .resolve_source_profile(
                &ProfileSelector::Named("lab".into()),
                None,
                &Default::default(),
                None
            )
            .is_ok()
    );
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Default("tcp".into()),
            None,
            &Default::default(),
            None
        ),
        Err(ProfileError::NotFound)
    ));
    register(&mut providers, "alias", "lab", false);
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Named("lab".into()),
            Some(&call),
            &Default::default(),
            None
        ),
        Err(ProfileError::Ambiguous)
    ));
    register(&mut providers, "default_one", "first-default", true);
    register(&mut providers, "default_two", "second-default", true);
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Default("tcp".into()),
            Some(&call),
            &Default::default(),
            None
        ),
        Err(ProfileError::Ambiguous)
    ));
}

#[test]
fn unknown_broadening_mistyped_and_oversized_options_refuse_in_native_and_direct_admission() {
    let mut providers = local();
    register(&mut providers, "observer", "lab", true);
    let call = business(&providers, "observer");
    let profile = providers.providers["observer"]
        .streams
        .as_ref()
        .unwrap()
        .source_profile(&call.invocation.capability)
        .unwrap();
    for (name, value) in [
        ("duration", int(0)),
        ("duration", int(-1)),
        ("duration", int(30001)),
        ("duration", text("not-numeric")),
        ("filter", text("")),
        ("filter", text("tcp\nport 42")),
        ("filter", text(&"x".repeat(4097))),
        ("interface", text("any")),
    ] {
        assert!(matches!(
            providers.resolve_source_profile(
                &ProfileSelector::Named("lab".into()),
                Some(&call),
                &[(name.into(), value.clone())].into(),
                None
            ),
            Err(ProfileError::InvalidOption)
        ));
        if name != "interface" {
            let parameter = &profile.options()[name].parameter;
            assert!(
                profile
                    .validate_arguments(&[(parameter.clone(), value)].into())
                    .is_err()
            );
        }
    }
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Named("lab".into()),
            Some(&call),
            &Default::default(),
            Some(Duration::from_secs(6))
        ),
        Err(ProfileError::InvalidOption)
    ));
}

#[test]
fn replacement_and_foreign_registry_refuse_but_already_resolved_source_keeps_original_definition() {
    let mut providers = local();
    register(&mut providers, "observer", "old", true);
    let call = business(&providers, "observer");
    let resolved = providers
        .resolve_source_profile(
            &ProfileSelector::Named("old".into()),
            Some(&call),
            &Default::default(),
            None,
        )
        .unwrap();
    register(&mut providers, "observer", "new", true);
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Named("new".into()),
            Some(&call),
            &Default::default(),
            None
        ),
        Err(ProfileError::Changed)
    ));
    assert_eq!(resolved.name(), "old");
    assert!(!Arc::ptr_eq(
        &resolved.source.provider,
        &providers.providers["observer"]
    ));
    let mut foreign = local();
    register(&mut foreign, "observer", "new", true);
    assert!(matches!(
        foreign.resolve_source_profile(
            &ProfileSelector::Named("new".into()),
            Some(&business(&providers, "observer")),
            &Default::default(),
            None
        ),
        Err(ProfileError::Changed)
    ));
}

#[test]
fn actual_local_and_remote_destinations_match_without_alias_dns_or_command_inference() {
    let ssh = "kind: ssh, host: synthetic.invalid, user: operator, port: 2222, client: /synthetic/ssh, identity_file: /synthetic/key, known_hosts: /synthetic/hosts, shell: posix, inherit: remote";
    let docker = "kind: docker, socket: /synthetic/docker.sock, container: synthetic-container, inherit: container";
    for target in ["kind: local", ssh, docker] {
        let mut providers = local();
        let a = register(&mut providers, "capture", "lab", true);
        let b = register(&mut providers, "business", "other", false);
        scoped(&mut providers, "capture", target, "capture_target");
        scoped(&mut providers, "business", target, "business_target");
        let call = business(&providers, "business");
        let resolved = providers
            .resolve_source_profile(
                &ProfileSelector::Default("tcp".into()),
                Some(&call),
                &Default::default(),
                None,
            )
            .unwrap();
        assert!(resolved.into_companion(call).is_ok());
        assert_eq!(a.load(Ordering::SeqCst) + b.load(Ordering::SeqCst), 0);
    }
}

#[test]
fn mismatched_remote_or_unbound_target_refuses_and_attachment_cannot_change_the_selected_target() {
    let mut providers = local();
    register(&mut providers, "capture", "lab", true);
    register(&mut providers, "business", "other", false);
    scoped(
        &mut providers,
        "business",
        "kind: docker, socket: /synthetic/docker.sock, container: synthetic-container, inherit: container",
        "remote",
    );
    let call = business(&providers, "business");
    assert!(matches!(
        providers.resolve_source_profile(
            &ProfileSelector::Named("lab".into()),
            Some(&call),
            &Default::default(),
            None
        ),
        Err(ProfileError::NotFound)
    ));
    let resolved = providers
        .resolve_source_profile(
            &ProfileSelector::Named("lab".into()),
            None,
            &Default::default(),
            None,
        )
        .unwrap();
    assert!(resolved.into_companion(call).is_err());
    let mut unbound = Providers::new();
    register(&mut unbound, "observer", "lab", true);
    assert!(matches!(
        unbound.resolve_source_profile(
            &ProfileSelector::Named("lab".into()),
            Some(&business(&unbound, "observer")),
            &Default::default(),
            None
        ),
        Err(ProfileError::Changed)
    ));
}

#[test]
fn invalid_declaration_bounds_stop_options_and_contracts_cannot_create_profile_metadata() {
    for case in 0..8 {
        let (mut declaration, controls) = declaration("lab", true);
        match case {
            0 => declaration.name = " ".into(),
            1 => declaration.kind = "tcp\n".into(),
            2 => declaration.ready_wait = Duration::ZERO,
            3 => declaration.finalization.control = Duration::from_secs(61),
            4 => declaration.stop = text("invalid-int"),
            5 => declaration.options["duration"].restriction = Restriction::Maximum(0),
            6 => declaration.options["duration"].restriction = Restriction::BackendCondition,
            7 => declaration.options["filter"].parameter = "duration_ms".into(),
            _ => unreachable!(),
        }
        assert!(
            SourceProfile::new(declaration, controls).is_err(),
            "case {case}"
        );
    }
}
