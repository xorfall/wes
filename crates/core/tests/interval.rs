use wes_core::{
    Data, Decimal, DurationValue, Interval, Primitive, Shape, Timestamp,
    contracts::ContractRegistry, literals,
};

#[test]
fn interval_is_finite_half_open_and_validated_at_every_constructor() {
    let start: Timestamp = "2025-01-01T00:00:00Z".parse().unwrap();
    let end = start.add("PT1S".parse().unwrap()).unwrap();
    let range = Interval::new(start, end).unwrap();
    assert!(range.contains(start));
    assert!(!range.contains(end));
    assert!(!Interval::new(start, start).unwrap().contains(start));
    assert!(Interval::new(end, start).is_err());
    assert_eq!(range.to_string().parse::<Interval>().unwrap(), range);
    assert_eq!(range.duration().unwrap().to_string(), "PT1S");
    let shape = Shape::Primitive(Primitive::Interval);
    assert_eq!(
        literals::read(&range.to_string(), &shape),
        Some(Data::Interval(range))
    );
    for bad in [
        "2025-01-02T00:00:00Z/2025-01-01T00:00:00Z",
        "../2025-01-01T00:00:00Z",
        "2025-01-01T00:00:00Z/PT1S",
    ] {
        assert!(bad.parse::<Interval>().is_err());
        assert!(literals::read(bad, &shape).is_none());
    }
    let registry = ContractRegistry::new();
    let contract = registry.resolve("Interval").unwrap();
    assert!(contract.issues(&Data::Interval(range)).is_empty());
    assert!(
        !contract
            .issues(&Data::Record(Default::default()))
            .is_empty()
    );
}
#[test]
fn exact_temporal_arithmetic_normalizes_negative_epoch_and_checks_bounds() {
    let zero = Timestamp::from_nanos(0).unwrap();
    let before = Timestamp::from_nanos(-1).unwrap();
    assert_eq!(before.to_string(), "1969-12-31T23:59:59.999999999Z");
    assert_eq!(before.since(zero).unwrap().to_string(), "-PT0.000000001S");
    assert_eq!(
        before.add(DurationValue::from_nanos(1).unwrap()).unwrap(),
        zero
    );
    assert_eq!(
        "2025-01-01T03:00:00+03:00".parse::<Timestamp>().unwrap(),
        "2025-01-01T00:00:00Z".parse().unwrap()
    );
    let max: Timestamp = "+1000000000-12-31T23:59:59.999999999Z".parse().unwrap();
    assert!(max.add(DurationValue::from_nanos(1).unwrap()).is_err());
    assert!(
        DurationValue::new(i64::MAX, 999999999)
            .unwrap()
            .add(DurationValue::from_nanos(1).unwrap())
            .is_err()
    );
    assert!(Interval::around(zero, DurationValue::from_nanos(0).unwrap()).is_err());
    assert!(Interval::around(zero, DurationValue::from_nanos(-1).unwrap()).is_err());
    assert!(Interval::around(max, DurationValue::from_nanos(1).unwrap()).is_err());
}
#[test]
fn scaled_decimal_epoch_is_exact_and_bounded() {
    for (raw, places, expected) in [
        ("-0.000000001", 9, Some(-1)),
        ("1.2300", 9, Some(1230000000)),
        ("1e1000000", 0, None),
        ("1e-1000000", 9, None),
        ("0e1000000", 9, Some(0)),
        ("0.0000000001", 9, None),
    ] {
        assert_eq!(
            raw.parse::<Decimal>().unwrap().exact_scaled_i128(places),
            expected,
            "{raw}"
        );
    }
}

#[test]
fn utc_calendar_parts_and_negative_duration_canonicalization_are_exact() {
    for (source, expected) in [
        (
            "2000-02-29T23:59:58.000000001Z",
            [2000, 2, 29, 23, 59, 58, 1, 2],
        ),
        ("1969-12-31T23:00:00-01:00", [1970, 1, 1, 0, 0, 0, 0, 4]),
        ("0000-02-29T00:00:00Z", [0, 2, 29, 0, 0, 0, 0, 2]),
    ] {
        assert_eq!(source.parse::<Timestamp>().unwrap().utc_parts(), expected);
    }
    for (input, canonical) in [
        ("PT-1M-5S", "-PT1M5S"),
        ("-PT0.000000001S", "-PT0.000000001S"),
        ("-PT0S", "PT0S"),
        ("P-1DT1H", "-PT23H"),
    ] {
        let d: DurationValue = input.parse().unwrap();
        assert_eq!(d.to_string(), canonical);
        assert_eq!(canonical.parse::<DurationValue>().unwrap(), d);
    }
    let smallest = DurationValue::new(i64::MIN, 0).unwrap();
    assert_eq!(
        smallest.to_string().parse::<DurationValue>().unwrap(),
        smallest
    );
}
