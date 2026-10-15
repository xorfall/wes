use super::*;
use serde_json::{Value as Json, json};
use wes_core::flow::FlowPolicy;

fn value(data: Json) -> Value {
    Value::new(
        Shape::Unknown,
        crate::codec::decode_json_preserving(
            &serde_json::to_vec(&data).unwrap(),
            Default::default(),
        )
        .unwrap(),
        Provenance::default(),
    )
    .unwrap()
}
fn render(view: HttpFunction, input: &Value) -> Result<Value, LocalError> {
    view.evaluate(input, &CancellationToken::new())
}
fn json_data(value: &Value) -> Json {
    serde_json::from_slice(&crate::codec::encode_json(value.data(), Default::default()).unwrap())
        .unwrap()
}
#[test]
fn dictionary_has_registered_extension_unused_obsolete_and_temporary_definitions() {
    for (code, class, registration) in [
        (200, "success", "registered"),
        (404, "client-error", "registered"),
        (499, "client-error", "unassigned"),
        (427, "client-error", "unassigned"),
        (430, "client-error", "unassigned"),
        (509, "server-error", "unassigned"),
        (104, "informational", "temporary"),
        (418, "client-error", "unused"),
        (510, "server-error", "obsolete"),
        (599, "server-error", "unassigned"),
    ] {
        let actual = json_data(&render(HttpFunction::Status, &value(json!(code))).unwrap());
        assert_eq!(actual["code"], code);
        assert_eq!(actual["class"], class);
        assert_eq!(actual["registration"], registration);
        assert_eq!(
            actual,
            json_data(&render(HttpFunction::Status, &text(code.to_string())).unwrap())
        );
    }
    for input in [
        json!(99),
        json!(600),
        json!(200.5),
        json!(true),
        json!("0200"),
        json!("+200"),
        json!({"code":200}),
    ] {
        assert!(render(HttpFunction::Status, &value(input)).is_err());
    }
    let statuses = render(HttpFunction::Catalogue, &text("statuses")).unwrap();
    assert!(
        matches!(statuses.shape(), Shape::List(item) if matches!(item.as_ref(), Shape::Record(_)))
    );
    let rows = json_data(&statuses);
    assert_eq!(rows.as_array().unwrap().len(), 64);
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .all(|row| row["registration"] != "unassigned")
    );
    let errors = json_data(&render(HttpFunction::Catalogue, &text("errors")).unwrap());
    assert_eq!(errors.as_array().unwrap().len(), 12);
    assert_eq!(
        json_data(&render(HttpFunction::Error, &text("ENV021")).unwrap())["known"],
        false
    );
    assert!(render(HttpFunction::Catalogue, &text("missing")).is_err());
}
#[test]
fn catalogue_covers_actual_adapter_failures_without_message_parsing() {
    use crate::http::Failure;
    let failures = [
        Failure::Request("synthetic"),
        Failure::Credential("synthetic".into()),
        Failure::CredentialLookup,
        Failure::Transport,
        Failure::Timeout,
        Failure::Redirect,
        Failure::Size,
        Failure::Response,
        Failure::Status {
            status: 422,
            excerpt: "sensitive fixture body".into(),
        },
        Failure::Internal,
    ];
    for failure in failures {
        let wes_engine::providers::InvocationError::Failed(error) = failure.error() else {
            panic!("failure")
        };
        let actual = json_data(&render(HttpFunction::Error, &error.to_value()).unwrap());
        assert_eq!(actual["code"], error.code());
        assert_eq!(actual["known"], true);
        for issue in error.issues() {
            assert_eq!(
                json_data(&render(HttpFunction::Error, &text(&issue.code)).unwrap())["known"],
                true
            );
        }
        assert!(!actual.to_string().contains("sensitive fixture body"));
    }
}
#[test]
fn definition_work_limits_cancellation_and_private_labels_remain_enforced() {
    let huge = value(json!("x".repeat(128 * 1024 + 1)));
    let Err(LocalError::Failed(error)) = render(HttpFunction::Status, &huge) else {
        panic!("size refusal")
    };
    assert_eq!(error.code(), "HTTPD002");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        HttpFunction::Catalogue.evaluate(&text("statuses"), &cancelled),
        Err(LocalError::Cancelled)
    ));
    for policy in [
        FlowPolicy::default().private(),
        FlowPolicy::default().unknown(),
    ] {
        let input = text("404").with_provenance(Provenance::default().with_policy(&policy));
        assert_eq!(
            render(HttpFunction::Status, &input)
                .unwrap()
                .provenance()
                .policy(),
            &policy
        );
        let bad = text("invalid").with_provenance(Provenance::default().with_policy(&policy));
        let Err(LocalError::Failed(error)) = render(HttpFunction::Status, &bad) else {
            panic!("invalid")
        };
        assert_eq!(error.policy(), &policy);
    }
}
