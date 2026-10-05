use wes_core::{
    Shape,
    capability::{Capability, Catalogue, ProviderDescription, Safety},
};
use wes_language::{
    Call, Expression, SourceText, Value, parse,
    resolve::resolve,
    targets::{self, QueryTarget},
    vocabulary::commands,
};
fn call(source: &str) -> Call {
    let parsed = parse(&SourceText::new("fixture", source));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    let Expression::Call(call) = &parsed.script.statements[0].expression else {
        panic!("call")
    };
    call.clone()
}
fn catalogue() -> Catalogue {
    let mut catalogue = Catalogue::default();
    for name in ["docker", "node"] {
        catalogue.register(
            ProviderDescription::new(
                name,
                [Capability::new(
                    ["container", "inspect"],
                    Shape::Unknown,
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
        );
    }
    catalogue
}
#[test]
fn canonical_and_registered_short_forms_have_one_operation_and_identical_arguments() {
    for (short, canonical) in [
        (":refresh $x", ":node refresh $x"),
        (":change $x limit:20", ":node change $x limit:20"),
        (":cancel $x", ":node cancel $x"),
        (":policy $x mode:manual", ":node policy $x mode:manual"),
        (":timeout $x after:PT10S", ":node timeout $x after:PT10S"),
        (
            ":remove $x scope:downstream",
            ":node remove $x scope:downstream",
        ),
    ] {
        let a = commands::invocation(&call(short)).unwrap();
        let b = commands::invocation(&call(canonical)).unwrap();
        assert_eq!(a.spec.command, b.spec.command);
        assert_eq!(a.spec.canonical, b.spec.canonical);
        assert_eq!(a.call.to_string(), b.call.to_string());
        assert_eq!(
            a.call.operands[0].name().span,
            call(short).operands[0].name().span
        );
    }
    for source in [
        ":policy mode:manual",
        ":node remove $x",
        ":drop $x",
        ":save old",
        ":load old",
        ":view $x",
        ":type load path:x",
    ] {
        assert!(commands::invocation(&call(source)).is_err(), "{source}");
    }
}
#[test]
fn help_and_inspect_share_bare_root_resolution_and_explicit_collision_diagnostics() {
    let catalogue = catalogue();
    for verb in ["help", "inspect"] {
        let help = verb == "help";
        assert_eq!(
            targets::resolve(&call(&format!(":{verb} node")), help, &catalogue)
                .unwrap_err()
                .code,
            "RES001"
        );
        assert!(matches!(
            targets::resolve(&call(&format!(":{verb} command:node")), help, &catalogue).unwrap(),
            QueryTarget::Command(_)
        ));
        assert!(matches!(
            targets::resolve(&call(&format!(":{verb} provider:node")), help, &catalogue).unwrap(),
            QueryTarget::Provider { .. }
        ));
        for path in ["docker", "docker container", "docker container inspect"] {
            assert!(matches!(
                targets::resolve(&call(&format!(":{verb} {path}")), help, &catalogue).unwrap(),
                QueryTarget::Provider { .. }
            ));
        }
        assert!(
            targets::resolve(
                &call(&format!(":{verb} command:\"node missing\"")),
                help,
                &catalogue
            )
            .is_err()
        );
    }
    assert!(matches!(
        targets::resolve(&call(":help"), true, &catalogue).unwrap(),
        QueryTarget::Root
    ));
    assert!(targets::resolve(&call(":inspect"), false, &catalogue).is_err());
}
#[test]
fn lexical_names_and_selected_reference_ports_remain_explicit() {
    for name in ["dev", "run-id", "development", "Summary"] {
        let bare = call(&format!(":inspect env:{name}"));
        let quoted = call(&format!(":inspect env:\"{name}\""));
        assert_eq!(
            bare.arguments[0].value.name().text,
            quoted.arguments[0].value.name().text
        );
    }
    let catalogue = Catalogue::default();
    for reference in ["x", "x::error", "x::cancel"] {
        let target =
            targets::resolve(&call(&format!(":inspect ${reference}")), false, &catalogue).unwrap();
        assert!(
            matches!(target,QueryTarget::Output {metadata:true,reference:r} if r.text == reference)
        );
        let read = commands::invocation(&call(&format!(":read value:${reference}"))).unwrap();
        assert!(matches!(&read.call.operands[0],Value::Reference(r) if r.text == reference));
    }
    assert!(resolve(&call(":inspect env:$x"), &catalogue).is_err());
}
#[test]
fn public_command_registry_has_unique_paths_and_direct_short_forms() {
    let mut paths = std::collections::BTreeSet::new();
    let mut shorts = std::collections::BTreeSet::new();
    for entry in commands::COMMAND_PATHS {
        assert!(paths.insert(entry.path));
        if let Some(short) = entry.short {
            assert!(shorts.insert(short));
        }
        let path = entry.path.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let signature = commands::signature(&path).unwrap();
        assert_eq!(signature.canonical, path.join(" "));
        assert_eq!(signature.command, entry.operation);
    }
}

#[test]
fn normalization_is_idempotent_and_preserves_reference_spans() {
    for source in [
        ":refresh $x",
        ":node refresh $x",
        ":read $x::error",
        ":inspect $x",
        ":inspect env:dev",
        ":help node refresh",
    ] {
        let original = call(source);
        let once = commands::normalize(&original).unwrap();
        assert_eq!(commands::normalize(&once).unwrap(), once);
        for value in original
            .operands
            .iter()
            .filter(|v| matches!(v, Value::Reference(_)))
        {
            assert!(
                once.operands
                    .iter()
                    .chain(once.arguments.iter().map(|a| &a.value))
                    .any(|n| n == value)
            );
        }
    }
}
#[test]
fn bare_named_text_uses_one_lexical_class_for_all_identifier_kinds() {
    for value in [
        "dev",
        "run-id",
        "Summary",
        "http://localhost:8771",
        "/tmp/file",
    ] {
        assert!(wes_language::bare_named_text(value));
    }
    for value in [
        "", "dev team", "$value", "a|b", "a>b", "a\\b", "a\"b", "{x}",
    ] {
        assert!(!wes_language::bare_named_text(value));
    }
}
