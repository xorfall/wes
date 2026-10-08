use wes_core::framing::{
    ByteSpan, Decoding, Delimiter, Error, Frame, Framer, Malformed, Profile, RejectionReason,
};

fn profile(delimiter: Delimiter) -> Profile {
    Profile {
        delimiter,
        decoding: Decoding::StrictUtf8,
        malformed: Malformed::Forensic { excerpt_bytes: 3 },
        raw_bytes: 4,
        decoded_bytes: 64,
        spans: 16,
    }
}
#[test]
fn scheduled_yields_preserve_exact_bytes_delimiter_carry_and_rejections() {
    for (input, delimiter) in [
        (b"ok\r\nabcdefghij\r\n\xff\nzz".as_slice(), Delimiter::Lines),
        (
            "éabababcdefghijabab終".as_bytes(),
            Delimiter::Literal(b"abab".to_vec()),
        ),
    ] {
        let expected = read(input, profile(delimiter.clone()), 3);
        for pause_at in 0..=input.len() {
            let mut framer = Framer::new(profile(delimiter.clone())).unwrap();
            let mut at = 0;
            let mut examined = 0;
            let mut paused = false;
            let mut frames = vec![];
            while at < input.len() {
                let pull = framer
                    .pull_frame_scheduled(&input[at..], |_| {
                        if !paused && examined == pause_at {
                            paused = true;
                            return Ok::<_, ()>(false);
                        }
                        examined += 1;
                        Ok(true)
                    })
                    .unwrap();
                at += pull.consumed;
                assert_eq!(framer.position(), at as u64);
                assert_eq!(examined, at);
                assert!(framer.buffered_bytes() <= 11);
                if pull.paused {
                    assert!(pull.frame.is_none());
                    assert_eq!(at, pause_at);
                }
                frames.extend(pull.frame);
            }
            framer
                .finish_frames(|frame| {
                    frames.push(frame);
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert_eq!(frames, expected, "pause at {pause_at}");
            assert_eq!(examined, input.len());
        }
    }
    let mut framer = Framer::new(profile(Delimiter::Lines)).unwrap();
    assert!(matches!(
        framer.pull_frame_scheduled(b"x", |_| Err::<bool, _>("fatal")),
        Err(Error::Admission("fatal"))
    ));
    assert!(matches!(
        framer.pull_frame_scheduled(b"x", |_| Ok::<_, ()>(true)),
        Err(Error::Closed)
    ));
}
fn read(input: &[u8], profile: Profile, block: usize) -> Vec<Frame> {
    let excerpt_limit = profile.malformed.excerpt_bytes();
    let carry = match &profile.delimiter {
        Delimiter::Lines => 2,
        Delimiter::Literal(d) => d.len(),
    };
    let bound = profile.raw_bytes + carry + profile.malformed.excerpt_bytes();
    let mut framer = Framer::new(profile).unwrap();
    let mut frames = vec![];
    let mut charged = 0;
    for block in input.chunks(block) {
        let mut at = 0;
        while at < block.len() {
            let (n, frame) = framer
                .pull_frame_admitted(&block[at..], |n| {
                    charged += n;
                    Ok::<_, ()>(())
                })
                .unwrap();
            assert!(n > 0);
            at += n;
            assert!(framer.buffered_bytes() <= bound);
            frames.extend(frame);
        }
    }
    framer
        .finish_frames(|frame| {
            frames.push(frame);
            Ok::<_, ()>(())
        })
        .unwrap();
    assert_eq!(
        charged,
        input.len(),
        "every skipped byte is admitted exactly once"
    );
    assert_eq!(framer.position(), input.len() as u64);
    assert_eq!(framer.buffered_bytes(), 0);
    for (at, frame) in frames.iter().enumerate() {
        match frame {
            Frame::Record(row) => assert_eq!(row.ordinal, at as u64),
            Frame::Rejected(row) => {
                assert_eq!(row.ordinal, at as u64);
                assert!(
                    row.excerpt.capacity() <= excerpt_limit,
                    "an excerpt must release the original allocation"
                );
                assert_eq!(
                    row.excerpt,
                    input[row.source.start as usize..row.source.start as usize + row.excerpt.len()]
                );
                assert_eq!(
                    row.excerpt_truncated,
                    (row.excerpt.len() as u64) < row.source.end - row.source.start
                );
                assert!(
                    row.reason_span.start >= row.source.start
                        && row.reason_span.end <= row.source.end
                );
            }
        }
    }
    frames
}

#[test]
fn oversized_lines_preserve_content_crlf_lone_cr_and_source_ordinals_at_every_split() {
    let input = b"ok\nabcdef\r\nx\rz\n";
    let p = profile(Delimiter::Lines);
    let frames = read(input, p.clone(), input.len());
    for block in 1..=input.len() {
        assert_eq!(read(input, p.clone(), block), frames);
    }
    let Frame::Rejected(row) = &frames[1] else {
        panic!("rejection")
    };
    assert_eq!(row.reason, RejectionReason::RawLimit);
    assert_eq!(row.source, ByteSpan { start: 3, end: 9 });
    assert_eq!(row.reason_span, row.source);
    assert_eq!(row.delimiter, ByteSpan { start: 9, end: 11 });
    assert_eq!(row.excerpt, b"abc");
    assert!(row.excerpt_truncated);
    assert!(!row.unterminated);
    let Frame::Record(last) = &frames[2] else {
        panic!("record")
    };
    assert_eq!(last.text, "x\rz");
    assert_eq!(last.ordinal, 2);
}

#[test]
fn overlapping_multibyte_delimiters_and_large_skips_remain_bounded_and_block_invariant() {
    for (input, delimiter) in [
        (b"longaaaaabokaaabtail".as_slice(), b"aaab".as_slice()),
        ("longabcdef<終>ok<終>".as_bytes(), "<終>".as_bytes()),
    ] {
        let p = profile(Delimiter::Literal(delimiter.to_vec()));
        let frames = read(input, p.clone(), input.len());
        for block in 1..=input.len() {
            assert_eq!(read(input, p.clone(), block), frames);
        }
        assert!(frames.iter().any(|f| matches!(f, Frame::Rejected(_))));
    }
    let mut input = vec![b'x'; 131_072];
    input.extend_from_slice(b"\r\nok\n");
    let p = profile(Delimiter::Lines);
    let expected = read(&input, p.clone(), 4096);
    for block in [1, 7, 64, 65_536] {
        assert_eq!(read(&input, p.clone(), block), expected);
    }
    let Frame::Rejected(row) = &expected[0] else {
        panic!("rejection")
    };
    assert_eq!(row.source.end, 131_072);
    assert_eq!(row.delimiter.end, 131_074);
    assert_eq!(row.excerpt, b"xxx");
    assert_eq!(expected.len(), 2);
}

#[test]
fn strict_and_lossy_decoding_remain_independent_of_recovery() {
    let input = b"ok\nA\xffB\nz\n";
    let p = profile(Delimiter::Lines);
    let frames = read(input, p.clone(), 1);
    let Frame::Rejected(row) = &frames[1] else {
        panic!("rejection")
    };
    assert_eq!(row.reason, RejectionReason::InvalidUtf8);
    assert_eq!(row.reason_span, ByteSpan { start: 4, end: 5 });
    assert_eq!(row.source, ByteSpan { start: 3, end: 6 });
    assert_eq!(row.excerpt, b"A\xffB");
    assert!(!row.excerpt_truncated);
    let mut lossy = p.clone();
    lossy.decoding = Decoding::LossyUtf8;
    let frames = read(input, lossy, 1);
    let Frame::Record(row) = &frames[1] else {
        panic!("lossy record")
    };
    assert_eq!(row.text, "A\u{fffd}B");
    assert!(row.lossy);
    assert_eq!(row.raw, b"A\xffB");
    let mut strict = p;
    strict.malformed = Malformed::Strict {};
    let mut f = Framer::new(strict).unwrap();
    assert_eq!(
        f.push_frames(input, |_| Ok::<_, ()>(())),
        Err(Error::InvalidUtf8(ByteSpan { start: 4, end: 5 }))
    );
    assert_eq!(f.finish_frames(|_| Ok::<_, ()>(())), Err(Error::Closed));
}

#[test]
fn rejected_decode_releases_the_large_original_record_allocation() {
    let mut p = profile(Delimiter::Lines);
    p.raw_bytes = 65_536;
    p.decoded_bytes = 131_072;
    let mut input = vec![b'x'; 32_768];
    input.extend_from_slice(b"\xff\nok\n");
    let frames = read(&input, p, 4096);
    let Frame::Rejected(row) = &frames[0] else {
        panic!("rejection")
    };
    assert_eq!(row.reason, RejectionReason::InvalidUtf8);
    assert_eq!(
        row.source,
        ByteSpan {
            start: 0,
            end: 32_769
        }
    );
    assert_eq!(
        row.reason_span,
        ByteSpan {
            start: 32_768,
            end: 32_769
        }
    );
    assert_eq!(row.excerpt, b"xxx");
    assert!(row.excerpt_truncated);
    assert!(matches!(&frames[1], Frame::Record(row) if row.ordinal == 1));
}

#[test]
fn decoded_and_mapping_limits_reject_one_frame_without_faking_text_or_losing_the_next() {
    for (decoded, spans, input, reason) in [
        (
            1,
            16,
            b"\xff\na\n".as_slice(),
            RejectionReason::DecodedLimit,
        ),
        (
            64,
            2,
            b"a\xffb\xff\nz\n".as_slice(),
            RejectionReason::SpanLimit,
        ),
    ] {
        let mut p = profile(Delimiter::Lines);
        p.decoding = Decoding::LossyUtf8;
        p.decoded_bytes = decoded;
        p.spans = spans;
        let expected = read(input, p.clone(), input.len());
        for block in 1..=input.len() {
            assert_eq!(read(input, p.clone(), block), expected);
        }
        let Frame::Rejected(row) = &expected[0] else {
            panic!("rejection")
        };
        assert_eq!(row.reason, reason);
        assert!(matches!(&expected[1], Frame::Record(row) if row.ordinal == 1));
    }
}

#[test]
fn only_clean_eof_finishes_unterminated_rejections_and_excerpts_never_include_delimiters() {
    let mut p = profile(Delimiter::Lines);
    p.raw_bytes = 1;
    p.malformed = Malformed::Forensic { excerpt_bytes: 16 };
    for input in [b"ab\r\n".as_slice(), b"ab".as_slice()] {
        let frames = read(input, p.clone(), 1);
        let Frame::Rejected(row) = &frames[0] else {
            panic!("rejection")
        };
        assert_eq!(row.excerpt, b"ab");
        assert!(!row.excerpt_truncated);
        assert_eq!(row.unterminated, input == b"ab");
        assert_eq!(row.source, ByteSpan { start: 0, end: 2 });
    }
    for (input, count) in [
        (b"".as_slice(), 0),
        (b"\n".as_slice(), 1),
        (b"\n\n".as_slice(), 2),
        (b"a\n".as_slice(), 1),
    ] {
        assert_eq!(read(input, p.clone(), 1).len(), count);
    }
}

#[test]
fn byte_admission_and_emitter_failures_close_without_inventing_a_final_rejection() {
    let mut f = Framer::new(profile(Delimiter::Lines)).unwrap();
    let mut charged = 0;
    assert_eq!(
        f.pull_frame_admitted(b"abcdefg\n", |_| {
            if charged == 6 {
                return Err("quota");
            }
            charged += 1;
            Ok(())
        }),
        Err(Error::Admission("quota"))
    );
    assert_eq!(f.position(), 6);
    let mut calls = 0;
    assert_eq!(
        f.finish_frames(|_| {
            calls += 1;
            Ok::<_, &str>(())
        }),
        Err(Error::Closed)
    );
    assert_eq!(calls, 0);
    let mut f = Framer::new(profile(Delimiter::Lines)).unwrap();
    assert_eq!(
        f.push_frames(b"abcdef\nok\n", |_| {
            calls += 1;
            Err("sink")
        }),
        Err(Error::Admission("sink"))
    );
    assert_eq!(calls, 1);
    assert_eq!(f.position(), 7);
    assert_eq!(
        f.push_frames(b"ok\n", |_| Ok::<_, &str>(())),
        Err(Error::Closed)
    );
}

#[test]
fn record_only_ports_refuse_forensic_profiles_before_admission_instead_of_silently_dropping() {
    let mut f = Framer::new(profile(Delimiter::Lines)).unwrap();
    let mut calls = 0;
    assert_eq!(
        f.push(b"abcdef\n", |_| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(Error::InvalidProfile)
    );
    assert_eq!(
        f.pull_admitted(b"abcdef\n", |_| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(Error::InvalidProfile)
    );
    assert_eq!(
        f.finish(|_| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(Error::InvalidProfile)
    );
    assert_eq!(calls, 0);
    assert_eq!(f.position(), 0);
    assert!(matches!(
        f.pull_frame_admitted(b"abcdef\n", |_| Ok::<_, ()>(()))
            .unwrap()
            .1,
        Some(Frame::Rejected(_))
    ));
}

#[test]
fn restored_boundaries_preserve_source_offsets_ordinals_and_refuse_counter_overflow() {
    let p = profile(Delimiter::Lines);
    let mut f = Framer::at_boundary(p.clone(), 50, 17).unwrap();
    let mut frames = vec![];
    f.push_frames(b"123456\nok\n", |frame| {
        frames.push(frame);
        Ok::<_, ()>(())
    })
    .unwrap();
    let Frame::Rejected(row) = &frames[0] else {
        panic!("rejection")
    };
    assert_eq!(row.ordinal, 17);
    assert_eq!(row.source, ByteSpan { start: 50, end: 56 });
    assert!(
        matches!(&frames[1], Frame::Record(row) if row.ordinal == 18 && row.source.start == 57)
    );
    let mut calls = 0;
    let mut f = Framer::at_boundary(p.clone(), 0, u64::MAX).unwrap();
    assert_eq!(
        f.push_frames(b"a\n", |_| {
            calls += 1;
            Ok::<_, ()>(())
        }),
        Err(Error::PositionExhausted)
    );
    assert_eq!(calls, 0);
    let mut f = Framer::at_boundary(p, u64::MAX - 1, 0).unwrap();
    assert_eq!(
        f.push_frames(b"ab", |_| Ok::<_, ()>(())),
        Err(Error::PositionExhausted)
    );
    assert_eq!(f.position(), u64::MAX);
}

#[test]
fn malformed_policy_is_explicit_closed_and_bounded() {
    for bytes in [0, 4097] {
        let mut p = profile(Delimiter::Lines);
        p.malformed = Malformed::Forensic {
            excerpt_bytes: bytes,
        };
        assert!(matches!(Framer::new(p), Err(Error::InvalidProfile)));
    }
    let p = profile(Delimiter::Lines);
    let mut raw = serde_json::to_value(&p).unwrap();
    raw.as_object_mut().unwrap().remove("malformed");
    assert!(serde_json::from_value::<Profile>(raw).is_err());
    for policy in [
        serde_json::json!({"kind":"repair"}),
        serde_json::json!({"kind":"strict","extra":1}),
    ] {
        let mut raw = serde_json::to_value(&p).unwrap();
        raw["malformed"] = policy;
        assert!(serde_json::from_value::<Profile>(raw).is_err());
    }
}
