use wes_core::{Data, ErrorId, ErrorValue, ValidationIssue, contracts::ContractRegistry};

#[test]
fn error_identity_is_shared_by_observers_and_cancellation_is_a_distinct_named_type() {
    let error = ErrorValue::new(
        ErrorId::new("error-1").unwrap(),
        "RUN001",
        "operation failed",
        vec![],
        None,
    )
    .unwrap();
    let observation = error.clone();
    assert_eq!(observation.id(), error.id());
    assert_eq!(error.to_value().shape().to_string(), "Error");
    assert_eq!(
        error.to_cancellation_value().shape().to_string(),
        "Cancellation"
    );
    let registry = ContractRegistry::new();
    assert!(
        registry
            .resolve("Error")
            .unwrap()
            .issues(error.to_value().data())
            .is_empty()
    );
    assert!(
        registry
            .resolve("Cancellation")
            .unwrap()
            .issues(error.to_cancellation_value().data())
            .is_empty()
    );
    let Data::Record(fields) = error.to_value().data().clone() else {
        panic!("record")
    };
    assert_eq!(fields["causeId"], Data::Text(String::new().into()));
}

#[test]
fn causes_reference_identity_without_copying_other_payloads() {
    let root = ErrorId::new("root").unwrap();
    let id = ErrorId::new("dependent").unwrap();
    let error = ErrorValue::new(
        id,
        "RUN004",
        "upstream failed",
        vec![ValidationIssue {
            path: "/input".into(),
            code: "CUSTOM123".into(),
            message: "missing".into(),
        }],
        Some(root.clone()),
    )
    .unwrap();
    assert_eq!(error.cause(), Some(&root));
    assert_eq!(error.issues()[0].code, "CUSTOM123");
    assert!(ErrorValue::new(root.clone(), "RUN004", "", vec![], Some(root)).is_err());
}

#[test]
fn invalid_portable_errors_are_refused_at_construction() {
    assert!(ErrorId::new(" ").is_err());
    assert!(ErrorValue::new(ErrorId::new("x").unwrap(), "", "", vec![], None).is_err());
    assert!(
        ErrorValue::new(
            ErrorId::new("x").unwrap(),
            "X",
            "",
            vec![ValidationIssue {
                path: "not-a-pointer".into(),
                code: "X".into(),
                message: String::new()
            }],
            None
        )
        .is_err()
    );
}

#[test]
fn source_locations_are_structured_validated_and_removed_for_private_errors() {
    let location = wes_core::SourceLocation {
        source: "fixture.wes".into(),
        start: 4,
        end: 8,
        line: 2,
        column: 1,
        end_line: 2,
        end_column: 5,
    };
    let error = ErrorValue::new(
        ErrorId::new("located").unwrap(),
        "CAL005",
        "division by zero",
        vec![],
        None,
    )
    .unwrap()
    .with_locations(vec![location.clone()])
    .unwrap();
    let value = error.to_value();
    let Data::Record(fields) = value.data() else {
        panic!("error")
    };
    assert_eq!(fields["locations"], Data::List(vec![location.data()]));
    let mut invalid = location.clone();
    invalid.line = 0;
    assert!(error.clone().with_locations(vec![invalid]).is_err());
    let private = error.with_policy(&wes_core::flow::FlowPolicy::default().private());
    assert!(private.locations().is_empty());
    assert!(
        private
            .with_locations(vec![location])
            .unwrap()
            .locations()
            .is_empty()
    );
}
