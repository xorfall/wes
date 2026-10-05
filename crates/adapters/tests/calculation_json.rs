use wes_adapters::codec::CalculationServices;
use wes_core::{Data, contracts::ContractRegistry};
use wes_engine::{calc::LocalServices, driver::CancellationToken};
use wes_language::Span;
#[test]
fn explicit_json_boundary_preserves_missing_null_and_values_without_implicit_native_coercion() {
    let mut registry = ContractRegistry::new();
    registry.load("types:\n  Row:\n    base: Record\n    fields:\n      required: Option<Int>\n      absent: {type: Option<Int>, optional: true}\n").unwrap();
    let contract = registry.resolve("Row").unwrap();
    let reader = CalculationServices;
    let token = CancellationToken::new();
    for (json, expected) in [
        ("{\"required\":null}", Data::Option(None)),
        (
            "{\"required\":0}",
            Data::Option(Some(Box::new(Data::Int(0)))),
        ),
    ] {
        let value = reader
            .read(json, Some(&contract), &token, Span::at(0))
            .unwrap();
        let Data::Record(fields) = value.data() else {
            panic!()
        };
        assert_eq!(fields["required"], expected);
        assert!(!fields.contains_key("absent"));
        assert_eq!(value.shape().to_string(), "Row");
    }
    for json in [
        "{}",
        "{\"required\":false}",
        "{\"required\":\"0\"}",
        "{\"required\":0.0}",
        "{\"required\":0,\"required\":1}",
    ] {
        assert!(
            reader
                .read(json, Some(&contract), &token, Span::at(0))
                .is_err(),
            "{json}"
        );
    }
    let raw = reader
        .read("{\"x\":0,\"y\":0.0,\"z\":null}", None, &token, Span::at(0))
        .unwrap();
    let Data::Record(fields) = raw.data() else {
        panic!()
    };
    assert!(matches!(fields["x"], Data::Int(0)));
    assert!(matches!(fields["y"], Data::Decimal(_)));
    assert_eq!(fields["z"], Data::Option(None));
    token.cancel();
    assert!(
        reader
            .read("{}", None, &token, Span::at(0))
            .unwrap_err()
            .cancelled
    );
}

#[tokio::test]
async fn typed_http_definitions_execute_actual_file_as_pure_local_work() {
    use std::{collections::VecDeque, sync::Arc, time::Duration};
    use wes_engine::{
        driver::Executor,
        runtime::{Effect, Outcome},
        tasks::TaskExecutor,
        workspace::{Preparation, Workspace},
    };
    use wes_language::{SourceText, parse};
    let mut w = Workspace::local(wes_engine::providers::LocalScope::new("fixture").unwrap())
        .with_calculation_services(Arc::new(CalculationServices));
    let parsed = parse(&SourceText::new(
        "http.wes",
        include_str!("../../../examples/typed-presentations/http.wes"),
    ));
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    for statement in parsed.script.statements {
        let Preparation::Change(p) = w.prepare(&statement).unwrap() else {
            panic!()
        };
        w.commit(p).unwrap();
    }
    let mut queue = VecDeque::from(w.start(Duration::ZERO));
    while let Some(effect) = queue.pop_front() {
        if let Effect::Spawn(ticket) = effect {
            let run = ticket.run.clone();
            let ticket = w.enter_ticket(ticket).unwrap().unwrap();
            let report = TaskExecutor::ephemeral()
                .execute(ticket, CancellationToken::new())
                .await;
            assert!(
                matches!(report.outcome, Outcome::Produced(_)),
                "{:?}",
                report.outcome
            );
            queue.extend(w.complete(&run, report.outcome, Duration::ZERO));
        }
    }
    let data = |name: &str| {
        w.runtime()
            .value_of(&w.resolve(name).unwrap().node)
            .unwrap()
            .data()
    };
    let Data::Record(status) = data("status") else {
        panic!()
    };
    assert_eq!(status["code"], Data::Int(404));
    let Data::Record(definition) = data("analysis_definition") else {
        panic!()
    };
    assert_eq!(definition["purity"], Data::Text("pure".into()));
    assert_eq!(definition["conversionEligible"], Data::Bool(true));
    let Data::Record(analysis) = data("analysis") else {
        panic!()
    };
    assert_eq!(analysis["executionState"], Data::Text("cancelled".into()));
}

#[test]
fn explicit_temporal_json_contracts_validate_endpoints_without_native_coercion() {
    let registry = ContractRegistry::new();
    let contract = registry.resolve("Interval").unwrap();
    let reader = CalculationServices;
    let token = CancellationToken::new();
    let raw = "\"2025-01-01T00:00:00Z/2025-01-01T00:00:00Z\"";
    let value = reader
        .read(raw, Some(&contract), &token, Span::at(0))
        .unwrap();
    assert!(matches!(value.data(), Data::Interval(_)));
    assert!(contract.issues(value.data()).is_empty());
    let plain = reader.read(raw, None, &token, Span::at(0)).unwrap();
    assert!(matches!(plain.data(), Data::Text(_)));
    assert!(!contract.issues(plain.data()).is_empty());
    for invalid in [
        "\"2025-01-02T00:00:00Z/2025-01-01T00:00:00Z\"",
        "{}",
        "0",
        "null",
    ] {
        assert!(
            reader
                .read(invalid, Some(&contract), &token, Span::at(0))
                .is_err(),
            "{invalid}"
        );
    }
}
#[test]
fn json_syntax_positions_are_distinct_from_resource_limits_and_cancellation() {
    let reader = CalculationServices;
    let token = CancellationToken::new();
    let error = reader
        .read("{\n  \"amount\": }", None, &token, Span::at(10))
        .unwrap_err();
    assert!(
        error.message.contains("line 2") && error.message.contains("column"),
        "{}",
        error.message
    );
    assert!(error.message.contains("expected value"));
    assert_eq!(error.span, Span::at(10));
    let large = format!("\"{}\"", "x".repeat(1024 * 1024));
    let error = reader.read(&large, None, &token, Span::at(0)).unwrap_err();
    assert!(error.message.contains("byte budget"));
    assert!(!error.message.contains("invalid JSON"));
    token.cancel();
    assert!(
        reader
            .read("{", None, &token, Span::at(0))
            .unwrap_err()
            .cancelled
    );
}
