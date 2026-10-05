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
fn event(kind: &str, details: Json) -> Json {
    json!({"kind":kind,"elapsedMs":0,"details":details})
}
fn head(status: i64) -> Json {
    event(
        "http.response",
        json!({"status":status,"version":"HTTP/1.1","url":"http://fixture/","headerCount":0,"headers":[]}),
    )
}
fn finished(state: &str, code: &str) -> Json {
    event("execution.finished", json!({"state":state,"code":code}))
}
fn trace(state: &str, events: Vec<Json>) -> Json {
    json!({"schema":1,"profile":"http","node":"id1000","run":"fixture-run","state":state,"persistence":"memory","dropped":0,"events":events})
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
fn status_and_execution_failure_remain_distinct_and_findings_link_to_events() {
    for (status, code, expected) in [
        (404, "HTTP008", "HTTPA_STATUS_REJECTED"),
        (422, "HTTP008", "HTTPA_STATUS_REJECTED"),
        (200, "HTTP007", "HTTPA_RESPONSE_REJECTED"),
    ] {
        let input = trace("failed", vec![head(status), finished("failed", code)]);
        let actual = json_data(&render(HttpFunction::Analysis, &value(input)).unwrap());
        assert_eq!(actual["statuses"][0]["code"], status);
        assert_eq!(actual["errors"][0]["code"], code);
        assert_eq!(actual["findings"][1]["code"], expected);
        assert_eq!(actual["findings"][1]["eventIndexes"], json!([0, 1]));
        assert_eq!(actual["node"], "id1000");
        assert_eq!(actual["run"], "fixture-run");
    }
    let direct = json_data(
        &render(
            HttpFunction::Analysis,
            &value(trace(
                "completed",
                vec![head(404), finished("completed", "")],
            )),
        )
        .unwrap(),
    );
    assert_eq!(direct["executionState"], "completed");
    assert!(direct["errors"].as_array().unwrap().is_empty());
    for (code, finding) in [
        ("HTTP003", "HTTPA_TRANSPORT_FAILED"),
        ("HTTP004", "HTTPA_TIMEOUT"),
        ("ENV021", "HTTPA_EXECUTION_FAILED"),
    ] {
        let actual = json_data(
            &render(
                HttpFunction::Analysis,
                &value(trace("failed", vec![finished("failed", code)])),
            )
            .unwrap(),
        );
        assert!(actual["statuses"].as_array().unwrap().is_empty());
        assert_eq!(actual["findings"][0]["code"], finding);
        assert_eq!(actual["findings"][1]["code"], "HTTPA_RESPONSE_UNOBSERVED");
    }
}
#[test]
fn live_cancelled_truncated_dropped_and_future_events_do_not_claim_complete_evidence() {
    let mut input = trace(
        "running",
        vec![
            head(200),
            event(
                "http.body",
                json!({"bytes":5000,"preview":"BODY_CANARY","complete":false}),
            ),
            event("future.event", json!({"secret":"HEADER_CANARY"})),
        ],
    );
    input["dropped"] = json!(3);
    let actual = json_data(&render(HttpFunction::Analysis, &value(input)).unwrap());
    assert_eq!(actual["partial"], true);
    let codes: Vec<_> = actual["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(
        codes.contains(&"HTTPA_RUNNING")
            && codes.contains(&"HTTPA_DROPPED")
            && codes.contains(&"HTTPA_PREVIEW_TRUNCATED")
            && codes.contains(&"HTTPA_UNSUPPORTED_EVENTS")
    );
    assert!(!actual.to_string().contains("CANARY"));
    let cancelled =
        json_data(&render(HttpFunction::Analysis, &value(trace("cancelled", vec![]))).unwrap());
    assert_eq!(cancelled["partial"], true);
    assert!(
        cancelled["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "HTTPA_CANCELLED")
    );
    // A snapshot can race between emission of execution.finished and the state update.
    assert!(
        render(
            HttpFunction::Analysis,
            &value(trace("running", vec![finished("completed", "")]))
        )
        .is_ok()
    );
}
#[test]
fn malformed_excessive_cancelled_and_private_inputs_fail_closed() {
    let base = trace("completed", vec![head(200), finished("completed", "")]);
    let mut invalids = vec![json!(true)];
    for (key, bad) in [
        ("schema", json!(2)),
        ("profile", json!("tcp")),
        ("dropped", json!(-1)),
        ("node", json!("")),
        ("events", json!([{}, {}])),
    ] {
        let mut x = base.clone();
        x[key] = bad;
        invalids.push(x);
    }
    invalids.push(trace("failed", vec![finished("completed", "")]));
    invalids.push(trace("failed", vec![finished("failed", "")]));
    invalids.push(trace(
        "completed",
        vec![finished("completed", ""), head(200)],
    ));
    invalids.push(trace("completed", vec![head(200), head(201)]));
    invalids.push(trace("running", vec![head(200); 65]));
    let mut backwards = base.clone();
    backwards["events"][0]["elapsedMs"] = json!(5);
    invalids.push(backwards);
    for invalid in invalids {
        assert!(render(HttpFunction::Analysis, &value(invalid)).is_err());
    }
    let huge = value(json!("x".repeat(128 * 1024 + 1)));
    let Err(LocalError::Failed(error)) = render(HttpFunction::Analysis, &huge) else {
        panic!("size refusal")
    };
    assert_eq!(error.code(), "HTTPD002");
    let cancelled = CancellationToken::new();
    cancelled.cancel();
    assert!(matches!(
        HttpFunction::Analysis.evaluate(&value(base.clone()), &cancelled),
        Err(LocalError::Cancelled)
    ));
    for policy in [
        FlowPolicy::default().private(),
        FlowPolicy::default().unknown(),
    ] {
        let input = value(base.clone()).with_provenance(Provenance::default().with_policy(&policy));
        assert_eq!(
            render(HttpFunction::Analysis, &input)
                .unwrap()
                .provenance()
                .policy(),
            &policy
        );
        let input = text("404").with_provenance(Provenance::default().with_policy(&policy));
        assert_eq!(
            render(HttpFunction::Status, &input)
                .unwrap()
                .provenance()
                .policy(),
            &policy
        );
        let input = text("invalid").with_provenance(Provenance::default().with_policy(&policy));
        let Err(LocalError::Failed(error)) = render(HttpFunction::Status, &input) else {
            panic!("invalid")
        };
        assert_eq!(error.policy(), &policy);
    }
}
