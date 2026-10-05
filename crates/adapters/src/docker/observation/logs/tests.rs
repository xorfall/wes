use super::*;
fn frame(channel: u8, bytes: &[u8]) -> Vec<u8> {
    let mut wire = vec![channel, 0, 0, 0];
    wire.extend_from_slice(&(bytes.len() as u32).to_be_bytes());
    wire.extend_from_slice(bytes);
    wire
}
fn fields(data: &Data) -> &indexmap::IndexMap<String, Data> {
    let Data::Record(fields) = data else {
        panic!("row")
    };
    fields
}
#[test]
fn multiplex_reassembles_utf8_and_lines_across_frames_without_mixing_channels() {
    let mut wire = frame(1, b"2026-09-24T00:00:00.123456789Z h\xc3");
    wire.extend(frame(2, b"error\n"));
    wire.extend(frame(1, b"\xa9llo\r\nlast"));
    let result = decode(&wire, false, 200, false).unwrap();
    assert!(!result.line_cut);
    assert_eq!(result.rows.len(), 3);
    assert_eq!(
        fields(&result.rows[0])["stream"],
        Data::Text("stderr".into())
    );
    let row = fields(&result.rows[1]);
    assert_eq!(row["text"], Data::Text("héllo".into()));
    assert_eq!(row["lossy"], Data::Bool(false));
    assert_eq!(row["partial"], Data::Bool(false));
    assert_eq!(
        row["timestamp_ns"],
        Data::Option(Some(Box::new(Data::Int(1790208000123456789))))
    );
    assert_eq!(fields(&result.rows[2])["partial"], Data::Bool(true));
}
#[test]
fn tty_optional_time_empty_lines_and_invalid_utf8_are_honest() {
    let result = decode(b"not-a-time payload\n\n\xfflast", true, 3, false).unwrap();
    assert_eq!(result.rows.len(), 3);
    for row in &result.rows {
        assert_eq!(fields(row)["stream"], Data::Text("tty".into()));
    }
    assert_eq!(fields(&result.rows[0])["timestamp_ns"], Data::Option(None));
    assert_eq!(
        fields(&result.rows[0])["text"],
        Data::Text("not-a-time payload".into())
    );
    assert_eq!(fields(&result.rows[1])["text"], Data::Text("".into()));
    assert_eq!(fields(&result.rows[2])["lossy"], Data::Bool(true));
    assert_eq!(fields(&result.rows[2])["partial"], Data::Bool(true));
}
#[test]
fn all_frame_eof_boundaries_distinguish_network_damage_from_local_cut() {
    let full = frame(1, b"hello\n");
    for i in 1..full.len() {
        assert!(
            decode(&full[..i], false, 200, false).is_err(),
            "boundary {i}"
        );
        let cut = decode(&full[..i], false, 200, true).unwrap();
        if i > 8 {
            assert_eq!(fields(&cut.rows[0])["partial"], Data::Bool(true));
        }
    }
    for header in [vec![3, 0, 0, 0, 0, 0, 0, 0], vec![1, 1, 0, 0, 0, 0, 0, 0]] {
        assert!(decode(&header, false, 200, false).is_err());
        assert!(decode(&header, false, 200, true).is_err());
    }
    assert!(
        decode(&frame(1, b""), false, 1, false)
            .unwrap()
            .rows
            .is_empty()
    );
}
#[test]
fn exact_line_limit_is_not_reported_as_truncated_and_extra_lines_are() {
    let exact = decode(b"one\ntwo\n", true, 2, false).unwrap();
    assert_eq!(exact.rows.len(), 2);
    assert!(!exact.line_cut);
    for bytes in [
        b"one\ntwo\nthree\n".as_slice(),
        b"one\ntwo\npartial".as_slice(),
    ] {
        let extra = decode(bytes, true, 2, false).unwrap();
        assert!(extra.line_cut);
        assert_eq!(extra.rows.len(), 2);
    }
}
#[test]
fn byte_bounded_long_line_does_not_pretend_to_be_complete() {
    let wire = frame(1, &vec![b'x'; wire_limit() + 40]);
    let result = decode(&wire[..wire_limit()], false, 200, true).unwrap();
    let row = fields(&result.rows[0]);
    assert_eq!(row["partial"], Data::Bool(true));
    let Data::Text(text) = &row["text"] else {
        panic!("text")
    };
    assert_eq!(text.len(), wire_limit() - 8);
}

#[test]
fn incremental_decoder_is_invariant_under_every_transport_split() {
    let mut wire = frame(1, "2026-09-24T00:00:00Z Türkçe 🚀\nnext".as_bytes());
    wire.extend(frame(2, b"err\n"));
    wire.extend(frame(1, b" line\n"));
    let expected = decode(&wire, false, 200, false).unwrap().rows;
    for split in 0..=wire.len() {
        let mut rows = vec![];
        let mut emit = |line: Line| {
            rows.push(Data::Record(line_data(&line)));
            Ok(())
        };
        let mut parser = Decoder::new(false, wire_limit());
        parser.feed(&wire[..split], &mut emit).unwrap();
        parser.feed(&wire[split..], &mut emit).unwrap();
        parser.finish(false, &mut emit).unwrap();
        assert_eq!(rows, expected, "split {split}");
    }
    let mut rows = vec![];
    let mut emit = |line: Line| {
        rows.push(Data::Record(line_data(&line)));
        Ok(())
    };
    let mut parser = Decoder::new(false, wire_limit());
    for byte in &wire {
        parser.feed(&[*byte], &mut emit).unwrap();
    }
    parser.finish(false, &mut emit).unwrap();
    assert_eq!(rows, expected);
}
#[test]
fn overlong_lines_emit_one_marked_prefix_then_resume_at_the_next_line() {
    for tty in [false, true] {
        let content = b"0123456789abcdef\nok\nlast";
        let wire = if tty {
            content.to_vec()
        } else {
            frame(1, content)
        };
        let mut lines = vec![];
        let mut emit = |line: Line| {
            lines.push(line);
            Ok(())
        };
        let mut parser = Decoder::new(tty, 8);
        for byte in &wire {
            parser.feed(&[*byte], &mut emit).unwrap();
        }
        parser.finish(false, &mut emit).unwrap();
        assert_eq!(lines.len(), 3);
        assert_eq!(lines[0].bytes, b"01234567");
        assert!(lines[0].partial && lines[0].truncated);
        assert_eq!(lines[1].bytes, b"ok");
        assert!(!lines[1].partial && !lines[1].truncated);
        assert_eq!(lines[2].bytes, b"last");
        assert!(lines[2].partial && !lines[2].truncated);
    }
}
#[test]
fn claimed_frame_size_is_not_an_allocation_or_a_valid_eof() {
    let mut parser = Decoder::new(false, 8);
    let mut lines = vec![];
    let mut emit = |line: Line| {
        lines.push(line);
        Ok(())
    };
    parser
        .feed(&[1, 0, 0, 0, 255, 255, 255, 255], &mut emit)
        .unwrap();
    for _ in 0..1024 {
        parser.feed(b"abcdefghi", &mut emit).unwrap();
    }
    assert!(parser.finish(false, &mut emit).is_err());
    assert_eq!(lines.len(), 1);
    assert_eq!(lines[0].bytes.len(), 8);
    assert!(lines[0].truncated);
}
