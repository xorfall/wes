use wes_language::{Expression, SourceText, parse};

#[test]
fn block_and_annotation_roundtrip_and_scope_the_entire_pipeline() {
    for text in [
        ":sandbox {\n:calc { return {text:'} | :sandbox'}; } > one\n$one | :calc { return input; } > two\n} > preview",
        "@sandbox :calc { return 1; } | :calc { return input + 1; } > preview",
        ":sandbox { :fork { { :calc { return 1; } } { :calc { return 2; } } } > branches } > preview",
    ] {
        let parsed = parse(&SourceText::new("sandbox", text));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        let statement = &parsed.script.statements[0];
        assert_eq!(statement.binding.as_ref().unwrap().name.text, "preview");
        assert!(matches!(statement.expression, Expression::Sandbox(_)));
        let canonical = parsed.script.to_string();
        let roundtrip = parse(&SourceText::new("roundtrip", &canonical));
        assert!(
            roundtrip.diagnostics.is_empty(),
            "{:?}",
            roundtrip.diagnostics
        );
        assert_eq!(canonical, roundtrip.script.to_string());
    }
}
#[test]
fn incomplete_blocks_and_annotation_arguments_are_rejected() {
    for text in [
        ":sandbox {} > empty",
        ":sandbox { :calc { return 1; }",
        "@sandbox(foo) :calc { return 1; } > bad",
    ] {
        assert!(
            !parse(&SourceText::new("bad", text)).diagnostics.is_empty(),
            "{text}"
        );
    }
}
