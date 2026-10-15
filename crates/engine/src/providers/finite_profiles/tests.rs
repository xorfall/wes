use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use wes_core::{Data, Primitive, capability::Parameter, contracts::ContractRegistry};

struct Port {
    profile: Arc<FiniteProfile>,
    entries: Arc<AtomicUsize>,
}
impl Invoker for Port {
    fn finite_profile(&self, _: &Capability) -> Option<Arc<FiniteProfile>> {
        Some(self.profile.clone())
    }
    fn invoke(&self, _: Call, _: CancellationToken) -> InvocationFuture {
        self.entries.fetch_add(1, Ordering::SeqCst);
        panic!("profile lookup must not enter the executable")
    }
}
fn value(data: Data) -> Value {
    Value::new(Shape::Unknown, data, Provenance::default()).unwrap()
}
fn register(providers: &mut Providers, alias: &str, kind: &str) -> Arc<AtomicUsize> {
    let mut registry = ContractRegistry::new();
    registry
        .load("version: 1\ntypes:\n  Count: {base: Int, min: 1, max: 3}\n")
        .unwrap();
    let contract = registry.resolve("Count").unwrap();
    let profile = FiniteProfile::new(
        "bounded".into(),
        kind.into(),
        [(
            "items".into(),
            Argument {
                parameter: "count".into(),
                contract: contract.clone(),
                required: true,
            },
        )]
        .into(),
    )
    .unwrap();
    let mut capability = Capability::new(
        ["execute"],
        Shape::Primitive(Primitive::Int),
        Safety::Unsafe,
    );
    capability.parameters = vec![Parameter::new("count", contract.shape(), true)];
    let entries = Arc::new(AtomicUsize::new(0));
    providers.register(
        ProviderDescription::new(alias, [capability], vec![]).unwrap(),
        Arc::new(Port {
            profile: Arc::new(profile),
            entries: entries.clone(),
        }),
    );
    entries
}
fn inputs(n: i64) -> IndexMap<String, Input> {
    [("items".into(), Input::Literal(value(Data::Int(n))))].into()
}

#[test]
fn finite_lookup_is_pure_typed_alias_independent_and_preserves_frozen_calls() {
    let mut providers = Providers::local(LocalScope::new("finite-fixture").unwrap());
    let entries = register(&mut providers, "arbitrary_alias", "sample.decode");
    let bound = providers
        .resolve_finite_profile("bounded", "sample.decode", &inputs(2))
        .unwrap();
    assert_eq!(bound.invocation().provider.name(), "arbitrary_alias");
    assert!(
        matches!(&bound.invocation().inputs["count"], Input::Literal(v) if v.data()==&Data::Int(2))
    );
    assert!(bound.is_local_target());
    assert_eq!(bound.invocation().guards["count"][0].name(), "Count");
    let old = bound.invocation().provider.clone();
    providers.unregister("arbitrary_alias");
    register(&mut providers, "another_alias", "sample.other");
    assert!(matches!(
        providers.resolve_finite_profile("bounded", "sample.decode", &inputs(2)),
        Err(ProfileError::NotFound)
    ));
    assert!(Arc::ptr_eq(&old, &bound.invocation().provider));
    assert_eq!(entries.load(Ordering::SeqCst), 0);
}
#[test]
fn finite_lookup_refuses_missing_unknown_invalid_ambiguous_and_unbound_without_entry() {
    let mut providers = Providers::local(LocalScope::new("finite-fixture").unwrap());
    let entries = register(&mut providers, "a", "sample.decode");
    for data in [Data::Int(0), Data::Int(4), Data::Text("two".into())] {
        assert!(matches!(
            providers.resolve_finite_profile(
                "bounded",
                "sample.decode",
                &[("items".into(), Input::Literal(value(data)))].into()
            ),
            Err(ProfileError::InvalidArgument)
        ));
    }
    for args in [
        IndexMap::new(),
        [("unknown".into(), Input::Literal(value(Data::Int(2))))].into(),
    ] {
        assert!(matches!(
            providers.resolve_finite_profile("bounded", "sample.decode", &args),
            Err(ProfileError::InvalidArgument)
        ));
    }
    register(&mut providers, "b", "sample.decode");
    assert!(matches!(
        providers.resolve_finite_profile("bounded", "sample.decode", &inputs(2)),
        Err(ProfileError::Ambiguous)
    ));
    let mut unbound = Providers::default();
    register(&mut unbound, "a", "sample.decode");
    assert!(matches!(
        unbound.resolve_finite_profile("bounded", "sample.decode", &inputs(2)),
        Err(ProfileError::NotFound)
    ));
    assert_eq!(entries.load(Ordering::SeqCst), 0);
}
#[test]
fn finite_metadata_requires_complete_one_to_one_contracts_and_bounded_identifiers() {
    let contract = ContractRegistry::new().resolve("Int").unwrap();
    let arg = Argument {
        parameter: "n".into(),
        contract,
        required: true,
    };
    assert!(FiniteProfile::new("bad name".into(), "sample".into(), IndexMap::new()).is_err());
    assert!(FiniteProfile::new("name".into(), "x".repeat(65), IndexMap::new()).is_err());
    assert!(
        FiniteProfile::new(
            "name".into(),
            "sample".into(),
            [("a".into(), arg.clone()), ("b".into(), arg)].into()
        )
        .is_err()
    );
    let mut providers = Providers::local(LocalScope::new("finite-fixture").unwrap());
    register(&mut providers, "a", "sample.decode");
    let original = providers.providers["a"].clone();
    let bad = Capability::new(["execute"], Shape::Unknown, Safety::Unsafe);
    let description = Arc::new(ProviderDescription::new("a", [bad], vec![]).unwrap());
    providers.providers.insert(
        "a".into(),
        Arc::new(Provider {
            description,
            ..(*original).clone()
        }),
    );
    assert!(matches!(
        providers.resolve_finite_profile("bounded", "sample.decode", &inputs(2)),
        Err(ProfileError::InvalidDeclaration)
    ));
}

#[test]
fn finite_target_location_uses_captured_transport_not_alias_or_directory() {
    use wes_core::environments::{CapturedSource, CapturedSources, Package};
    for (target, local) in [
        ("kind: local, cwd: /synthetic", true),
        (
            "kind: ssh, host: 192.0.2.18, user: fixture, port: 2222, client: /synthetic/ssh, identity_file: /synthetic/key, known_hosts: /synthetic/hosts, shell: posix, inherit: remote",
            false,
        ),
        (
            "kind: docker, socket: /synthetic/docker.sock, container: fixture, inherit: container",
            false,
        ),
    ] {
        let mut providers = Providers::local(LocalScope::new("target-fixture").unwrap());
        let entries = register(&mut providers, "local_alias", "sample.decode");
        let package = Package::parse(&format!("version: 1\ntargets:\n  host: {{{target}}}\nenvironments:\n  lab:\n    imports:\n      local_alias:\n        source: {{kind: spec, file: synthetic.json}}\n        bind: {{target: host}}\n")).unwrap();
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
        let binding = registry
            .inspect("lab")
            .unwrap()
            .bind("local_alias")
            .unwrap();
        let original = providers.providers["local_alias"].clone();
        providers.providers.insert(
            "local_alias".into(),
            Arc::new(Provider {
                scope: ExecutionScope::Environment {
                    binding,
                    authority: None,
                },
                ..(*original).clone()
            }),
        );
        let call = providers
            .resolve_finite_profile("bounded", "sample.decode", &inputs(2))
            .unwrap();
        assert_eq!(call.is_local_target(), local);
        assert_eq!(entries.load(Ordering::SeqCst), 0);
    }
}
