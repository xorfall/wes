use wes_language::{SourceText, parse};
#[test]
fn trace_parentheses_round_trip_without_changing_other_annotations_or_urls() {
    for text in [
        "@trace(http) http request url:\"http://localhost/a(b)\" > answer",
        "@env{dev} @trace(http) api get",
        "@unchecked{arg} api get arg:hello",
        "@trace{http} api get",
    ] {
        let parsed = parse(&SourceText::new("test", text));
        assert!(
            parsed.diagnostics.is_empty(),
            "{text}: {:?}",
            parsed.diagnostics
        );
        let statement = &parsed.script.statements[0];
        let written = statement.to_string();
        let again = parse(&SourceText::new("roundtrip", &written));
        assert!(again.diagnostics.is_empty(), "{written}");
        assert_eq!(again.script.statements[0].to_string(), written);
    }
    for text in [
        "@trace(http api get",
        "@trace(http} api get",
        "@trace{http) api get",
    ] {
        assert!(
            !parse(&SourceText::new("bad", text)).diagnostics.is_empty(),
            "{text}"
        );
    }
}

#[test]
fn trace_profiles_are_provider_selected_names_with_finite_call_and_size_constraints() {
    use wes_core::{
        Shape,
        capability::{Capability, Catalogue, ProviderDescription, Safety},
    };
    use wes_language::{
        Expression,
        check::{self, Environment},
        resolve::resolve,
    };
    let mut catalogue = Catalogue::new();
    let finite = Capability::new(["read"], Shape::Unknown, Safety::Safe);
    let mut stream = Capability::new(["stream"], Shape::Unknown, Safety::Safe);
    stream.streaming = true;
    catalogue.register(ProviderDescription::new("fixture", [finite, stream], vec![]).unwrap());
    for (source, valid) in [
        ("@trace(grpc) fixture read".into(), true),
        ("@trace{binary} fixture read".into(), true),
        ("@trace(http) fixture read".into(), true),
        (format!("@trace({}) fixture read", "a".repeat(64)), true),
        (format!("@trace({}) fixture read", "a".repeat(65)), false),
        ("@trace fixture read".into(), false),
        ("@trace(binary,grpc) fixture read".into(), false),
        ("@trace(binary) @trace(grpc) fixture read".into(), false),
        ("@trace(binary) fixture stream".into(), false),
        ("@trace(binary) @interactive fixture read".into(), false),
        ("@trace(binary) :help".into(), false),
    ] {
        let parsed = parse(&SourceText::new("profiles", &source));
        assert!(
            parsed.diagnostics.is_empty(),
            "{source}: {:?}",
            parsed.diagnostics
        );
        let statement = &parsed.script.statements[0];
        let Expression::Call(call) = &statement.expression else {
            panic!()
        };
        let resolution = resolve(call, &catalogue).unwrap();
        let diagnostics = check::check(
            &resolution,
            &statement.annotations,
            &Default::default(),
            &Environment::default(),
        );
        assert_eq!(
            !diagnostics.iter().any(|d| d.code == "TRC001"),
            valid,
            "{source}: {diagnostics:?}"
        );
    }
}
