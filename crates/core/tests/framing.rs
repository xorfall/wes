use wes_core::framing::{ByteSpan, Decoding, Delimiter, Error, Framer, Profile, Record};

fn profile(delimiter: Delimiter, decoding: Decoding) -> Profile {
    Profile {
        delimiter,
        decoding,
        raw_bytes: 1024,
        decoded_bytes: 4096,
        spans: 128,
    }
}
fn read(input: &[u8], profile: Profile, block: usize) -> Vec<Record> {
    let mut framer = Framer::new(profile).unwrap();
    let mut records = vec![];
    for bytes in input.chunks(block) {
        framer
            .push(bytes, |row| {
                records.push(row);
                Ok::<_, ()>(())
            })
            .unwrap();
    }
    framer
        .finish(|row| {
            records.push(row);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(framer.position(), input.len() as u64);
    assert_eq!(framer.buffered_bytes(), 0);
    records
}
#[test]
fn line_positions_and_unicode_are_independent_of_every_read_block_size() {
    let input = "é\r\n\nx\ry\n終".as_bytes();
    let p = profile(Delimiter::Lines, Decoding::StrictUtf8);
    let expected = read(input, p.clone(), input.len());
    for block in 1..=input.len() {
        assert_eq!(read(input, p.clone(), block), expected);
    }
    assert_eq!(
        expected.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
        ["é", "", "x\ry", "終"]
    );
    assert_eq!(expected[0].source, ByteSpan { start: 0, end: 2 });
    assert_eq!(expected[0].delimiter, ByteSpan { start: 2, end: 4 });
    assert_eq!(expected[1].source, ByteSpan { start: 4, end: 4 });
    assert_eq!(expected[2].source, ByteSpan { start: 5, end: 8 });
    assert_eq!(expected[3].source, ByteSpan { start: 9, end: 12 });
    assert_eq!(expected[3].delimiter, ByteSpan { start: 12, end: 12 });
    assert!(expected[3].unterminated);
    assert!(!expected[0].unterminated);
    for row in expected {
        assert_eq!(
            row.raw,
            input[row.source.start as usize..row.source.end as usize]
        );
        for span in row.spans {
            assert_eq!(
                &row.text[span.output_start..span.output_end],
                std::str::from_utf8(&row.raw[span.input_start..span.input_end]).unwrap()
            );
        }
    }
}
#[test]
fn empty_trailing_and_consecutive_delimiters_never_invent_an_extra_record() {
    for (input, count) in [("", 0), ("\n", 1), ("\n\n", 2), ("a\n", 1), ("a", 1)] {
        let rows = read(
            input.as_bytes(),
            profile(Delimiter::Lines, Decoding::StrictUtf8),
            1,
        );
        assert_eq!(rows.len(), count);
        assert_eq!(
            rows.iter().map(|r| r.ordinal).collect::<Vec<_>>(),
            (0..count as u64).collect::<Vec<_>>()
        );
    }
}
#[test]
fn multibyte_and_overlapping_literal_delimiters_cross_blocks_without_changing_positions() {
    for (input, delimiter, expected) in [
        ("left<終>right<終>", "<終>", vec!["left", "right"]),
        ("aaabaaaab", "aab", vec!["a", "aa"]),
        ("aababab", "abab", vec!["a", "ab"]),
    ] {
        let p = profile(
            Delimiter::Literal(delimiter.as_bytes().into()),
            Decoding::StrictUtf8,
        );
        let rows = read(input.as_bytes(), p.clone(), input.len());
        assert_eq!(
            rows.iter().map(|r| r.text.as_str()).collect::<Vec<_>>(),
            expected
        );
        for block in 1..=input.len() {
            assert_eq!(read(input.as_bytes(), p.clone(), block), rows);
        }
    }
}
#[test]
fn lossy_decoding_marks_exact_original_replacement_spans_and_bounds_mapping_growth() {
    let raw = b"a\xff\xfe\xf0\x9f";
    let p = profile(Delimiter::Lines, Decoding::LossyUtf8);
    let rows = read(raw, p.clone(), 1);
    assert_eq!(rows[0].text, "a\u{fffd}\u{fffd}\u{fffd}");
    assert!(rows[0].lossy);
    assert_eq!(rows[0].raw, raw);
    assert_eq!(rows[0].spans.last().unwrap().input_start, 3);
    assert_eq!(rows[0].spans.last().unwrap().input_end, 5);
    for block in 1..=raw.len() {
        assert_eq!(read(raw, p.clone(), block), rows);
    }
    let mut limited = p;
    limited.spans = 2;
    let mut f = Framer::new(limited).unwrap();
    f.push(raw, |_| Ok::<_, ()>(())).unwrap();
    assert_eq!(
        f.finish(|_| Ok::<_, ()>(())),
        Err(Error::SpanLimit(ByteSpan { start: 0, end: 5 }))
    );
}
#[test]
fn strict_decode_raw_decoded_and_callback_failures_close_the_cursor_without_replay() {
    let mut f = Framer::new(profile(Delimiter::Lines, Decoding::StrictUtf8)).unwrap();
    assert_eq!(
        f.push(b"ok\n\xff\n", |_| Ok::<_, ()>(())),
        Err(Error::InvalidUtf8(ByteSpan { start: 3, end: 4 }))
    );
    assert_eq!(f.finish(|_| Ok::<_, ()>(())), Err(Error::Closed));
    let mut p = profile(Delimiter::Lines, Decoding::StrictUtf8);
    p.raw_bytes = 2;
    assert_eq!(read(b"ab\r\n", p.clone(), 1)[0].text, "ab");
    let mut f = Framer::new(p.clone()).unwrap();
    f.push(b"ab\r", |_| Ok::<_, ()>(())).unwrap();
    assert_eq!(
        f.finish(|_| Ok::<_, ()>(())),
        Err(Error::RawLimit(ByteSpan { start: 0, end: 3 }))
    );
    let mut f = Framer::new(p).unwrap();
    assert_eq!(
        f.push(b"abc", |_| Ok::<_, ()>(())),
        Err(Error::RawLimit(ByteSpan { start: 0, end: 3 }))
    );
    let mut p = profile(Delimiter::Lines, Decoding::LossyUtf8);
    p.decoded_bytes = 2;
    let mut f = Framer::new(p).unwrap();
    assert_eq!(
        f.push(b"\xff\n", |_| Ok::<_, ()>(())),
        Err(Error::DecodedLimit(ByteSpan { start: 0, end: 1 }))
    );
    let mut f = Framer::new(profile(Delimiter::Lines, Decoding::StrictUtf8)).unwrap();
    let mut calls = 0;
    assert_eq!(
        f.push(b"a\nb\n", |_| {
            calls += 1;
            Err("sink full")
        }),
        Err(Error::Admission("sink full"))
    );
    assert_eq!(calls, 1);
    assert_eq!(f.push(b"b\n", |_| Ok::<_, &str>(())), Err(Error::Closed));
}

#[test]
fn pull_admits_each_byte_once_and_refuses_before_examining_it() {
    let mut f = Framer::new(profile(Delimiter::Lines, Decoding::StrictUtf8)).unwrap();
    let mut charged = 0;
    let (consumed, row) = f
        .pull_admitted(b"a\nb\n", |n| {
            charged += n;
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(consumed, 2);
    assert_eq!(charged, 2);
    assert_eq!(row.unwrap().text, "a");
    let (consumed, row) = f
        .pull_admitted(b"b\n", |n| {
            charged += n;
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(consumed, 2);
    assert_eq!(charged, 4);
    assert_eq!(row.unwrap().text, "b");
    assert_eq!(
        f.pull_admitted(b"unread", |_| Err("quota")),
        Err(Error::Admission("quota"))
    );
    assert_eq!(f.position(), 4);
    assert_eq!(f.buffered_bytes(), 0);
    assert_eq!(
        f.pull_admitted(b"unread", |_| Ok::<_, &str>(())),
        Err(Error::Closed)
    );
}
