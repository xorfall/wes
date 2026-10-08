use std::sync::Arc;
use wes_core::{
    Data, Provenance, Shape, Value, capability::Catalogue, contracts::ContractRegistry,
};
use wes_engine::{
    calc::{Limits, Machine, Step},
    driver::CancellationToken,
};
use wes_language::calc::{self, Package};

fn machine(source: &str, limits: Limits) -> Result<Machine, String> {
    let program =
        calc::parse_body(source, 0, Package::standard()).map_err(|e| e.code.to_string())?;
    let compiled = calc::analyze(
        Arc::new(program),
        calc::Environment {
            catalogue: &Catalogue::new(),
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
    )
    .map_err(|e| e.code.to_string())?;
    Machine::new(Arc::new(compiled), Default::default(), limits).map_err(|e| e.code.into())
}
fn evaluate(source: &str) -> Result<Value, String> {
    let mut m = machine(source, Limits::default())?;
    loop {
        match m
            .poll(&CancellationToken::new())
            .map_err(|e| e.code.to_owned())?
        {
            Step::Yield => {}
            Step::Complete(v) => return Ok(v),
            Step::Request(_) => panic!("pure case requested a host boundary"),
        }
    }
}

#[test]
fn deferred_conformance_manifest_executes_with_exact_expected_values_and_errors() {
    // This manifest is parsed by the test only; no source generation or live providers.
    let cases: serde_json::Value = serde_json::from_str(include_str!(
        "../../../tests/fixtures/calc-conformance.json"
    ))
    .unwrap();
    for case in cases.as_array().unwrap() {
        let result = evaluate(case["body"].as_str().unwrap());
        if let Some(code) = case["error"].as_str() {
            assert_eq!(result.unwrap_err(), code, "{}", case["id"]);
        } else {
            let value = result.unwrap_or_else(|e| panic!("{}: {e}", case["id"]));
            assert_eq!(fingerprint(value.data()), case["expect"], "{}", case["id"]);
        }
    }
}
fn fingerprint(data: &Data) -> serde_json::Value {
    use serde_json::json;
    match data {
        Data::Int(n) => json!({"kind":"int","value":n.to_string()}),
        Data::Decimal(n) => json!({"kind":"decimal","value":n.to_string()}),
        Data::Bool(v) => json!({"kind":"bool","value":v}),
        Data::Option(value) => json!({"kind":"option","value":value.as_deref().map(fingerprint)}),
        _ => panic!("manifest kind"),
    }
}

#[test]
fn closures_loop_control_unicode_empty_collections_and_rounding_are_defined() {
    for (source, data) in [
        (
            "let fs=[]; let sum=0; for(const x of [1,2,3,4]) { if(x==2) continue; if(x==4) break; sum=sum+x; } return sum;",
            Data::Int(4),
        ),
        (
            "const outer=1; function f(x){ const outer=x; return ()=>outer; } const g=f(7); return g()+outer;",
            Data::Int(8),
        ),
        ("return length('😀a');", Data::Int(2)),
        ("return '😀a'[0];", Data::Text("😀".into())),
        ("return [].reduce((a,x)=>a+x,7);", Data::Int(7)),
        (
            "return roundDiv(1,8,2);",
            Data::Decimal("0.12".parse().unwrap()),
        ),
        (
            "return roundDiv(3,8,2);",
            Data::Decimal("0.38".parse().unwrap()),
        ),
        (
            "return roundDiv(-3,8,2);",
            Data::Decimal("-0.38".parse().unwrap()),
        ),
        ("return div(-7,3);", Data::Int(-2)),
        ("return rem(-7,3);", Data::Int(-1)),
        ("return 1.0==1.00;", Data::Bool(true)),
    ] {
        assert_eq!(evaluate(source).unwrap().data(), &data, "{source}");
    }
}

#[test]
fn resource_limits_and_cancellation_halt_and_do_not_publish_late_success() {
    for source in [
        "while(true) {} return 1;",
        "function f(){return f();} return f();",
        "return range(0,9223372036854775807);",
        "return 1e200000+1.0;",
    ] {
        let mut m = machine(
            source,
            Limits {
                work: 10_000,
                ..Limits::default()
            },
        )
        .unwrap();
        let error = loop {
            match m.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Err(e) => break e,
                _ => panic!("expected refusal"),
            }
        };
        assert_eq!(error.code, "CAL006", "{source}");
        assert!(m.poll(&CancellationToken::new()).is_err());
    }
    let token = CancellationToken::new();
    let mut m = machine("while(true) {} return 1;", Limits::default()).unwrap();
    assert!(matches!(m.poll(&token), Ok(Step::Yield)));
    token.cancel();
    assert!(m.poll(&token).unwrap_err().cancelled);
    assert_eq!(evaluate("let x=x; return x;").unwrap_err(), "CAL013");
    assert_eq!(
        evaluate("return { function:()=>1 };").unwrap_err(),
        "CAL004"
    );
}

#[test]
fn parsed_json_suspends_once_and_rejects_stale_resume() {
    let mut m = machine("return parseJson('{}');", Limits::default()).unwrap();
    let id = loop {
        match m.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Request(r) => break r.id(),
            _ => panic!("request"),
        }
    };
    let value = Value::new(
        Shape::Unknown,
        Data::Record(Default::default()),
        Provenance::default(),
    )
    .unwrap();
    assert!(m.resume(id + 1, Ok(value.clone())).is_err());
    m.resume(id, Ok(value)).unwrap();
    assert!(
        m.resume(
            id,
            Ok(Value::new(Shape::Unknown, Data::Int(1), Provenance::default()).unwrap())
        )
        .is_err()
    );
    loop {
        match m.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Complete(value) => {
                assert_eq!(value.data(), &Data::Record(Default::default()));
                break;
            }
            _ => panic!("duplicate request"),
        }
    }
}

#[test]
fn checked_values_keep_shape_and_workspace_control_provenance() {
    let registry = ContractRegistry::new();
    let program =
        calc::parse_body("return some(check('Int',$input));", 0, Package::standard()).unwrap();
    let compiled = calc::analyze(
        Arc::new(program),
        calc::Environment {
            catalogue: &Catalogue::new(),
            contracts: &registry,
            workspace: &|name| (name == "input").then_some(Shape::Unknown),
        },
    )
    .unwrap();
    let value = Value::new(
        Shape::Unknown,
        Data::Int(7),
        Provenance::default()
            .with_fact("source", "fixture")
            .cautioned(["uncertain".into()]),
    )
    .unwrap();
    let mut machine = Machine::new(
        Arc::new(compiled),
        [("input".into(), value.clone())].into(),
        Limits::default(),
    )
    .unwrap();
    loop {
        match machine.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Complete(result) => {
                assert_eq!(result.shape().to_string(), "Option<Int>");
                assert_eq!(result.provenance(), value.provenance());
                break;
            }
            Step::Request(_) => panic!(),
        }
    }
}

#[test]
fn assignment_before_declaration_does_not_initialize_a_binding() {
    assert_eq!(evaluate("x=1; let x=2; return x;").unwrap_err(), "CAL013");
}

#[test]
fn mutable_function_arity_and_record_callable_fields_follow_runtime_values() {
    assert_eq!(
        evaluate("let f=()=>1; f=x=>x+1; return f(4);")
            .unwrap()
            .data(),
        &Data::Int(5)
    );
    assert_eq!(
        evaluate("const r={map:()=>7}; return r.map();")
            .unwrap()
            .data(),
        &Data::Int(7)
    );
    assert_eq!(
        evaluate("const r={call:()=>9}; return r.call();")
            .unwrap()
            .data(),
        &Data::Int(9)
    );
}

#[test]
fn iter_lazy_consumers_are_independent_and_early_stop_does_not_evaluate_later_items() {
    for (source, expected) in [
        (
            "const xs=iter.items([1,2,3]); return xs.take(1).count()+xs.count();",
            Data::Int(4),
        ),
        (
            "return iter.items([2,0]).map(x=>div(8,x)).take(1).reduce((a,x)=>a+x,0);",
            Data::Int(4),
        ),
        (
            "return iter.items([1,2,3,4]).filter(x=>x>2).take(1).reduce((a,x)=>a+x,0);",
            Data::Int(3),
        ),
        (
            "return iter.items([1,2,3,4]).take(2).filter(x=>x>2).count();",
            Data::Int(0),
        ),
        ("return iter.items([none,some(1)]).count();", Data::Int(2)),
        (
            "let n=0; for(const x of iter.items([1,2,3,4])) {if(x==2) continue; if(x==4) break; n=n+x;} return n;",
            Data::Int(4),
        ),
        (
            "return iter.items([0]).map(x=>div(1,x)).take(0).count();",
            Data::Int(0),
        ),
        (
            "const p=iter.items([1,2]); return p.map(x=>x+1).count()+p.count();",
            Data::Int(4),
        ),
    ] {
        assert_eq!(evaluate(source).unwrap().data(), &expected, "{source}");
    }
}
#[test]
fn iter_boundary_purity_and_wrong_receiver_are_explicit() {
    for source in [
        "let n=1; return iter.items([1]).map(x=>x+n).collect();",
        "let n=0; return iter.items([1]).map(x=>{n=n+1;return x;}).collect();",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL009", "{source}");
    }

    for source in [
        "return iter.items([1]).map(x=>x);",
        "return {nested:iter.items([1]).filter(x=>true)};",
        "return iter.items([1]).take(-1);",
        "return iter.items([1]).length();",
        "return iter.items([1])==iter.items([1]);",
        "return iter.items([1]).field('missing').collect();",
    ] {
        assert_eq!(
            evaluate(source).unwrap_err(),
            if source.contains("take(-1)") {
                "CAL015"
            } else {
                "CAL004"
            },
            "{source}"
        );
    }
    assert!(matches!(
        evaluate("return iter.lines('a').take(1);").unwrap().data(),
        Data::Iter(_)
    ));
    assert_eq!(
        evaluate("let n=1; return [1].map(x=>x+n)[0];")
            .unwrap()
            .data(),
        &Data::Int(2)
    );
}

#[test]
fn captured_yaml_recipes_check_only_consumed_items_and_do_not_skip_invalid_typed_items() {
    let mut registry = ContractRegistry::new();
    registry
        .load(include_str!("../../../examples/iter.types.yaml"))
        .unwrap();
    for (source, expected) in [
        (
            "return iter.use('shortLines','ok\\ntoo long').take(1).count();",
            Ok(Data::Int(1)),
        ),
        (
            "return iter.use('shortLines','ok\\ntoo long').skip(1).count();",
            Err("CAL017"),
        ),
        (
            "return iter.use('shortLines','too long').take(0).count();",
            Ok(Data::Int(0)),
        ),
        (
            "return iter.checked(iter.items([1,'bad']),'Int').take(1).count();",
            Ok(Data::Int(1)),
        ),
        (
            "return iter.checked(iter.items([1,'bad']),'Int').count();",
            Err("CAL017"),
        ),
    ] {
        let program = calc::parse_body(source, 0, Package::standard()).unwrap();
        let compiled = calc::analyze(
            Arc::new(program),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &registry,
                workspace: &|_| None,
            },
        )
        .unwrap();
        let mut m =
            Machine::new(Arc::new(compiled), Default::default(), Limits::default()).unwrap();
        let actual = loop {
            match m.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Ok(Step::Complete(v)) => break Ok(v.data().clone()),
                Err(e) => break Err(e.code),
                Ok(Step::Request(_)) => panic!("unexpected request"),
            }
        };
        assert_eq!(actual, expected, "{source}");
    }
}

fn decode_bytes(
    input: Value,
    limits: Limits,
    token: &CancellationToken,
) -> Result<Value, wes_engine::calc::Failure> {
    let program = calc::parse_body("return text($input);", 0, Package::standard()).unwrap();
    let compiled = calc::analyze(
        Arc::new(program),
        calc::Environment {
            catalogue: &Catalogue::new(),
            contracts: &ContractRegistry::new(),
            workspace: &|name| (name == "input").then(|| input.shape().clone()),
        },
    )
    .unwrap();
    let mut machine = Machine::new(Arc::new(compiled), [("input".into(), input)].into(), limits)?;
    loop {
        match machine.poll(token)? {
            Step::Yield => {}
            Step::Complete(value) => return Ok(value),
            Step::Request(_) => panic!("Bytes decoding must not invoke a host"),
        }
    }
}
fn byte_value(bytes: &[u8]) -> Value {
    Value::new(
        Shape::Primitive(wes_core::Primitive::Bytes),
        Data::Bytes(bytes.to_vec().into()),
        Provenance::default()
            .with_fact("source", "synthetic")
            .with_policy(
                &wes_core::flow::FlowPolicy::default()
                    .from_origin("fixture")
                    .private(),
            ),
    )
    .unwrap()
}

#[test]
fn explicit_text_decodes_bytes_exactly_and_preserves_private_provenance() {
    for text in ["", "Türkçe 😀", "\u{feff}header\r\nbody\0", "e\u{301}"] {
        let input = byte_value(text.as_bytes());
        let output =
            decode_bytes(input.clone(), Limits::default(), &CancellationToken::new()).unwrap();
        assert_eq!(output.shape(), &Shape::Primitive(wes_core::Primitive::Text));
        assert_eq!(output.data(), &Data::Text(text.into()));
        assert_eq!(output.provenance(), input.provenance());
        assert!(output.provenance().policy().is_private());
        assert_eq!(input.data(), &Data::Bytes(text.as_bytes().to_vec().into()));
    }
    for (source, expected) in [
        ("return text(42);", "42"),
        ("return text(true);", "true"),
        ("return text('already text');", "already text"),
    ] {
        assert_eq!(
            evaluate(source).unwrap().data(),
            &Data::Text(expected.into())
        );
    }
    assert_eq!(evaluate("return text([1,2]);").unwrap_err(), "CAL004");
}

#[test]
fn explicit_text_rejects_invalid_and_incomplete_utf8_without_echoing_bytes() {
    for bytes in [
        b"private-canary\xff".as_slice(),
        b"private-canary\xe2\x82",
        b"private-canary\xc0\xaf",
    ] {
        let error = decode_bytes(
            byte_value(bytes),
            Limits::default(),
            &CancellationToken::new(),
        )
        .unwrap_err();
        assert_eq!(error.code, "CAL016");
        assert!(error.message.contains("UTF-8"));
        assert!(error.message.contains("byte 14"));
        assert!(!error.message.contains("private-canary"));
    }
}

#[test]
fn explicit_text_obeys_work_allocation_and_cancellation_budgets() {
    let input = byte_value(&vec![b'x'; 32_768]);
    for limits in [
        Limits {
            work: 1000,
            ..Limits::default()
        },
        Limits {
            bytes: 40_000,
            ..Limits::default()
        },
    ] {
        let error = decode_bytes(input.clone(), limits, &CancellationToken::new()).unwrap_err();
        assert_eq!(error.code, "CAL006");
    }
    let token = CancellationToken::new();
    token.cancel();
    assert!(
        decode_bytes(input, Limits::default(), &token)
            .unwrap_err()
            .cancelled
    );
}

fn runtime_failure(source: &str) -> wes_engine::calc::Failure {
    let mut machine =
        machine(source, Limits::default()).unwrap_or_else(|error| panic!("{source}: {error}"));
    loop {
        match machine.poll(&CancellationToken::new()) {
            Ok(Step::Yield) => {}
            Err(error) => return error,
            other => panic!("expected local failure for {source}: {other:?}"),
        }
    }
}

#[test]
fn calculation_errors_identify_member_kind_counts_and_bounds_without_payloads() {
    for (source, code, fragments) in [
        (
            "return [1].map(x => x.toString());",
            "CAL004",
            vec!["'toString'", "Int", "text(value)"],
        ),
        (
            "function identity(x){return x;} return identity({secret:'payload-canary'}).missing;",
            "CAL004",
            vec!["'missing'", "Record"],
        ),
        (
            "function identity(x){return x;} return identity({}).toString();",
            "CAL004",
            vec!["'toString'", "Record"],
        ),
        (
            "return {}['payload-canary'];",
            "CAL004",
            vec!["record field", "has(record, key)"],
        ),
        (
            "function f(x) { if(x) return 1; return 0; } return f('payload-canary');",
            "CAL004",
            vec!["expected Bool", "received Text"],
        ),
        (
            "return range('payload-canary');",
            "CAL004",
            vec!["expected Int", "received Text"],
        ),
        (
            "return has({}, 1);",
            "CAL004",
            vec!["expected Text", "received Int"],
        ),
        (
            "return map('payload-canary', x=>x);",
            "CAL004",
            vec!["expected List", "received Text"],
        ),
        (
            "return text({secret:'payload-canary'});",
            "CAL004",
            vec!["text", "received Record"],
        ),
        (
            "return length(1);",
            "CAL004",
            vec!["List, Record, Text or Bytes", "received Int"],
        ),
        (
            "return isSome(true);",
            "CAL004",
            vec!["Option", "received Bool"],
        ),
        ("return keys(1);", "CAL004", vec!["keys", "received Int"]),
        (
            "return int('payload-canary');",
            "CAL016",
            vec!["int", "Text", "64-bit"],
        ),
        (
            "return decimal('payload-canary');",
            "CAL016",
            vec!["decimal", "Text"],
        ),
        (
            "function f(x) { return x + 1; } return f('payload-canary');",
            "CAL004",
            vec!["'add'", "Text and Int"],
        ),
        (
            "function f(x) { return x < 1; } return f('payload-canary');",
            "CAL004",
            vec!["comparison", "Text and Int"],
        ),
        (
            "return [1][3];",
            "CAL015",
            vec!["index 3", "length is 1", "0..0"],
        ),
        ("return [1][-1];", "CAL015", vec!["index -1", "0..0"]),
        (
            "return [][0];",
            "CAL015",
            vec!["List is empty", "no valid index"],
        ),
        (
            "return '😀a'[2];",
            "CAL015",
            vec!["index 2", "length is 2", "0..1"],
        ),
        ("return ''[0];", "CAL015", vec!["Text is empty"]),
        (
            "return 'a'[-1];",
            "CAL015",
            vec!["index -1", "indices start at 0"],
        ),
        (
            "return true[0];",
            "CAL004",
            vec!["indexing", "received Bool"],
        ),
        (
            "return [1].map((x,y)=>x);",
            "CAL012",
            vec!["expects 2", "received 1"],
        ),
        (
            "const f=range; return f();",
            "CAL012",
            vec!["'range'", "expects 1..3", "received 0"],
        ),
        (
            "const f=[1].map; return f();",
            "CAL012",
            vec!["method 'map'", "expects 1", "received 0"],
        ),
        (
            "const f=[1].map; return f(x=>x, 2);",
            "CAL012",
            vec!["method 'map'", "expects 1", "received 2"],
        ),
        (
            "let f=1; return f();",
            "CAL004",
            vec!["Int", "not callable", "Function"],
        ),
        ("return div(1,0);", "CAL005", vec!["div divisor", "zero"]),
        ("return rem(1,0);", "CAL005", vec!["rem divisor", "zero"]),
        (
            "return 9223372036854775807+1;",
            "CAL005",
            vec!["overflow", "64-bit"],
        ),
        (
            "return div(-9223372036854775807-1,-1);",
            "CAL005",
            vec!["overflow", "64-bit"],
        ),
    ] {
        let error = runtime_failure(source);
        assert_eq!(error.code, code, "{source}");
        for fragment in fragments {
            assert!(
                error.message.contains(fragment),
                "{source}: {} lacks {fragment}",
                error.message
            );
        }
        assert!(
            !error.message.contains("payload-canary"),
            "{source}: {}",
            error.message
        );
        assert!(
            !error.message.contains("secret"),
            "{source}: {}",
            error.message
        );
        if source == "function identity(x){return x;} return identity({}).toString();" {
            assert!(!error.message.contains("use text"));
        }
        assert!(error.span.end() <= source.len());
    }
    let error = runtime_failure("return 9223372036854775807+1;");
    assert!(!error.message.contains("zero"));
    let error = runtime_failure(&format!(
        "function identity(x){{return x;}} return identity({{}}).{};",
        "ü".repeat(1000)
    ));
    assert!(error.message.contains('…'));
    assert!(error.message.len() < 1024);
}

#[test]
fn mistaken_javascript_members_offer_supported_alternatives_without_value_text() {
    for (source, hint) in [
        ("return [1,2].length;", "length(value)"),
        (
            "return \"private-canary\".includes(\"x\");",
            "calc is not JavaScript",
        ),
    ] {
        let mut machine =
            machine(source, Limits::default()).unwrap_or_else(|error| panic!("{source}: {error}"));
        loop {
            match machine.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => continue,
                Err(error) => {
                    assert!(error.message.contains(hint), "{}", error.message);
                    assert!(!error.message.contains("private-canary"));
                    break;
                }
                _ => panic!("expected actionable error"),
            }
        }
    }
}

#[test]
fn typed_calculation_parameters_preserve_private_origin_even_when_unused() {
    use indexmap::IndexMap;
    for source in [
        ":calc pure { return text(input); }",
        ":calc pure { return 'constant'; }",
    ] {
        let program = calc::parse_context(source, 0, Package::standard()).unwrap();
        let input = byte_value(b"private synthetic value");
        let compiled = calc::analyze_with_parameters(
            program.into(),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| None,
            },
            &IndexMap::from([("input".into(), input.shape().clone())]),
        )
        .unwrap();
        assert!(compiled.workspace.is_empty());
        let mut machine = Machine::new(
            compiled.into(),
            IndexMap::from([("input".into(), input.clone())]),
            Limits::default(),
        )
        .unwrap();
        loop {
            match machine.poll(&CancellationToken::new()).unwrap() {
                Step::Yield => (),
                Step::Complete(value) => {
                    assert_eq!(value.provenance(), input.provenance());
                    break;
                }
                Step::Request(_) => panic!("pure input cannot call a host"),
            }
        }
    }
}

#[test]
fn temporal_builtins_preserve_precision_and_interval_field_types() {
    for source in [
        "const t=instant('2025-01-01T00:00:00.000000001Z'); return fromEpochSeconds(toEpochSeconds(t))==t && fromEpochMillis(toEpochMillis(t))==t && fromEpochNanos(toEpochNanos(t))==t;",
        "const t=fromEpochNanos(-1); return text(t)=='1969-12-31T23:59:59.999999999Z' && toEpochNanos(t)==decimal(-1);",
        "const a=instant('2025-01-01T00:00:00Z'); const b=instant('2025-01-01T01:00:00+01:00'); return a==b && a<=b;",
        "const t=fromEpochSeconds(0); const d=duration('PT0.000000001S'); return t+d==d+t && (t+d)-t==d && t-d<t && d+d>d;",
        "const t=fromEpochSeconds(0); const r=around(t,duration('PT5S')); return r.start==fromEpochSeconds(-5) && r.end==fromEpochSeconds(5) && r.end-r.start==duration('PT10S');",
        "const t=fromEpochSeconds(0); const r=interval(t,t); return r.start==r.end && text(r)=='1970-01-01T00:00:00Z/1970-01-01T00:00:00Z';",
        "const t=instant('+1000000000-12-31T23:59:59.999999999Z'); return fromEpochNanos(toEpochNanos(t))==t;",
    ] {
        assert_eq!(
            evaluate(source).unwrap().data(),
            &Data::Bool(true),
            "{source}"
        );
    }
    assert_eq!(
        evaluate("return interval(fromEpochSeconds(0),fromEpochSeconds(1));")
            .unwrap()
            .shape(),
        &Shape::Primitive(wes_core::Primitive::Interval)
    );
    assert_eq!(
        evaluate("return interval(fromEpochSeconds(0),fromEpochSeconds(1)).start;")
            .unwrap()
            .shape(),
        &Shape::Primitive(wes_core::Primitive::Instant)
    );
}
#[test]
fn temporal_errors_are_consistent_and_do_not_round_or_read_a_clock() {
    for source in [
        "return instant(3);",
        "return duration(false);",
        "return interval(fromEpochSeconds(0),3);",
        "return around(fromEpochSeconds(0),1);",
        "return fromEpochNanos('123');",
        "return toEpochMillis(1);",
        "return instant('2025-01-01T00:00:00Z')+1;",
        "return duration('PT1S')<1;",
        "return fromEpochSeconds(0)+fromEpochSeconds(1);",
        "return duration('PT1S')*duration('PT1S');",
    ] {
        assert_eq!(
            machine(source, Limits::default()).err().unwrap(),
            "CAL004",
            "{source}"
        );
    }
    for source in [
        "return instant('not-a-date');",
        "return duration('30s');",
        "return interval(fromEpochSeconds(1),fromEpochSeconds(0));",
        "return around(fromEpochSeconds(0),duration('PT0S'));",
        "return fromEpochSeconds(0.0000000001);",
        "return fromEpochNanos(1.5);",
        "return fromEpochSeconds(1e100000);",
        "return fromEpochSeconds(1e-100000);",
        "return instant('+1000000000-12-31T23:59:59.999999999Z')+duration('PT0.000000001S');",
    ] {
        assert_eq!(
            evaluate(source).unwrap_err(),
            if source.contains("not-a-date") || source.contains("30s") {
                "CAL016"
            } else {
                "CAL005"
            },
            "{source}"
        );
    }
    // Dynamic user-function returns must use the same runtime restrictions as statically known values.
    assert_eq!(
        evaluate("function f(){return duration('PT1S');} return f()<1;").unwrap_err(),
        "CAL004"
    );
}

#[test]
fn finite_concat_preserves_order_duplicates_and_structural_types() {
    for source in [
        "return concat([3,1],[1,2]);",
        "return [3,1].concat([1,2]);",
        "return [].concat([3,1,1,2]).concat([]);",
    ] {
        assert_eq!(
            evaluate(source).unwrap().data(),
            &Data::List(vec![Data::Int(3), Data::Int(1), Data::Int(1), Data::Int(2)])
        );
    }
    assert_eq!(
        evaluate("return [].concat([]);").unwrap().data(),
        &Data::List(vec![])
    );
    assert!(evaluate("return [{a:1,b:'a'}].concat([{b:'b',a:2}]);").is_ok());
    assert!(evaluate("return [[1]].concat([[]]);").is_ok());
    assert!(evaluate("return [none].concat([some(2)]);").is_ok());
    for source in [
        "return concat([1],[1.0]);",
        "return [1,'a'].concat([]);",
        "return [{a:1}].concat([{b:1}]);",
        "return [[1,'a']].concat([]);",
        "return concat(iter.items([1]),[2]);",
        "return concat('a','b');",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL004", "{source}");
    }
}

#[test]
fn sort_by_is_stable_and_compares_exact_keys_without_numeric_coercion() {
    let stable = evaluate("return [{key:2,id:'a'},{key:1,id:'b'},{key:2,id:'c'},{key:1,id:'d'}].sortBy(row=>row.key).map(row=>row.id);").unwrap();
    assert_eq!(
        stable.data(),
        &Data::List(["b", "d", "a", "c"].map(|s| Data::Text(s.into())).to_vec())
    );
    assert_eq!(
        evaluate("return sortBy([4,2,3,1],x=>x);").unwrap().data(),
        &Data::List([1, 2, 3, 4].map(Data::Int).to_vec())
    );
    assert_eq!(
        evaluate("return [].sortBy(x=>x);").unwrap().data(),
        &Data::List(vec![])
    );
    assert_eq!(
        evaluate("return [1.00,0.1,1.0].sortBy(x=>x).map(x=>text(x));")
            .unwrap()
            .data(),
        &Data::List(
            ["0.1", "1.00", "1.0"]
                .map(|s| Data::Text(s.into()))
                .to_vec()
        )
    );
    let exact = evaluate("return [instant('2030-01-01T00:00:00.000000002Z'),instant('2030-01-01T00:00:00.000000001Z')].sortBy(t=>t).map(t=>text(t));").unwrap();
    assert_eq!(
        exact.data(),
        &Data::List(
            [
                "2030-01-01T00:00:00.000000001Z",
                "2030-01-01T00:00:00.000000002Z"
            ]
            .map(|s| Data::Text(s.into()))
            .to_vec()
        )
    );
    assert_eq!(
        evaluate("return [duration('PT2S'),duration('PT1S')].sortBy(t=>t).map(t=>text(t));")
            .unwrap()
            .data(),
        &Data::List(["PT1S", "PT2S"].map(|s| Data::Text(s.into())).to_vec())
    );
    for source in [
        "return [none].sortBy(x=>x);",
        "return [true].sortBy(x=>x);",
        "return [1,2.0].sortBy(x=>x);",
        "return [1].sortBy(x=>[]);",
        "return [1].sortBy(1);",
        "return iter.items([1]).sortBy(x=>x);",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL004", "{source}");
    }
}

#[test]
fn sort_selector_purity_covers_helpers_and_dynamic_function_values_even_for_empty_lists() {
    for source in [
        "let counter=0; return [1].sortBy(x=>{counter=counter+1;return x;});",
        "let bias=1; return [].sortBy(x=>x+bias);",
        "let counter=0; function helper(x){counter=counter+1;return x;} return [1].sortBy(x=>helper(x));",
        "let selector=x=>1; return [1].sortBy(x=>selector(x));",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL009", "{source}");
    }
    assert_eq!(evaluate("const bias=3; function key(x){let result=x+bias;return result;} return [2,1].sortBy(key);").unwrap().data(), &Data::List(vec![Data::Int(1),Data::Int(2)]));
}

#[test]
fn sort_selector_is_evaluated_once_per_item_across_host_suspensions() {
    let mut machine = machine(
        "return [4,1,3,2].sortBy(x=>parseJson(text(x)));",
        Limits {
            quantum: 2,
            ..Limits::default()
        },
    )
    .unwrap();
    let mut calls = 0;
    loop {
        match machine.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Request(wes_engine::calc::Request::ParseJson { id, text, .. }) => {
                calls += 1;
                machine
                    .resume(
                        id,
                        Ok(Value::new(
                            Shape::Primitive(wes_core::Primitive::Int),
                            Data::Int(text.parse().unwrap()),
                            Provenance::default(),
                        )
                        .unwrap()),
                    )
                    .unwrap();
            }
            Step::Complete(value) => {
                assert_eq!(
                    value.data(),
                    &Data::List([1, 2, 3, 4].map(Data::Int).to_vec())
                );
                break;
            }
            _ => panic!("sort requested external execution"),
        }
    }
    assert_eq!(calls, 4);
}

#[test]
fn finite_collections_obey_work_memory_and_cancellation_limits() {
    for source in [
        "return range(10000).concat(range(10000));",
        "return range(10000).sortBy(x=>0-x);",
    ] {
        for limits in [
            Limits {
                work: 2000,
                ..Limits::default()
            },
            Limits {
                bytes: 500_000,
                ..Limits::default()
            },
        ] {
            let mut machine = machine(source, limits).unwrap();
            loop {
                match machine.poll(&CancellationToken::new()) {
                    Ok(Step::Yield) => {}
                    Err(error) => {
                        assert_eq!(error.code, "CAL006");
                        break;
                    }
                    _ => panic!("limit was not enforced"),
                }
            }
        }
        let mut machine = machine(
            source,
            Limits {
                quantum: 1,
                ..Limits::default()
            },
        )
        .unwrap();
        let token = CancellationToken::new();
        assert!(matches!(machine.poll(&token), Ok(Step::Yield)));
        token.cancel();
        assert!(machine.poll(&token).unwrap_err().cancelled);
    }
}
#[test]
fn work_budget_diagnostic_reports_the_configured_unit_count() {
    for work in [100, 250] {
        let mut m = machine(
            "while(true) {} return 1;",
            Limits {
                work,
                ..Limits::default()
            },
        )
        .unwrap();
        loop {
            match m.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Err(error) => {
                    assert!(error.message.contains(&format!("{work} work units")));
                    assert!(error.message.contains("not loop iterations"));
                    break;
                }
                _ => panic!("expected work limit"),
            }
        }
    }
}

#[test]
fn optional_collection_elements_unify_without_numeric_coercion() {
    use wes_core::Primitive;
    let option = Shape::Option(Box::new(Shape::Primitive(Primitive::Int)));
    for source in [
        "return [none,some(30)];",
        "return [some(30),none];",
        "return [1,2].map(x=>{if(x==1)return none;return some(x);});",
    ] {
        assert_eq!(
            evaluate(source).unwrap().shape(),
            &Shape::List(Box::new(option.clone())),
            "{source}"
        );
    }
    let nested = evaluate("return [[none],[some(30)]];").unwrap();
    assert_eq!(
        nested.shape(),
        &Shape::List(Box::new(Shape::List(Box::new(option))))
    );
    for source in [
        "return [some(1),some(1.0)];",
        "return [some(1),some('x'),some(2)];",
        "return [1,'x',2];",
        "return [[1,'x'],[2]];",
    ] {
        assert_eq!(
            evaluate(source).unwrap().shape(),
            &Shape::List(Box::new(Shape::Unknown)),
            "{source}"
        );
    }
}

#[test]
fn pure_text_and_record_transforms_preserve_values_and_unicode_boundaries() {
    for (source, expected) in [
        ("return join(['a','b',''],',');", Data::Text("a,b,".into())),
        ("return [].join(',');", Data::Text("".into())),
        ("return ['a'].join(',');", Data::Text("a".into())),
        ("return slice('a😀éZ',1,3);", Data::Text("😀é".into())),
        ("return 'a😀éZ'.slice(2);", Data::Text("éZ".into())),
        ("return slice('😀',1,1);", Data::Text("".into())),
        (
            "return slice([10,20,30],1,2);",
            Data::List(vec![Data::Int(20)]),
        ),
        ("return slice([],0);", Data::List(vec![])),
        (
            "const before={a:1,b:'old'};const after=withFields(before,{b:'new',c:some(3)});return [before.b,after.b];",
            Data::List(vec![Data::Text("old".into()), Data::Text("new".into())]),
        ),
    ] {
        assert_eq!(evaluate(source).unwrap().data(), &expected, "{source}");
    }
    for source in [
        "return join([1],',');",
        "return join(['a'],1);",
        "return slice('x',-1);",
        "return slice('x',0,2);",
        "return slice('x',1,0);",
        "return slice([1],0,2);",
        "return withFields(1,{});",
        "return withFields({},1);",
    ] {
        assert_eq!(
            evaluate(source).unwrap_err(),
            if source.starts_with("return slice(") {
                "CAL015"
            } else {
                "CAL004"
            },
            "{source}"
        );
    }
}

#[test]
fn transforms_yield_and_budget_work_before_large_allocations() {
    let mut m = machine(
        "return range(5000).map(x=>text(x)).join(',');",
        Limits {
            quantum: 8,
            ..Limits::default()
        },
    )
    .unwrap();
    let token = CancellationToken::new();
    assert!(matches!(m.poll(&token).unwrap(), Step::Yield));
    token.cancel();
    assert_eq!(m.poll(&token).unwrap_err().code, "CAL007");
    for source in [
        format!("return ['{}'].join(',');", "x".repeat(200)),
        format!("return slice('{}',0);", "x".repeat(200)),
    ] {
        let mut m = machine(
            &source,
            Limits {
                work: 100,
                quantum: 8,
                ..Limits::default()
            },
        )
        .unwrap();
        loop {
            match m.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Err(error) => {
                    assert_eq!(error.code, "CAL006");
                    break;
                }
                _ => panic!("transform escaped its work budget"),
            }
        }
    }
}

#[test]
fn regex_capture_groups_are_lazy_optional_and_independently_reusable() {
    let output=evaluate("const xs=iter.captures('code=503 code=200/foo','code=([0-9]+)(?:/([a-z]+))?');return xs.collect();").unwrap();
    let Data::List(rows) = output.data() else {
        panic!("capture rows")
    };
    assert_eq!(rows.len(), 2);
    let Data::Record(first) = &rows[0] else {
        panic!("capture record")
    };
    assert_eq!(first["match"], Data::Text("code=503".into()));
    assert_eq!(
        first["groups"],
        Data::List(vec![
            Data::Option(Some(Box::new(Data::Text("503".into())))),
            Data::Option(None)
        ])
    );
    let Data::Record(second) = &rows[1] else {
        panic!("capture record")
    };
    assert_eq!(
        second["groups"],
        Data::List(vec![
            Data::Option(Some(Box::new(Data::Text("200".into())))),
            Data::Option(Some(Box::new(Data::Text("foo".into()))))
        ])
    );
    assert_eq!(
        evaluate("const xs=iter.captures('é','()');return [xs.count(),xs.count()];")
            .unwrap()
            .data(),
        &Data::List(vec![Data::Int(2), Data::Int(2)])
    );
    assert_eq!(
        evaluate("return iter.captures('abc','[0-9]+').count();")
            .unwrap()
            .data(),
        &Data::Int(0)
    );
    assert_eq!(
        evaluate("return iter.captures('abc','(').take(0).collect();").unwrap_err(),
        "CAL016"
    );
}

#[test]
fn boolean_regex_is_pure_bounded_unicode_and_does_not_collect() {
    for (source, expected) in [
        (
            "return regexTest('é status=503','status=[45][0-9]{2}');",
            true,
        ),
        (
            "return regexTest('status=200','status=[45][0-9]{2}');",
            false,
        ),
        ("return regexTest('','');", true),
        ("return regexTest('é','^é$');", true),
        ("return regexTest('é','^$');", false),
    ] {
        assert_eq!(evaluate(source).unwrap().data(), &Data::Bool(expected));
    }
    assert_eq!(
        evaluate("return regexTest('x','[');").unwrap_err(),
        "CAL016"
    );
    assert_eq!(evaluate("return regexTest(1,'x');").unwrap_err(), "CAL004");
    assert_eq!(
        evaluate("return 'x'.regexTest('x');").unwrap_err(),
        "CAL002"
    );
    let result =
        evaluate("return iter.items(['ok','bad']).filter(x=>regexTest(x,'^ok$')).collect();")
            .unwrap();
    assert_eq!(result.data(), &Data::List(vec![Data::Text("ok".into())]));
    let mut low_work = machine(
        "return regexTest('abcdefghijklmnopqrstuvwxyz','z');",
        Limits {
            work: 20,
            ..Limits::default()
        },
    )
    .unwrap();
    let error = loop {
        match low_work.poll(&CancellationToken::new()) {
            Ok(Step::Yield) => {}
            Err(e) => break e,
            _ => panic!("work limit bypassed"),
        }
    };
    assert_eq!(error.code, "CAL006");
    assert_eq!(
        low_work.usage().regex.compilations,
        0,
        "refuse before native compilation/search"
    );
    let mut low_memory = machine(
        "return regexTest('x','x');",
        Limits {
            bytes: 1024,
            ..Limits::default()
        },
    )
    .unwrap();
    assert_eq!(
        low_memory.poll(&CancellationToken::new()).unwrap_err().code,
        "CAL006"
    );
    assert_eq!(low_memory.usage().regex.compilations, 0);
}

#[test]
fn normalization_has_typed_empty_spans_and_requires_explicit_text_selection() {
    let value = evaluate("return stripAnsi('');").unwrap();
    assert_eq!(value.shape(), &wes_core::text::normalized_shape());
    assert_eq!(
        evaluate("return stripAnsi('plain').text;").unwrap().data(),
        &Data::Text("plain".into())
    );
    let encoded = serde_json::to_string("é\x1b[31mERR\x1b[0m\r\n").unwrap();
    let output = evaluate(&format!("return stripAnsi({encoded});")).unwrap();
    let Data::Record(record) = output.data() else {
        panic!("normalized record")
    };
    assert_eq!(record["text"], Data::Text("éERR\r\n".into()));
    let Data::List(spans) = &record["spans"] else {
        panic!("span list")
    };
    assert_eq!(spans.len(), 3);
    for malformed in ["\x1b[", "\x1b]unterminated", "\x1b7"] {
        let literal = serde_json::to_string(malformed).unwrap();
        assert_eq!(
            evaluate(&format!("return stripAnsi({literal});")).unwrap_err(),
            "CAL016"
        );
    }
}

#[test]
fn regex_admission_counts_full_long_regions_and_rejects_oversized_patterns_before_compiling() {
    fn captured(source: &str, pattern: &str, limits: Limits) -> Machine {
        let program = calc::parse_body(
            "return regexTest($source,$pattern);",
            0,
            Package::standard(),
        )
        .unwrap();
        let text = Shape::Primitive(wes_core::Primitive::Text);
        let compiled = calc::analyze(
            Arc::new(program),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| Some(text.clone()),
            },
        )
        .unwrap();
        let inputs = [("source", source), ("pattern", pattern)]
            .into_iter()
            .map(|(name, value)| {
                (
                    name.into(),
                    Value::new(
                        text.clone(),
                        Data::Text(value.into()),
                        Provenance::default(),
                    )
                    .unwrap(),
                )
            })
            .collect();
        Machine::new(Arc::new(compiled), inputs, limits).unwrap()
    }
    for (source, expected) in [
        ("a".repeat(131_072), false),
        ("a".repeat(131_072) + "z", true),
    ] {
        let mut vm = captured(&source, "z", Limits::default());
        loop {
            match vm.poll(&CancellationToken::new()).unwrap() {
                Step::Yield => {}
                Step::Complete(value) => {
                    assert_eq!(value.data(), &Data::Bool(expected));
                    break;
                }
                Step::Request(_) => panic!("native boolean test must not request host work"),
            }
        }
        assert!(vm.usage().work >= source.len() as u64);
    }
    let mut vm = captured("x", &"a".repeat(16_385), Limits::default());
    let error = loop {
        match vm.poll(&CancellationToken::new()) {
            Err(error) => break error,
            Ok(Step::Yield) => {}
            _ => panic!("oversized pattern was admitted"),
        }
    };
    assert_eq!(error.code, "CAL006");
    assert_eq!(vm.usage().regex.compilations, 0);
}

#[test]
fn workload_metrics_distinguish_vm_charge_from_cache_compilation() {
    let source = "let hits=0; for(const row of range(10)){ for(const pattern of ['status=[0-9]+','^WARN','^ERROR','latency','retry','^OK','complete$']){ if(regexTest('status=503 complete',pattern)) hits=hits+1; } } return hits;";
    let mut machine = machine(source, Limits::default()).unwrap();
    loop {
        match machine.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Complete(value) => {
                assert_eq!(value.data(), &Data::Int(20));
                break;
            }
            Step::Request(_) => panic!("pure native call"),
        }
    }
    let usage = machine.usage();
    assert_eq!(usage.regex.compilations, 7);
    assert_eq!(usage.regex.hits, 63);
    assert!(usage.allocated_bytes >= 7 * wes_core::IterRegexCache::COMPILED_CHARGE);
    println!(
        "work={} allocation-charge={} regex={:?}",
        usage.work, usage.allocated_bytes, usage.regex
    );
}

#[test]
fn reopening_an_evicted_regex_plan_is_charged_and_empty_matches_terminate() {
    let source = "const rows=iter.matches('z','z'); for(const n of range(20)){regexTest('x',text(n));} return rows.count();";
    let mut vm = machine(source, Limits::default()).unwrap();
    loop {
        match vm.poll(&CancellationToken::new()).unwrap() {
            Step::Yield => {}
            Step::Complete(value) => {
                assert_eq!(value.data(), &Data::Int(1));
                break;
            }
            Step::Request(_) => panic!("native operation"),
        }
    }
    assert_eq!(vm.usage().regex.compilations, 22);
    assert!(vm.usage().allocated_bytes >= 22 * wes_core::IterRegexCache::COMPILED_CHARGE);
    assert_eq!(
        evaluate("return iter.matches('é','').count();")
            .unwrap()
            .data(),
        &Data::Int(2)
    );
    let mut vm = machine(
        "return regexTest('private synthetic text','.*');",
        Limits::default(),
    )
    .unwrap();
    let token = CancellationToken::new();
    token.cancel();
    assert_eq!(vm.poll(&token).unwrap_err().code, "CAL007");
    assert_eq!(vm.usage().regex.compilations, 0);
}

#[test]
fn capture_collection_preserves_declared_types_even_without_matching_groups_or_rows() {
    let capture = wes_core::IterMode::capture_shape();
    for source in [
        "return iter.captures('a','(b)?a').collect();",
        "return iter.captures('a','z').collect();",
    ] {
        assert_eq!(
            evaluate(source).unwrap().shape(),
            &Shape::List(Box::new(capture.clone()))
        );
    }
    let groups = Shape::List(Box::new(Shape::Option(Box::new(Shape::Primitive(
        wes_core::Primitive::Text,
    )))));
    for source in [
        "return iter.captures('a','(b)?a').field('groups').collect();",
        "return iter.captures('a','(b)?a').map(x=>x.groups).collect();",
    ] {
        assert_eq!(
            evaluate(source).unwrap().shape(),
            &Shape::List(Box::new(groups.clone()))
        );
    }
}

#[test]
fn record_updates_require_rechecking_nominal_constraints() {
    let mut contracts = ContractRegistry::new();
    contracts.load("types: {Positive: {base: Int, min: 1}, Counter: {base: Record, fields: {count: Positive}}}").unwrap();
    for (source, checked) in [
        (
            "const before=check('Counter',{count:2});return withFields(before,{count:0});",
            false,
        ),
        (
            "const before=check('Counter',{count:2});return check('Counter',withFields(before,{count:0}));",
            true,
        ),
    ] {
        let compiled = calc::analyze(
            Arc::new(calc::parse_body(source, 0, Package::standard()).unwrap()),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &contracts,
                workspace: &|_| None,
            },
        )
        .unwrap();
        let mut machine =
            Machine::new(Arc::new(compiled), Default::default(), Limits::default()).unwrap();
        loop {
            match machine.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Ok(Step::Complete(value)) => {
                    assert!(!checked);
                    let Shape::Record(record) = value.shape() else {
                        panic!("record output");
                    };
                    assert_eq!(record.name(), "");
                    assert_eq!(
                        value.data(),
                        &Data::Record([("count".into(), Data::Int(0))].into())
                    );
                    break;
                }
                Err(error) => {
                    assert!(checked);
                    assert_eq!(error.code, "CAL017");
                    break;
                }
                _ => panic!("pure record update requested external execution"),
            }
        }
    }
}

#[test]
fn explicit_duration_units_scaling_ratios_and_utc_parts_have_exact_types() {
    for source in [
        "const d=duration('PT5M'); return d*3==duration('PT15M') && 0.5*d==duration('PT2M30S') && d/2==duration('PT2M30S');",
        "return duration('PT10M')/duration('PT5M')==2.0;",
        "const d=durationSeconds(-0.000000001); return toNanos(d)==-1.0 && durationMillis(toMillis(d))==d && durationNanos(toNanos(d))==d;",
        "const p=utcParts(instant('2000-02-29T23:59:58.000000001Z')); return p.year==2000 && p.month==2 && p.day==29 && p.hour==23 && p.minute==59 && p.second==58 && p.nanosecond==1 && p.weekday==2;",
        "return text(duration('-PT1M5S'))=='-PT1M5S';",
    ] {
        assert_eq!(
            evaluate(source).unwrap().data(),
            &Data::Bool(true),
            "{source}"
        );
    }
    for source in [
        "return durationNanos(0.1);",
        "return durationNanos(1)/2;",
        "return durationSeconds(1)/0;",
        "return durationSeconds(1)/durationSeconds(3);",
        "return durationSeconds(9223372036854775807)*2;",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL005", "{source}");
    }
    for source in [
        "return instant('2000-01-01T00:00:00Z').hour;",
        "return toSeconds(1);",
        "return durationSeconds('1');",
        "return 1/durationSeconds(1);",
    ] {
        assert_eq!(
            machine(source, Limits::default()).err().unwrap(),
            "CAL004",
            "{source}"
        );
    }
}
#[test]
fn diagnostic_categories_and_limits_describe_the_cause() {
    for (source, code) in [
        ("return [1][2];", "CAL015"),
        ("return 'x'[-1];", "CAL015"),
        ("return [1].slice(0,2);", "CAL015"),
        ("return iter.items([1]).skip(-2);", "CAL015"),
        ("return instant('2025-02-30T00:00:00Z');", "CAL016"),
        ("return iter.matches('abc','[').collect();", "CAL016"),
        ("return check('Int','bad');", "CAL017"),
    ] {
        assert_eq!(evaluate(source).unwrap_err(), code, "{source}");
    }
    for (source, limits, message) in [
        (
            "return 'abcdefgh';",
            Limits {
                bytes: 2,
                ..Limits::default()
            },
            "2 bytes",
        ),
        (
            "function recurse(n){return recurse(n+1);} return recurse(0);",
            Limits {
                frames: 2,
                ..Limits::default()
            },
            "2 frames",
        ),
    ] {
        let mut m = machine(source, limits).unwrap();
        loop {
            match m.poll(&CancellationToken::new()) {
                Ok(Step::Yield) => {}
                Err(e) => {
                    assert_eq!(e.code, "CAL006");
                    assert!(e.message.contains(message), "{}", e.message);
                    break;
                }
                _ => panic!("expected bounded failure"),
            }
        }
    }
}

#[test]
fn lexical_operation_shadowing_executes_locals_closures_loops_and_receiver_methods() {
    for (source, expected) in [
        ("const count=3; return count;", Data::Int(3)),
        (
            "let count=0;for(const length of [1,2,3]){count=count+length;}return count;",
            Data::Int(6),
        ),
        (
            "const length=9;function keys(length){return length+1;}return keys(2)+length;",
            Data::Int(12),
        ),
        (
            "const count=4;const f=()=>count;{const count=8;}return f();",
            Data::Int(4),
        ),
        (
            "const count=4;function keys(){const count=8;return ()=>count;}const f=keys();return f()+count;",
            Data::Int(12),
        ),
        (
            "function count(n){if(n==0)return 0;return count(n-1)+1;}return count(4);",
            Data::Int(4),
        ),
        (
            "const f=()=>length;const length=7;return f();",
            Data::Int(7),
        ),
        (
            "let length=7;const f=()=>length;length=9;return f();",
            Data::Int(9),
        ),
        ("const length=7;return [1,2].length()+length;", Data::Int(9)),
        (
            "const count=7;return iter.items([1,2]).count()+count;",
            Data::Int(9),
        ),
        (
            "const keys=7;return {keys:()=>3}.keys()+keys;",
            Data::Int(10),
        ),
        (
            "let total=0;{const length=5;total=length;}return total+length('abc');",
            Data::Int(8),
        ),
        (
            "const call=x=>x+1;const check=x=>x+2;const parseJson=x=>x+3;return call(1)+check(1)+parseJson(1);",
            Data::Int(9),
        ),
    ] {
        assert_eq!(evaluate(source).unwrap().data(), &expected, "{source}");
    }
    assert_eq!(
        evaluate("function map(x){return x+2;}return iter.items([1,2]).map(x=>map(x)).collect();")
            .unwrap()
            .data(),
        &Data::List(vec![Data::Int(3), Data::Int(4)])
    );
    for name in [
        "length",
        "count",
        "keys",
        "duration",
        "join",
        "slice",
        "call",
        "check",
        "parseJson",
    ] {
        let source = format!("const {name}=3;return {name}(1);");
        assert_eq!(evaluate(&source).unwrap_err(), "CAL004", "{source}");
    }
    assert_eq!(
        evaluate("const f=()=>length;return f();const length=7;").unwrap_err(),
        "CAL013"
    );
    assert_eq!(
        evaluate("const count=1;count=2;return count;").unwrap_err(),
        "CAL011"
    );
}

#[test]
fn dynamically_returned_shadowing_helpers_cannot_bypass_callback_purity() {
    for source in [
        "let count=0;const make=()=>{function length(x){count=count+1;return x;}return length;};const draw=make();return iter.items([1]).map(draw).collect();",
        "let count=0;const make=()=>{function length(x){count=count+1;return x;}return length;};const draw=make();return [].sortBy(draw);",
    ] {
        assert_eq!(evaluate(source).unwrap_err(), "CAL009", "{source}");
    }
    assert_eq!(evaluate("const make=()=>{function length(x){return x+1;}return length;};const draw=make();return iter.items([1,2]).map(draw).collect();").unwrap().data(),&Data::List(vec![Data::Int(2),Data::Int(3)]));
}

#[test]
fn retrospective_numeric_diagnostics_keep_exact_semantics_and_executable_hints() {
    for source in ["return int('12x');", "return decimal('1.2.3');"] {
        let failure = runtime_failure(source);
        assert_eq!(failure.code, "CAL016", "{source}");
        assert!(!failure.message.contains("12x") && !failure.message.contains("1.2.3"));
    }
    assert_eq!(
        runtime_failure("return int('9223372036854775808');").code,
        "CAL005"
    );
    assert_eq!(runtime_failure("return int(1.5);").code, "CAL005");
    for source in ["return int(true);", "return int([1]);", "return int(none);"] {
        assert_eq!(runtime_failure(source).code, "CAL004", "{source}");
    }
    let failure = runtime_failure("return duration('PT10S') / 3;");
    assert_eq!(failure.code, "CAL005");
    assert!(
        failure
            .message
            .contains("durationNanos(roundDiv(toNanos(total), count, 0))")
    );
    assert_eq!(evaluate("const total=duration('PT10S'); const count=3; return durationNanos(roundDiv(toNanos(total), count, 0));").unwrap().data(), &Data::Duration("PT3.333333333S".parse().unwrap()));
    let failure = runtime_failure("return duration('PT1S') / 2e9;");
    assert!(failure.message.contains("durationNanos(roundDiv"));
    let failure = runtime_failure("return duration('PT1S') / duration('PT3S');");
    assert!(
        failure
            .message
            .contains("roundDiv(toNanos(a), toNanos(b), scale)")
    );
    assert_eq!(evaluate("const a=duration('PT1S'); const b=duration('PT3S'); const scale=3; return roundDiv(toNanos(a), toNanos(b), scale);").unwrap().data(), &Data::Decimal("0.333".parse().unwrap()));
    assert_eq!(
        runtime_failure("return duration('PT1S') / 0;").message,
        "division by zero"
    );
    assert_eq!(
        evaluate("return duration('PT10S') / 2;").unwrap().data(),
        &Data::Duration("PT5S".parse().unwrap())
    );
    let failure = runtime_failure("return slice('a😀b',1,9);");
    assert_eq!(failure.code, "CAL015");
    assert!(failure.message.contains("end 9") && failure.message.contains("length 3"));
    assert_eq!(
        runtime_failure("return iter.jsonLines(' ').collect();").code,
        "CAL016"
    );
}

#[test]
fn provider_failure_keeps_safe_reason_issues_and_a_bounded_public_explanation() {
    let cause = wes_core::ErrorValue::new(
        wes_core::ErrorId::new("provider-failure").unwrap(),
        "HTTP001",
        "query parameter limit: number must be 1..50",
        vec![wes_core::ValidationIssue {
            path: "/arguments/limit".into(),
            code: "TYP005".into(),
            message: "number must be 1..50".into(),
        }],
        None,
    )
    .unwrap();
    let failure = wes_engine::calc::Failure::provider(wes_language::Span::at(0), cause.clone());
    assert!(failure.message.contains("HTTP001: query parameter limit"));
    assert_eq!(failure.issues, cause.issues());
    assert_eq!(failure.cause.as_ref().unwrap().id(), cause.id());
    let private = cause.with_policy(&wes_core::flow::FlowPolicy::default().private());
    let failure = wes_engine::calc::Failure::provider(wes_language::Span::at(0), private);
    assert!(!failure.message.contains("limit"));
    assert!(failure.issues.is_empty());
    let long = wes_core::ErrorValue::new(
        wes_core::ErrorId::new("long-failure").unwrap(),
        "RUN001",
        "界".repeat(10000),
        vec![],
        None,
    )
    .unwrap();
    assert!(
        wes_engine::calc::Failure::provider(wes_language::Span::at(0), long)
            .message
            .chars()
            .count()
            < 4200
    );
}

#[test]
fn missing_return_identifies_the_named_function_and_preserves_the_error_code() {
    let mut machine = machine(
        "function classify(n) { if (n > 0) { return 1; } } return classify(-1);",
        Limits::default(),
    )
    .unwrap();
    loop {
        match machine.poll(&CancellationToken::new()) {
            Err(error) => {
                assert_eq!(error.code, "CAL013");
                assert!(error.message.contains("classify"));
                assert!(error.message.contains("every executed path"));
                break;
            }
            Ok(Step::Yield) => (),
            other => panic!("unexpected {other:?}"),
        }
    }
}

#[test]
fn temporal_member_diagnostics_suggest_only_the_matching_kind() {
    for (body, wanted, absent) in [
        (
            "return instant('2000-01-01T00:00:00Z').hour;",
            "utcParts",
            "toSeconds",
        ),
        ("return duration('PT1S').seconds;", "toSeconds", "utcParts"),
    ] {
        let program = calc::parse_body(body, 0, Package::standard()).unwrap();
        let error = calc::analyze(
            Arc::new(program),
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| None,
            },
        )
        .unwrap_err();
        assert_eq!(error.code, "CAL004");
        assert!(error.message.contains(wanted), "{}", error.message);
        assert!(!error.message.contains(absent), "{}", error.message);
    }
    let body = "const count = 1; count = 2; return count;";
    let program = calc::parse_body(body, 0, Package::standard()).unwrap();
    let error = calc::analyze(
        Arc::new(program),
        calc::Environment {
            catalogue: &Catalogue::new(),
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
    )
    .unwrap_err();
    assert_eq!(error.code, "CAL011");
    assert!(error.message.contains("use let"));
    let error = calc::parse_body(
        "let row = {value:1}; row.value = 2; return row;",
        0,
        Package::standard(),
    )
    .unwrap_err();
    assert!(error.message.contains("local let variable"));
}
