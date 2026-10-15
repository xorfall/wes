use wes_language::{SourceText, parse};

#[test]
fn capture_annotations_round_trip_named_literals_and_preserve_unrelated_annotations() {
    for source in [
        "@trace(capture:\"lab\", tail:\"500ms\") fixture read",
        "@env{dev} @trace(tcp) fixture read",
        "@trace(capture:\"name with spaces\",tail:\"0s\") fixture read",
        "@unchecked{arg} fixture read arg:hello",
    ] {
        let parsed = parse(&SourceText::new("test", source));
        assert!(
            parsed.diagnostics.is_empty(),
            "{source}: {:?}",
            parsed.diagnostics
        );
        let written = parsed.script.statements[0].to_string();
        let again = parse(&SourceText::new("roundtrip", &written));
        assert!(again.diagnostics.is_empty(), "{written}");
        assert_eq!(again.script.statements[0].to_string(), written);
        if let Some(annotation) = parsed.script.statements[0]
            .annotations
            .iter()
            .find(|a| a.name.text == "trace")
        {
            wes_language::capture::CaptureAnnotation::parse(annotation).unwrap();
            for argument in &annotation.arguments {
                assert!(argument.span.start() < argument.span.end());
            }
        }
    }
    for source in [
        "@trace(capture:\"lab\" fixture read",
        "@trace(capture:\"lab\"} fixture read",
        "@trace(capture:) fixture read",
        "@trace(capture:\"lab\",) fixture read",
        "@trace(capture:\"lab\",,tail:\"1s\") fixture read",
        "@trace(capture:\"lab\" tail:\"1s\") fixture read",
    ] {
        assert!(
            !parse(&SourceText::new("bad", source))
                .diagnostics
                .is_empty(),
            "{source}"
        );
    }
}

#[test]
fn only_finite_literal_capture_annotations_are_admitted() {
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
        ("@trace(tcp) fixture read".to_owned(), true),
        (
            "@trace(capture:\"lab\",tail:\"1s\") fixture read".into(),
            true,
        ),
        (
            format!("@trace(capture:\"{}\") fixture read", "a".repeat(64)),
            true,
        ),
        (
            format!("@trace(capture:\"{}\") fixture read", "a".repeat(65)),
            false,
        ),
        ("@trace(http) fixture read".into(), false),
        ("@trace(grpc) fixture read".into(), false),
        ("@trace fixture read".into(), false),
        ("@trace(capture:$profile) fixture read".into(), false),
        ("@trace(capture:\"\") fixture read".into(), false),
        ("@trace(capture:\" lab\") fixture read".into(), false),
        (
            "@trace(capture:\"lab\",capture:\"other\") fixture read".into(),
            false,
        ),
        ("@trace(tail:\"1s\") fixture read".into(), false),
        (
            "@trace(capture:\"lab\",tail:\"61s\") fixture read".into(),
            false,
        ),
        (
            "@trace(capture:\"lab\",tail:\"01s\") fixture read".into(),
            false,
        ),
        (
            "@trace(capture:\"lab\",tail:\"-1s\") fixture read".into(),
            false,
        ),
        (
            "@trace(capture:\"lab\",filter:\"tcp\") fixture read".into(),
            false,
        ),
        ("@trace(tcp) @trace(tcp) fixture read".into(), false),
        ("@trace(tcp) fixture stream".into(), false),
        ("@trace(tcp) @interactive fixture read".into(), false),
        ("@trace(tcp) :help".into(), false),
        ("@env(capture:\"lab\") fixture read".into(), false),
    ] {
        let parsed = parse(&SourceText::new("capture", &source));
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
            !diagnostics
                .iter()
                .any(|d| matches!(d.code.as_ref(), "TRC001" | "CHK014")),
            valid,
            "{source}: {diagnostics:?}"
        );
    }
}

#[test]
fn public_capture_commands_have_closed_typed_signatures_and_retired_reads_refuse() {
    use wes_language::{Expression, resolve::resolve, vocabulary::MetaCommand};
    for (source, operation) in [
        (
            ":trace start capture:\"lab\" duration:\"20s\" filter:\"port 4242\" packets:10 bytes:4096",
            MetaCommand::TraceStart,
        ),
        (":trace stop $capture", MetaCommand::TraceStop),
        (":trace read $business", MetaCommand::TraceRead),
        (
            ":trace import file:\"synthetic.pcap\" using:\"pcap\"",
            MetaCommand::TraceImport,
        ),
        (
            ":trace analyze $raw.raw using:\"headers\" options:{packets:2,headerBytes:128}",
            MetaCommand::TraceAnalyze,
        ),
        (
            ":trace export $raw.raw using:\"pcap\" file:\"/owned/result.pcap\" allow_confidential:true",
            MetaCommand::TraceExport,
        ),
        (":trace retain $raw", MetaCommand::Keep),
    ] {
        let parsed = parse(&SourceText::new("commands", source));
        assert!(parsed.diagnostics.is_empty());
        let Expression::Call(call) = &parsed.script.statements[0].expression else {
            panic!()
        };
        let resolution = resolve(call, &Default::default()).unwrap();
        let wes_language::resolve::Resolution::Meta { spec, .. } = resolution else {
            panic!()
        };
        assert_eq!(spec.command, operation);
        if spec.operands.min > 0 {
            let path = spec
                .canonical
                .split_whitespace()
                .map(str::to_owned)
                .collect::<Vec<_>>();
            let help = wes_language::vocabulary::commands::signature(&path).unwrap();
            let usage = wes_language::vocabulary::commands::help_usage(&help);
            assert!(
                usage.contains(if spec.field_operands {
                    "$selection"
                } else {
                    "$result"
                }),
                "{usage}"
            );
        }
    }
    for source in [
        ":trace $call",
        ":read trace:$call",
        ":read $call run:\"old\"",
    ] {
        let parsed = parse(&SourceText::new("retired", source));
        let Expression::Call(call) = &parsed.script.statements[0].expression else {
            panic!()
        };
        assert!(resolve(call, &Default::default()).is_err(), "{source}");
    }
    assert_eq!(
        wes_language::capture::milliseconds("2m", 120000),
        Some(120000)
    );
    for text in ["1h", "-1s", "01s", "1.5s", "99999999999s"] {
        assert!(wes_language::capture::milliseconds(text, 120000).is_none());
    }
}
