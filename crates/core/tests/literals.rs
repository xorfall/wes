use wes_core::{Data, Decimal, DurationValue, Primitive, Shape, Timestamp, literals};

#[test]
fn extreme_decimal_exponents_stay_compact_and_comparable() {
    let huge: Decimal = "1e2147483648".parse().unwrap();
    let tiny: Decimal = "1e-2147483647".parse().unwrap();
    assert_eq!(huge.to_string(), "1E+2147483648");
    assert_eq!(tiny.to_string(), "1E-2147483647");
    assert!(huge.numeric_cmp(&tiny).is_gt());
    assert!("1e-2147483648".parse::<Decimal>().is_err());
    assert!("1_000".parse::<Decimal>().is_err());
}

#[test]
fn temporal_values_reject_out_of_range_construction() {
    assert!(Timestamp::new(-31_557_014_167_219_201, 0).is_err());
    assert!(Timestamp::new(31_556_889_864_403_200, 0).is_err());
    assert!(Timestamp::new(0, 1_000_000_000).is_err());
    assert!(DurationValue::new(i64::MIN, 0).is_ok());
    assert!(DurationValue::new(i64::MAX, 999_999_999).is_ok());
}

#[test]
fn timestamp_format_parse_round_trips_across_the_full_supported_epoch_range() {
    let mut state = 0x123456789abcdefu64;
    for _ in 0..2000 {
        state = state.wrapping_mul(6364136223846793005).wrapping_add(1);
        let seconds = (i128::from(state) % 63_113_904_031_622_400 - 31_557_014_167_219_200) as i64;
        let timestamp = Timestamp::new(seconds, (state % 1_000_000_000) as u32).unwrap();
        assert_eq!(
            timestamp.to_string().parse::<Timestamp>().unwrap(),
            timestamp
        );
    }
}

#[test]
fn negative_duration_fraction_round_trips_without_unsigned_conversion() {
    for seconds in [
        i64::MIN,
        -3601,
        -3600,
        -61,
        -60,
        -1,
        0,
        1,
        60,
        3600,
        i64::MAX,
    ] {
        for nanos in [0, 1, 10_000, 999_999_999] {
            let duration = DurationValue::new(seconds, nanos).unwrap();
            assert_eq!(
                duration.to_string().parse::<DurationValue>().unwrap(),
                duration,
                "{duration}"
            );
        }
    }
}

#[test]
fn literal_context_preserves_text_and_utf8_bytes_without_inference() {
    assert_eq!(
        literals::read("123", &Shape::Unknown),
        Some(Data::Text("123".into()))
    );
    assert_eq!(
        literals::read("é😀", &Shape::Primitive(Primitive::Bytes)),
        Some(Data::Bytes("é😀".as_bytes().to_vec().into()))
    );
    assert!(
        literals::read(
            "[1,2]",
            &Shape::List(Box::new(Shape::Primitive(Primitive::Int)))
        )
        .is_none()
    );
}
