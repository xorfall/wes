use sha2::{Digest, Sha256};
use wes_adapters::datasets::PositionUnit;
use wes_adapters::datasets::{
    FormatError, FormatLimits, Record, SegmentHeader, SegmentReader, SourceRange, encode_segment,
};
use wes_core::{
    Data, Primitive, Provenance, Shape, Value,
    contracts::{ContractRegistry, ResolvedContractBundle, SnapshotLimits},
};

const STORE: &str = "bda44444-4444-4444-8444-444444444444";
const DATASET: &str = "bda55555-5555-4555-8555-555555555555";
fn schema(name: &str) -> ResolvedContractBundle {
    let mut registry = ContractRegistry::new();
    registry.load("version: 2\ntypes: {Small: {base: Int, min: 0, max: 10}, Tone: {base: Text, enum: [ok, error], display: {enumTones: {ok: ok, error: bad}}}}").unwrap();
    ResolvedContractBundle::capture(registry.resolve(name).unwrap(), SnapshotLimits::default())
        .unwrap()
}
fn header(schema: &ResolvedContractBundle, count: u64) -> SegmentHeader {
    SegmentHeader {
        store: STORE.into(),
        dataset: DATASET.into(),
        schema: schema.digest().into(),
        first: 7,
        count,
        source: SourceRange {
            identity: "synthetic-source-revision-1".into(),
            unit: PositionUnit::Bytes,
            start: 0,
            end: 100,
        },
    }
}
fn record(n: i64) -> Record {
    Record {
        source_start: n as u64 * 2,
        source_end: n as u64 * 2 + 2,
        value: Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(n),
            Provenance::default(),
        )
        .unwrap(),
    }
}
#[test]
fn indexed_ordinals_raw_positions_and_captured_metadata_roundtrip() {
    let schema = schema("Small");
    let records = [record(1), record(2)];
    let h = header(&schema, 2);
    let bytes = encode_segment(&h, &records, &schema, FormatLimits::default()).unwrap();
    let reader =
        SegmentReader::open(&bytes, STORE, DATASET, &schema, FormatLimits::default()).unwrap();
    assert_eq!(reader.header(), &h);
    assert!(reader.row(6).unwrap().is_none());
    assert!(reader.row(9).unwrap().is_none());
    for (i, n) in [1, 2].into_iter().enumerate() {
        let row = reader.row(7 + i as u64).unwrap().unwrap();
        assert_eq!(row.value.data(), &Data::Int(n));
        assert_eq!(
            (row.source_start, row.source_end),
            (n as u64 * 2, n as u64 * 2 + 2)
        );
        assert!(row.value.metadata().is_some());
    }
    assert_eq!(
        bytes,
        encode_segment(&h, &records, &schema, FormatLimits::default()).unwrap()
    );
}
#[test]
fn exact_native_scalars_and_optional_nested_values_use_retained_codec() {
    for (name, data) in [
        (
            "Decimal",
            Data::Decimal("9223372036854775808.0100".parse().unwrap()),
        ),
        ("Bytes", Data::Bytes(vec![0, 255, 42].into())),
        (
            "Instant",
            Data::Instant("2026-10-06T01:02:03.123456789Z".parse().unwrap()),
        ),
        ("Duration", Data::Duration("PT1.5S".parse().unwrap())),
        (
            "Option<List<Int>>",
            Data::Option(Some(Box::new(Data::List(vec![Data::Int(i64::MAX)])))),
        ),
    ] {
        let schema = schema(name);
        let r = Record {
            source_start: 0,
            source_end: 1,
            value: Value::new(schema.root().shape(), data.clone(), Provenance::default()).unwrap(),
        };
        let bytes =
            encode_segment(&header(&schema, 1), &[r], &schema, FormatLimits::default()).unwrap();
        let reader =
            SegmentReader::open(&bytes, STORE, DATASET, &schema, FormatLimits::default()).unwrap();
        assert_eq!(reader.row(7).unwrap().unwrap().value.data(), &data);
    }
}
#[test]
fn private_unknown_policy_contract_violations_and_noninline_schemas_are_refused() {
    let schema = schema("Small");
    for policy in [
        wes_core::flow::FlowPolicy::default().private(),
        wes_core::flow::FlowPolicy::default().unknown(),
    ] {
        let mut row = record(1);
        row.value = row
            .value
            .with_provenance(Provenance::default().with_policy(&policy));
        assert!(matches!(
            encode_segment(
                &header(&schema, 1),
                &[row],
                &schema,
                FormatLimits::default()
            ),
            Err(FormatError::Restricted)
        ));
    }
    assert!(matches!(
        encode_segment(
            &header(&schema, 1),
            &[record(11)],
            &schema,
            FormatLimits::default()
        ),
        Err(FormatError::Contract)
    ));
    let iterator_schema = self::schema("Option<Iter<Int>>");
    assert!(matches!(
        encode_segment(
            &header(&iterator_schema, 0),
            &[],
            &iterator_schema,
            FormatLimits::default()
        ),
        Err(FormatError::NonInline)
    ));
    let unknown = self::schema("Unknown");
    let hidden = Record {
        source_start: 0,
        source_end: 0,
        value: Value::new(
            Shape::Option(Box::new(Shape::Iter(Box::new(Shape::Primitive(
                Primitive::Int,
            ))))),
            Data::Option(None),
            Provenance::default(),
        )
        .unwrap(),
    };
    assert!(matches!(
        encode_segment(
            &header(&unknown, 1),
            &[hidden],
            &unknown,
            FormatLimits::default()
        ),
        Err(FormatError::NonInline)
    ));
}
#[test]
fn every_truncation_corruption_bad_version_and_foreign_identity_refuses() {
    let schema = schema("Small");
    let bytes = encode_segment(
        &header(&schema, 2),
        &[record(1), record(2)],
        &schema,
        FormatLimits::default(),
    )
    .unwrap();
    for end in 0..bytes.len() {
        assert!(
            SegmentReader::open(
                &bytes[..end],
                STORE,
                DATASET,
                &schema,
                FormatLimits::default()
            )
            .is_err(),
            "cut {end}"
        );
    }
    for offset in [0, 15, 100, bytes.len() - 1] {
        let mut changed = bytes.clone();
        changed[offset] ^= 1;
        assert!(
            SegmentReader::open(&changed, STORE, DATASET, &schema, FormatLimits::default())
                .is_err(),
            "offset {offset}"
        );
    }
    let mut bad = bytes.clone();
    bad[8] = 2;
    assert!(matches!(
        SegmentReader::open(&bad, STORE, DATASET, &schema, FormatLimits::default()),
        Err(FormatError::Version)
    ));
    assert!(SegmentReader::open(&bytes, DATASET, STORE, &schema, FormatLimits::default()).is_err());
    assert!(
        SegmentReader::open(
            &bytes,
            STORE,
            DATASET,
            &self::schema("Int"),
            FormatLimits::default()
        )
        .is_err()
    );
}
#[test]
fn lengths_counts_spans_and_aggregate_validation_are_bounded() {
    let schema = schema("Small");
    let h = header(&schema, 2);
    let rows = [record(1), record(2)];
    for limits in [
        FormatLimits {
            records: 1,
            ..Default::default()
        },
        FormatLimits {
            record_bytes: 1,
            ..Default::default()
        },
        FormatLimits {
            segment_bytes: 64,
            ..Default::default()
        },
        FormatLimits {
            validation_work: 1,
            ..Default::default()
        },
    ] {
        assert!(encode_segment(&h, &rows, &schema, limits).is_err());
    }
    let mut backwards = record(2);
    backwards.source_start = 1;
    assert!(
        encode_segment(
            &h,
            &[record(1), backwards],
            &schema,
            FormatLimits::default()
        )
        .is_err()
    );
    let mut wrong = h.clone();
    wrong.count = 1;
    assert!(encode_segment(&wrong, &rows, &schema, FormatLimits::default()).is_err());
    let bytes = encode_segment(&h, &rows, &schema, FormatLimits::default()).unwrap();
    let mut changed = bytes.clone();
    let length = u32::from_le_bytes(changed[12..16].try_into().unwrap()) as usize;
    let frame = 16 + length;
    changed[frame + 24..frame + 28].copy_from_slice(&u32::MAX.to_le_bytes());
    let end = changed.len() - 32;
    let digest = Sha256::digest(&changed[..end]);
    changed[end..].copy_from_slice(&digest);
    assert!(matches!(
        SegmentReader::open(&changed, STORE, DATASET, &schema, FormatLimits::default()),
        Err(FormatError::Limit("row"))
    ));
}

#[test]
fn recomputing_checksums_cannot_bypass_schema_or_policy_validation() {
    let schema = schema("Small");
    let bytes = encode_segment(
        &header(&schema, 1),
        &[record(1)],
        &schema,
        FormatLimits::default(),
    )
    .unwrap();
    let header_len = u32::from_le_bytes(bytes[12..16].try_into().unwrap()) as usize;
    let frame = 16 + header_len;
    let start = frame + 28;
    let length = u32::from_le_bytes(bytes[frame + 24..start].try_into().unwrap()) as usize;
    let original: serde_json::Value =
        serde_json::from_slice(&bytes[start..start + length]).unwrap();
    for violation in 0..2 {
        let mut value = original.clone();
        if violation == 0 {
            value["data"]["value"] = serde_json::json!("11");
        } else {
            value["policy"]["unknown"] = serde_json::json!(true);
        }
        let payload = serde_json::to_vec(&value).unwrap();
        let mut forged = bytes[..frame].to_vec();
        let mut prefix = bytes[frame..start].to_vec();
        prefix[24..28].copy_from_slice(&(payload.len() as u32).to_le_bytes());
        let mut checksum = Sha256::new();
        checksum.update(&prefix);
        checksum.update(&payload);
        forged.extend_from_slice(&prefix);
        forged.extend_from_slice(&payload);
        forged.extend_from_slice(&checksum.finalize());
        let total = forged.len() + 56;
        forged.extend_from_slice(b"WESEND01");
        forged.extend_from_slice(&1u64.to_le_bytes());
        forged.extend_from_slice(&(total as u64).to_le_bytes());
        let checksum = Sha256::digest(&forged);
        forged.extend_from_slice(&checksum);
        let result = SegmentReader::open(&forged, STORE, DATASET, &schema, FormatLimits::default());
        if violation == 0 {
            assert!(matches!(result, Err(FormatError::Contract)));
        } else {
            assert!(matches!(result, Err(FormatError::Restricted)));
        }
    }
}
