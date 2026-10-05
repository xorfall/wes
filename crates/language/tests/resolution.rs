use wes_core::{
    Shape,
    capability::{Capability, Catalogue, ProviderDescription, Safety, Sort},
};
use wes_language::{
    Expression, SourceText, parse,
    resolve::{Resolution, resolve},
    vocabulary::{MetaCommand, excused},
};

fn command(input: &str) -> wes_language::Call {
    let parsed = parse(&SourceText::new("test", input));
    assert!(parsed.diagnostics.is_empty());
    let Expression::Call(call) = parsed
        .script
        .statements
        .into_iter()
        .next()
        .unwrap()
        .expression
    else {
        panic!("call")
    };
    call
}
fn catalogue(name: &str) -> Catalogue {
    let mut catalogue = Catalogue::new();
    catalogue.register(
        ProviderDescription::new(
            name,
            [Capability::new(
                ["items", "get"],
                Shape::Unknown,
                Safety::Safe,
            )],
            vec![],
        )
        .unwrap(),
    );
    catalogue
}

#[test]
fn explicit_marker_never_falls_back_to_provider_and_collisions_need_a_choice() {
    let catalogue = catalogue("list");
    let error = resolve(&command("list items get"), &catalogue).unwrap_err();
    assert_eq!(error.code, "RES001");
    assert_eq!(error.hints.len(), 2);
    assert!(matches!(
        resolve(&command(":list capabilities"), &catalogue).unwrap(),
        Resolution::Meta { .. }
    ));
    assert_eq!(
        resolve(&command(":catalog items get"), &catalogue)
            .unwrap_err()
            .code,
        "RES002"
    );
    assert!(matches!(
        resolve(&command("renamed items get"), &self::catalogue("renamed")).unwrap(),
        Resolution::Capability { .. }
    ));
}

#[test]
fn unknown_and_reserved_names_have_distinct_errors_and_suggestions() {
    let catalogue = catalogue("catalog");
    for (input, code) in [
        (":if", "RES002"),
        ("absent", "RES004"),
        ("catalog missing", "RES005"),
        (":missing", "RES002"),
    ] {
        assert_eq!(resolve(&command(input), &catalogue).unwrap_err().code, code);
    }
    assert!(resolve(&command(":read trace:$node"), &catalogue).is_ok());
    assert!(!MetaCommand::Trace.spec(&[]).reserved);
    let typo = resolve(&command(":inspct"), &catalogue).unwrap_err();
    assert!(typo.hints.iter().any(|hint| hint.contains("inspect")));
    let typo = resolve(&command("catalog item get"), &catalogue).unwrap_err();
    assert!(typo.hints.iter().any(|hint| hint.contains("items get")));
    assert!(matches!(
        resolve(&command("trace items get"), &self::catalogue("trace")).unwrap(),
        Resolution::Capability { .. }
    ));
}

#[test]
fn command_metadata_carries_dynamic_registry_and_lifecycle_semantics() {
    let import = MetaCommand::Import.spec(&[]);
    assert_eq!(import.tail_registry, Some("importer"));
    assert_eq!(
        import.parameter("as").unwrap().sort,
        Sort::Fresh("provider".into())
    );
    assert!(MetaCommand::Change.spec(&[]).rewrites_target);
    assert!(!MetaCommand::List.spec(&[]).derived);
    for command in [
        MetaCommand::Wait,
        MetaCommand::Refresh,
        MetaCommand::Cancel,
        MetaCommand::Save,
        MetaCommand::Load,
    ] {
        assert!(!command.spec(&[]).recorded);
    }
    assert!(MetaCommand::Timeout.spec(&[]).recorded);
    assert_eq!(
        MetaCommand::Help.spec(&[]).tail_words,
        wes_language::vocabulary::commands::roots()
    );
}

#[test]
fn type_subcommands_use_distinct_signatures_without_changing_grammar() {
    let catalogue = Catalogue::new();
    let Resolution::Meta { spec, tail, .. } =
        resolve(&command(":package load path:types.yaml"), &catalogue).unwrap()
    else {
        panic!("meta")
    };
    assert_eq!(tail, ["load"]);
    assert!(!spec.parameter("path").unwrap().required);
    assert!(!spec.parameter("source").unwrap().required);
    assert!(spec.parameter("origin").is_some());
    assert!(!spec.produces_value);
    assert!(!spec.open_arguments);
    let Resolution::Meta { spec, .. } =
        resolve(&command(":type check $items as:\"List<Int>\""), &catalogue).unwrap()
    else {
        panic!("meta")
    };
    assert!(spec.produces_value && spec.derived);
    assert!(spec.path_tail.accepts(2));
    assert!(!spec.path_tail.accepts(3));
    assert!(spec.parameter("value").unwrap().required);
}

#[test]
fn only_unchecked_annotations_decode_as_excused_arguments() {
    assert_eq!(
        excused(["interactive:input", "unchecked:value", "provider:source"])
            .into_iter()
            .collect::<Vec<_>>(),
        ["value"]
    );
}

#[test]
fn resolving_retains_metadata_snapshot_across_provider_replacement() {
    let mut catalogue = catalogue("catalog");
    let Resolution::Capability { capability, .. } =
        resolve(&command("catalog items get"), &catalogue).unwrap()
    else {
        panic!("capability")
    };
    catalogue.register(ProviderDescription::new("catalog", [], vec![]).unwrap());
    assert_eq!(capability.path, ["items", "get"]);
    assert_eq!(
        resolve(&command("catalog items get"), &catalogue)
            .unwrap_err()
            .code,
        "RES005"
    );
}

#[test]
fn help_resolves_provider_paths_from_the_supplied_catalogue_only() {
    let catalogue = catalogue("catalog");
    for source in [
        ":help catalog",
        ":help catalog items get",
        "help catalog items get",
    ] {
        let Resolution::Meta { spec, tail, .. } = resolve(&command(source), &catalogue).unwrap()
        else {
            panic!()
        };
        assert_eq!(spec.command, MetaCommand::Help);
        assert!(spec.path_tail.accepts(tail.len()));
        assert!(spec.tail_words.is_empty());
    }
    let missing = resolve(&command(":help catalog item get"), &catalogue).unwrap_err();
    assert_eq!(missing.code, "RES005");
    assert!(
        missing
            .hints
            .iter()
            .any(|hint| hint.contains(":help provider:catalog"))
    );
    assert_eq!(
        resolve(&command(":help catalog"), &Catalogue::new())
            .unwrap_err()
            .code,
        "RES004"
    );
    assert_eq!(
        resolve(&command(":help import"), &self::catalogue("import"))
            .unwrap_err()
            .code,
        "RES001"
    );
    assert!(resolve(&command(":help command:import"), &self::catalogue("import")).is_ok());
}

#[test]
fn unresolved_bare_inspection_suggests_reference_without_changing_resolution() {
    let catalogue = catalogue("catalog");
    let error = resolve(&command(":inspect total"), &catalogue).unwrap_err();
    assert_eq!(error.code, "RES004");
    assert!(error.hints.iter().any(|h| h.contains(":inspect $total")));
    for source in [
        ":inspect $total",
        ":inspect catalog",
        ":inspect list",
        ":inspect name:total",
    ] {
        assert!(resolve(&command(source), &catalogue).is_ok(), "{source}");
    }
    for source in [
        ":help total",
        ":inspect provider:total",
        ":inspect total missing",
    ] {
        let error = resolve(&command(source), &catalogue).unwrap_err();
        assert!(
            !error.hints.iter().any(|h| h.contains("$total")),
            "{source}"
        );
    }
}
