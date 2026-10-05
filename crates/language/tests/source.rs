use wes_language::{Position, SourceText, Span};

#[test]
fn utf8_storage_and_utf16_protocol_offsets_are_not_conflated() {
    let source = SourceText::new("input", "a🦀é\r\nnext");
    assert_eq!(source.utf16_offset(5).unwrap(), 3);
    assert_eq!(source.byte_offset(3).unwrap(), 5);
    assert!(source.byte_offset(2).is_err());
    assert!(source.utf16_offset(2).is_err());
    assert_eq!(source.position(7).unwrap(), Position { line: 1, column: 5 });
    assert_eq!(source.line(1).unwrap(), "a🦀é");
    assert_eq!(source.line(2).unwrap(), "next");
}

#[test]
fn offsets_round_trip_at_every_scalar_boundary() {
    let source = SourceText::new("input", "İ😀e\u{301}\n終");
    for byte in source
        .text()
        .char_indices()
        .map(|(byte, _)| byte)
        .chain([source.byte_len()])
    {
        assert_eq!(
            source
                .byte_offset(source.utf16_offset(byte).unwrap())
                .unwrap(),
            byte
        );
    }
}

#[test]
fn empty_and_trailing_lines_and_end_of_input_are_positions() {
    let empty = SourceText::new("input", "");
    assert_eq!(empty.line_count(), 1);
    assert_eq!(empty.position(0).unwrap(), Position { line: 1, column: 1 });
    let text = SourceText::new("input", "x\n");
    assert_eq!(text.line(2).unwrap(), "");
    assert_eq!(text.position(2).unwrap(), Position { line: 2, column: 1 });
    assert!(text.line(0).is_err());
    assert!(text.line(3).is_err());
    assert!(text.position(3).is_err());
}

#[test]
fn spans_are_half_open_and_validate_order_and_encoding() {
    assert!(Span::new(2, 1).is_err());
    let span = Span::new(1, 3).unwrap();
    assert!(span.contains(1));
    assert!(!span.contains(3));
    assert_eq!(span.union(Span::at(5)), Span::new(1, 5).unwrap());
    let source = SourceText::new("input", "é");
    assert!(source.slice(Span::new(0, 1).unwrap()).is_err());
}
#[test]
fn source_labels_are_portable_without_becoming_execution_paths() {
    use wes_language::{SourceText, portable_source_name};
    for name in [
        "/opt/example/scripts/ölçüm.wes",
        r"C:\example\scripts\ölçüm.wes",
        r"\\host\share\ölçüm.wes",
    ] {
        assert_eq!(portable_source_name(name), "ölçüm.wes");
        assert_eq!(SourceText::new(name, "return 1;").name(), "ölçüm.wes");
    }
    for label in ["cell abc123", "<input>", "examples/sample.wes"] {
        assert_eq!(portable_source_name(label), label);
    }
}

#[test]
fn sliced_source_positions_keep_original_utf16_columns_without_padding() {
    let original = SourceText::new("workflow.wes", "é🦀 prior: next\nend");
    let byte = original.text().find("next").unwrap();
    let start = original.position(byte).unwrap();
    let sliced = SourceText::new(original.name(), &original.text()[byte..])
        .with_start(start)
        .unwrap();
    assert_eq!(sliced.position(0).unwrap(), start);
    assert_eq!(
        sliced.position(4).unwrap(),
        original.position(byte + 4).unwrap()
    );
    assert_eq!(sliced.position(5).unwrap(), Position { line: 2, column: 1 });
    assert!(
        SourceText::new("empty", "")
            .with_start(Position { line: 0, column: 1 })
            .is_err()
    );
    assert!(
        SourceText::new("empty", "x")
            .with_start(Position {
                line: usize::MAX,
                column: usize::MAX
            })
            .unwrap()
            .position(1)
            .is_err()
    );
}
