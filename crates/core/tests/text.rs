use wes_core::text::{AnsiError, ansi_spans};

#[test]
fn normalization_maps_unicode_and_line_endings_to_original_bytes() {
    let source = "é\x1b[31mERROR\x1b[0m\r\n\x1b]8;;https://example.invalid\x07link\x1b]8;;\x1b\\終";
    let mut spans = vec![];
    ansi_spans(source, |span| {
        spans.push(span);
        Ok::<_, ()>(())
    })
    .unwrap();
    let output: String = spans
        .iter()
        .map(|s| &source[s.input_start..s.input_end])
        .collect();
    assert_eq!(output, "éERROR\r\nlink終");
    let mut previous = 0;
    for span in spans {
        assert_eq!(span.output_start, previous);
        assert_eq!(
            &output[span.output_start..span.output_end],
            &source[span.input_start..span.input_end]
        );
        assert_eq!(
            span.input_end - span.input_start,
            span.output_end - span.output_start
        );
        previous = span.output_end;
    }
    assert_eq!(previous, output.len());
}

#[test]
fn empty_removed_and_unchanged_text_have_explicit_mapping() {
    for (source, expected, count) in [("", "", 0), ("\x1b[0m", "", 0), ("λ\n", "λ\n", 1)] {
        let mut output = String::new();
        let mut n = 0;
        ansi_spans(source, |s| {
            output.push_str(&source[s.input_start..s.input_end]);
            n += 1;
            Ok::<_, ()>(())
        })
        .unwrap();
        assert_eq!(output, expected);
        assert_eq!(n, count);
    }
}

#[test]
fn malformed_or_unsupported_escapes_and_output_admission_fail_safely() {
    for source in [
        "\x1b",
        "\x1b[",
        "\x1b[31",
        "\x1b]title",
        "\x1b]x\x1b",
        "\x1b7",
        "\x1b[é",
    ] {
        assert_eq!(
            ansi_spans(source, |_| Ok::<_, ()>(())),
            Err(AnsiError::Invalid)
        );
    }
    assert_eq!(
        ansi_spans("plain", |_| Err("full")),
        Err(AnsiError::Admission("full"))
    );
}
