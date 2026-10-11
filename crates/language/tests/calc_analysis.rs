use std::{cell::Cell, sync::Arc};
use wes_core::{
    Primitive, Shape,
    capability::{Capability, Catalogue, Parameter, ProviderDescription, Safety},
    contracts::ContractRegistry,
};
use wes_language::calc::{self, Package};

fn catalogue() -> Catalogue {
    let mut capability = Capability::new(["get"], Shape::Primitive(Primitive::Text), Safety::Safe);
    capability
        .parameters
        .push(Parameter::new("id", Shape::Primitive(Primitive::Int), true));
    let mut catalogue = Catalogue::new();
    catalogue.register(ProviderDescription::new("catalog", [capability], vec![]).unwrap());
    catalogue
}
fn compile(code: &str) -> Result<calc::Compiled, wes_language::Diagnostic> {
    let program = Arc::new(calc::parse_body(code, 0, Package::standard())?);
    calc::analyze(
        program,
        calc::Environment {
            catalogue: &catalogue(),
            contracts: &ContractRegistry::new(),
            workspace: &|name| {
                if name == "rows" {
                    Some(Shape::Unknown)
                } else {
                    None
                }
            },
        },
    )
}

#[test]
fn diagnostic_json_decode_captures_contract_and_preserves_optional_result_shape() {
    let compiled = compile("return decodeJson('1','Int');").unwrap();
    assert_eq!(
        compiled.output_shape(),
        calc::json_decode_shape(Shape::Primitive(Primitive::Int))
    );
    assert_eq!(compiled.contracts.len(), 1);
    assert!(compiled.purity.external_operations.is_empty());
    assert_eq!(
        compile("return decodeJson('null');")
            .unwrap()
            .output_shape(),
        calc::json_decode_shape(Shape::Unknown)
    );
    for source in [
        "const f=decodeJson; return f('1');",
        "return decodeJson('1',$rows);",
        "return decodeJson('1','MissingContract');",
        "return decodeJson(1);",
    ] {
        assert!(compile(source).is_err(), "{source}");
    }
}

#[test]
fn provably_non_callable_values_fail_before_execution_without_rejecting_dynamic_calls() {
    for source in [
        "const length=5; return length([200,503]);",
        "const keys='text'; return keys();",
        "const f=[1]; return f();",
        "const f={value:3}; return f();",
        "const f=some(3); return f();",
        "const f=3; const alias=f; return alias();",
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, "CAL004", "{source}");
        assert!(error.message.contains("not callable"), "{error}");
    }
    for source in [
        "let length=5; length=x=>x; return length(3);",
        "const f=x=>x; const alias=f; return alias(3);",
        "const f={length:x=>x}; return f.length(3);",
        "function run(f){return f(3);} return run(x=>x);",
        "return $rows();",
    ] {
        assert!(compile(source).is_ok(), "{source}");
    }
}
#[test]
fn scope_arity_const_and_loop_errors_are_reported_before_execution() {
    for (source, code) in [
        ("return missing;", "CAL010"),
        ("const x=1; const x=2; return x;", "CAL010"),
        ("const x=1; x=2; return x;", "CAL011"),
        ("break; return 1;", "CAL013"),
        (
            "while (true) { const f=()=>{break;}; break; } return 1;",
            "CAL013",
        ),
        ("function f(x) { return x; } return f();", "CAL012"),
        ("return 1 + 'x';", "CAL004"),
        ("if (1) return 2; return 3;", "CAL004"),
        ("return call('catalog',['get'],{id:'wrong'});", "CAL004"),
        (
            "const c=call; return c('catalog',['get'],{id:1});",
            "CAL002",
        ),
        (
            "let provider='catalog'; return call(provider,['get'],{id:1});",
            "CAL002",
        ),
    ] {
        assert_eq!(compile(source).unwrap_err().code, code, "{source}");
    }
    assert!(
        compile("function fact(n) { if (n <= 1) return 1; return n * fact(n-1); } return fact(5);")
            .is_ok()
    );
    assert!(compile("let n=1; const f=()=>n; n=2; return f();").is_ok());
}

#[test]
fn metadata_and_workspace_dependencies_are_captured_without_callable_access() {
    let lookups = Cell::new(0);
    let mut catalogue = catalogue();
    let mut contracts = ContractRegistry::new();
    contracts.load("types:\n  Label: {base: Text}\n").unwrap();
    let program=Arc::new(calc::parse_body("if (false) { call('catalog',['get'],{id:1}); } const x=check('Label',$rows); return x;",0,Package::standard()).unwrap());
    let compiled = calc::analyze(
        program,
        calc::Environment {
            catalogue: &catalogue,
            contracts: &contracts,
            workspace: &|_| {
                lookups.set(lookups.get() + 1);
                Some(Shape::Unknown)
            },
        },
    )
    .unwrap();
    assert!(compiled.effectful());
    assert_eq!(compiled.calls.len(), 1);
    assert_eq!(compiled.contracts.len(), 1);
    assert_eq!(compiled.workspace.len(), 1);
    assert_eq!(lookups.get(), 1);
    catalogue.register(
        ProviderDescription::new(
            "catalog",
            [Capability::new(
                ["get"],
                Shape::Primitive(Primitive::Bool),
                Safety::Unsafe,
            )],
            vec![],
        )
        .unwrap(),
    );
    assert_eq!(
        compiled.calls.values().next().unwrap().capability.result,
        Shape::Primitive(Primitive::Text)
    );
    assert_eq!(
        compiled.contracts.values().next().unwrap().shape(),
        Shape::Primitive(Primitive::Text)
    );
}

#[test]
fn large_expression_analysis_is_iterative_and_package_arity_is_authoritative() {
    let code = format!("return {}1;", "1 + ".repeat(10_000));
    assert!(compile(&code).is_ok());
    let source = calc::DEFAULT_PACKAGE.replace(
        "operation: range, min: 1, max: 3",
        "operation: range, min: 2, max: 2",
    );
    let program = Arc::new(
        calc::parse_body(
            "return range(3);",
            0,
            Arc::new(Package::load(&source).unwrap()),
        )
        .unwrap(),
    );
    assert!(
        calc::analyze(
            program,
            calc::Environment {
                catalogue: &Catalogue::new(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| None
            }
        )
        .is_err()
    );
}

#[test]
fn static_errors_explain_expected_actual_and_selected_declarations() {
    for (source, code, fragments) in [
        (
            "return range();",
            "CAL012",
            vec!["operation 'range'", "expects 1..3", "received 0"],
        ),
        (
            "return [1].map();",
            "CAL012",
            vec!["method 'map'", "expects 1", "received 0"],
        ),
        (
            "return [1].map(x=>x, 2);",
            "CAL012",
            vec!["method 'map'", "expects 1", "received 2"],
        ),
        (
            "function f(x,y){return x;} return f(1);",
            "CAL012",
            vec!["function 'f'", "expects 2", "received 1"],
        ),
        (
            "return ((x)=>x)();",
            "CAL012",
            vec!["function", "expects 1", "received 0"],
        ),
        (
            "return (1).text();",
            "CAL002",
            vec!["'text'", "method syntax", "function"],
        ),
        (
            "if(1) return 0;",
            "CAL004",
            vec!["requires Bool", "received Int"],
        ),
        (
            "for(const x of 1) { return x; } return 0;",
            "CAL004",
            vec!["requires List or Iter", "received Int"],
        ),
        (
            "return 1 + true;",
            "CAL004",
            vec!["Int or Decimal or Text", "received Bool"],
        ),
        (
            "return 1 + 'payload-canary';",
            "CAL004",
            vec!["explicit conversion", "Int and Text"],
        ),
        (
            "return iter.missing('payload-canary');",
            "CAL010",
            vec!["Iter operation 'missing'"],
        ),
        (
            "return iter.use('absent-recipe', []);",
            "CAL010",
            vec!["iterator recipe 'absent-recipe'"],
        ),
        (
            "return call('absent-provider',['get'],{});",
            "CAL010",
            vec!["provider 'absent-provider'"],
        ),
        (
            "return call('catalog',['absent-capability'],{});",
            "CAL010",
            vec!["capability 'absent-capability'", "provider 'catalog'"],
        ),
        (
            "return call('catalog',['get'],{id:'payload-canary'});",
            "CAL004",
            vec!["argument 'id'", "requires Int", "received Text"],
        ),
        (
            "return call('catalog',['get'],false);",
            "CAL004",
            vec!["require Record", "received Bool"],
        ),
    ] {
        let error = compile(source).unwrap_err();
        assert_eq!(error.code, code, "{source}");
        for fragment in fragments {
            assert!(
                error.message.contains(fragment),
                "{source}: {} lacks {fragment}",
                error.message
            );
        }
        assert!(!error.message.contains("payload-canary"));
    }
}

#[test]
fn diagnostic_declaration_labels_escape_controls_and_bound_unicode_names() {
    let error = compile("return call('missing\\nprovider',['get'],{});").unwrap_err();
    assert!(error.message.contains("missing\\nprovider"));
    assert!(!error.message.contains('\n'));
    let error = compile(&format!(
        "return call('{}',['get'],{{}});",
        "ü".repeat(10_000)
    ))
    .unwrap_err();
    assert!(error.message.contains('…'));
    assert!(error.message.len() < 1024);
}

#[test]
fn pure_assertion_uses_resolved_effects_even_in_dead_helpers() {
    for source in [
        ":calc pure { return call('catalog',['get'],{id:1}); }",
        ":calc pure { if (false) { call('catalog',['get'],{id:1}); } return 1; }",
        ":calc pure { function unused() { return call('catalog',['get'],{id:1}); } return 1; }",
    ] {
        let program = calc::parse_context(source, 0, Package::standard()).unwrap();
        let result = calc::analyze(
            program.into(),
            calc::Environment {
                catalogue: &catalogue(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| None,
            },
        );
        assert_eq!(result.unwrap_err().code, "CAL009", "{source}");
    }
    for source in [
        ":calc pure { let n=0; n=n+1; return n; }",
        ":calc pure { return $rows; }",
    ] {
        let program = calc::parse_context(source, 0, Package::standard()).unwrap();
        let result = calc::analyze(
            program.into(),
            calc::Environment {
                catalogue: &catalogue(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| Some(Shape::Unknown),
            },
        )
        .unwrap();
        assert!(!result.effectful());
    }
    let package = Package::load(&calc::DEFAULT_PACKAGE.replace("  call:", "  invoke:")).unwrap();
    let source = ":calc pure { return invoke('catalog',['get'],{id:1}); }";
    let program = calc::parse_context(source, 0, Arc::new(package)).unwrap();
    let result = calc::analyze(
        program.into(),
        calc::Environment {
            catalogue: &catalogue(),
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
    );
    assert_eq!(result.unwrap_err().code, "CAL009");
}

#[test]
fn ordered_comparisons_check_known_operands_but_leave_unknowns_to_runtime() {
    for operator in ["<", "<=", ">", ">="] {
        for operands in [("1", "1.5"), ("1.5", "1"), ("true", "false"), ("[]", "[]")] {
            let source = format!("return {} {operator} {};", operands.0, operands.1);
            assert_eq!(compile(&source).unwrap_err().code, "CAL004", "{source}");
        }
        for operands in [("1", "2"), ("1.0", "2.0"), ("'a'", "'b'"), ("$rows", "2")] {
            let source = format!("return {} {operator} {};", operands.0, operands.1);
            assert!(compile(&source).is_ok(), "{source}");
        }
    }
    assert!(compile("return 1 == 1.0;").is_ok()); // equality retains its structural semantics
}

#[test]
fn calculation_output_predictions_are_conservative_across_control_flow() {
    for source in [
        "return 2.4;",
        "if (true) { return 1.2; } else { return 3.4; }",
        "const f=()=>{return 'x';}; return 2.4;",
        "if (true) { return 1.2; } return 2.4;",
    ] {
        assert_eq!(
            compile(source).unwrap().output_shape(),
            Shape::Primitive(Primitive::Decimal),
            "{source}"
        );
    }
    for source in [
        "if (true) { return 1.2; }",
        "if (true) { return 1; } else { return 2.4; }",
        "let n=2.4; return n;",
        "return $rows;",
        "while (true) { break; return 1; } return 2.4;",
    ] {
        assert_eq!(
            compile(source).unwrap().output_shape(),
            Shape::Unknown,
            "{source}"
        );
    }
    let Shape::Record(record) = compile("return {rate:2.4};").unwrap().output_shape() else {
        panic!("record");
    };
    assert_eq!(
        record.field("rate"),
        Some(&Shape::Primitive(Primitive::Decimal))
    );
}

#[test]
fn known_fields_iterator_sources_and_recipe_misuse_are_rejected_without_execution() {
    for (source, hint) in [
        (
            "const response={status:503}; return response.retryAfter;",
            "'retryAfter'",
        ),
        ("return iter.words(503);", "expects Text; received Int"),
        ("return iter.keys([]);", "expects Record"),
        ("return iter.items('x');", "expects List"),
        ("return iter.lines('one')[0];", "take(1).collect()"),
        ("return length(iter.lines('one'));", ".count()"),
        ("return iter.lines('one').length();", ".count()"),
        ("return true ? 1 : 0;", "if/else"),
    ] {
        let error = compile(source).unwrap_err();
        assert!(error.message.contains(hint), "{source}: {}", error.message);
    }
    for source in [
        "return $rows.anyField;",
        "return iter.words($rows);",
        "return {x:1}.keys();",
        "return {length:()=>7}.length();",
        "return [1,2][0];",
        "return interval(instant('2030-01-01T00:00:00Z'),instant('2030-01-01T00:01:00Z')).start;",
    ] {
        assert!(compile(source).is_ok(), "{source}");
    }
}

#[test]
fn new_finite_operations_predict_structural_types_and_reject_known_bad_sources() {
    assert_eq!(
        compile("return [none,some(1)];").unwrap().output_shape(),
        Shape::List(Box::new(Shape::Option(Box::new(Shape::Primitive(
            Primitive::Int
        )))))
    );
    for source in ["return join(['a','b'],',');", "return slice('a😀b',1,2);"] {
        assert_eq!(
            compile(source).unwrap().output_shape(),
            Shape::Primitive(Primitive::Text)
        );
    }
    let Shape::Record(updated) =
        compile("return withFields({status:200,tag:'old'},{status:'changed',extra:true});")
            .unwrap()
            .output_shape()
    else {
        panic!("record output")
    };
    assert_eq!(updated.name(), "");
    assert_eq!(
        updated.field("status"),
        Some(&Shape::Primitive(Primitive::Text))
    );
    assert_eq!(
        updated.field("extra"),
        Some(&Shape::Primitive(Primitive::Bool))
    );
    for source in [
        "return join([1],',');",
        "return slice(1,0);",
        "return slice('x',0.0);",
        "return withFields({},1);",
        "return iter.captures(1,'x');",
    ] {
        assert_eq!(compile(source).unwrap_err().code, "CAL004", "{source}");
    }
}

#[test]
fn definite_flow_errors_are_early_without_hoisting_or_rejecting_closures() {
    for source in [
        "return f();function f(){return 1;}",
        "const x=x;return x;",
        "const x=1;",
        "function f(){const x=1;}return f();",
        "if(true){const x=1;}else{return 2;}",
    ] {
        assert_eq!(compile(source).unwrap_err().code, "CAL013", "{source}");
    }
    for source in [
        "const f=()=>later;const later=1;return f();",
        "function f(n){if(n==0)return 1;return f(n-1);}return f(3);",
        "while(true){return 1;}",
        "let x=true;if(x)return 1;",
    ] {
        assert!(compile(source).is_ok(), "{source}");
    }
}

#[test]
fn eager_local_effects_remain_legal_and_lazy_callbacks_check_complete_helpers() {
    assert!(compile("let n=0;return [1,2].map(x=>{n=n+x;return n;});").is_ok());
    assert_eq!(
        compile("let n=0;return iter.items([1,2]).map(x=>{n=n+x;return n;});")
            .unwrap_err()
            .code,
        "CAL009"
    );
    assert_eq!(
        compile("let n=0;return iter.items([1,2]).filter(x=>true).map(x=>n);")
            .unwrap_err()
            .code,
        "CAL009"
    );
    assert!(compile("const xs=iter.items([1,2]).map(x=>helper(x));function helper(x){return x+1;}return xs.collect();").is_ok());
}

#[test]
fn independent_name_mutation_arity_and_record_failures_have_distinct_codes() {
    for (source, code) in [
        ("return $absent;", "CAL010"),
        ("const value=1;value=2;return value;", "CAL011"),
        ("return join(['a']);", "CAL012"),
        ("return {a:1,a:2};", "CAL014"),
    ] {
        assert_eq!(compile(source).unwrap_err().code, code, "{source}");
    }
}

#[test]
fn map_prediction_uses_only_proven_callback_returns() {
    let text = Shape::List(Box::new(Shape::Primitive(Primitive::Text)));
    for source in [
        "return [1,2].map(x=>text(x));",
        "const draw=x=>text(x); return [1].map(draw);",
        "return [1].map(x=>{ if(true) return 'a'; return 'b'; });",
    ] {
        assert_eq!(compile(source).unwrap().output_shape(), text, "{source}");
    }
    for source in [
        "return [1].map(x=>x);",
        "let draw=x=>'a'; return [1].map(draw);",
        "return [1].map(x=>{ if(true) return 'a'; });",
        "return [1].map(x=>{ if(true) return 'a'; return 1; });",
    ] {
        assert_eq!(
            compile(source).unwrap().output_shape(),
            Shape::List(Box::new(Shape::Unknown)),
            "{source}"
        );
    }
}

#[test]
fn every_bare_operation_name_is_an_ordinary_local_and_parameter_name() {
    for (name, _) in Package::standard()
        .operations()
        .filter(|(name, _)| !name.contains('.'))
    {
        for source in [
            format!("const {name}=7; return {name};"),
            format!("return [1,2].map({name} => {name}+1);"),
            format!("function {name}(value) {{ return value+1; }} return {name}(2);"),
        ] {
            assert!(compile(&source).is_ok(), "{source}");
        }
    }
    // Adding an operation does not retroactively reserve an existing lexical name.
    let package = Arc::new(Package::load(&calc::DEFAULT_PACKAGE.replace(
        "  length: {operation: length, min: 1, max: 1}",
        "  length: {operation: length, min: 1, max: 1}\n  measure: {operation: length, min: 1, max: 1}",
    )).unwrap());
    let program = Arc::new(
        calc::parse_body(
            "const measure=7; return [1,2].map(measure=>measure+1);",
            0,
            package,
        )
        .unwrap(),
    );
    calc::analyze(
        program,
        calc::Environment {
            catalogue: &catalogue(),
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
    )
    .unwrap();
}

#[test]
fn shadowed_functions_resolve_by_symbol_without_builtin_metadata_or_arity() {
    for source in [
        "function length(a,b) { return a+b; } return length(1,2);",
        "const call=x=>x+1; return call(4);",
        "const check=x=>x+1; return check(4);",
        "const parseJson=x=>x+1; return parseJson(4);",
        "function map(x){return x+1;} return iter.items([1,2]).map(x=>map(x)).collect();",
        "const count=7; return iter.items([1,2]).count()+count;",
        "const length=3; return [1,2].length()+length;",
    ] {
        let compiled = compile(source).unwrap_or_else(|e| panic!("{source}: {e:?}"));
        assert!(!compiled.effectful(), "{source}");
        assert!(
            compiled.calls.is_empty() && compiled.contracts.is_empty(),
            "{source}"
        );
    }
    assert_eq!(
        compile("function length(a,b){return a+b;} return length(1);")
            .unwrap_err()
            .code,
        "CAL012"
    );
    for source in [
        "const length=length('abc');return length;",
        "return count(1);function count(x){return x;}",
        "const count=1; { const count=count+1; } return count;",
    ] {
        assert_eq!(compile(source).unwrap_err().code, "CAL013", "{source}");
    }
    assert_eq!(
        compile("const count=1;const count=2;return count;")
            .unwrap_err()
            .code,
        "CAL010"
    );
}

#[test]
fn lexical_parameters_shadow_operations_without_becoming_workspace_dependencies() {
    let parameters = [
        ("duration".to_string(), Shape::Primitive(Primitive::Int)),
        ("count".to_string(), Shape::Primitive(Primitive::Int)),
    ]
    .into();
    let program =
        Arc::new(calc::parse_body("return duration+count;", 0, Package::standard()).unwrap());
    let compiled = calc::analyze_with_parameters(
        program,
        calc::Environment {
            catalogue: &catalogue(),
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
        &parameters,
    )
    .unwrap();
    assert_eq!(compiled.parameters.len(), 2);
    assert!(compiled.workspace.is_empty() && !compiled.effectful());
    assert_eq!(compiled.output_shape(), Shape::Primitive(Primitive::Int));
}

#[test]
fn purity_and_provider_capture_follow_resolved_functions_instead_of_their_spelling() {
    for source in [
        "function count(x){return call('catalog',['get'],{id:x});} return iter.items([1]).map(x=>count(x)).collect();",
        "function map(x){return call('catalog',['get'],{id:x});} return iter.items([1]).map(x=>map(x)).collect();",
        "let count=0; function length(x){count=count+1;return x;} return iter.items([1]).map(x=>length(x)).collect();",
        "let count=0; return [1].sortBy(x=>{count=count+1;return x;});",
    ] {
        assert_eq!(compile(source).unwrap_err().code, "CAL009", "{source}");
    }
    let compiled = compile("const count=0; function length(x){return call('catalog',['get'],{id:x});} return length(count);").unwrap();
    assert!(compiled.effectful());
    assert_eq!(compiled.calls.len(), 1);
    let path = &compiled.calls.values().next().unwrap().capability.path;
    assert_eq!(path, &["get"]);
    for (source, allowed) in [
        ("const call=x=>x+1;return call(4);", true),
        ("const check=x=>x+1;return check(4);", true),
        (
            "function count(x){return call('catalog',['get'],{id:x});}return count(1);",
            false,
        ),
    ] {
        let mut program = calc::parse_body(source, 0, Package::standard()).unwrap();
        program.requires_pure = true;
        let result = calc::analyze(
            Arc::new(program),
            calc::Environment {
                catalogue: &catalogue(),
                contracts: &ContractRegistry::new(),
                workspace: &|_| None,
            },
        );
        if allowed {
            result.unwrap();
        } else {
            assert_eq!(result.unwrap_err().code, "CAL009");
        }
    }
}

#[test]
fn keywords_literals_and_iterator_namespace_keep_their_reserved_roles() {
    for name in [
        "const", "let", "function", "if", "else", "while", "for", "of", "return", "break",
        "continue", "true", "false", "none", "iter",
    ] {
        for source in [
            format!("const {name}=1;return 1;"),
            format!("return [1].map({name}=>1);"),
        ] {
            assert!(compile(&source).is_err(), "{source}");
        }
    }
}

#[test]
fn documented_provider_call_path_is_a_literal_list_and_captures_the_real_operation() {
    let mut capability = Capability::new(["read"], Shape::Primitive(Primitive::Text), Safety::Safe);
    capability.parameters.push(Parameter::new(
        "key",
        Shape::Primitive(Primitive::Text),
        true,
    ));
    let mut catalogue = Catalogue::new();
    catalogue.register(ProviderDescription::new("sample", [capability], vec![]).unwrap());
    let help = calc::Operation::Call.help();
    assert_eq!(help.parameters[1], ("path", "List<Text>"));
    let program = Arc::new(
        calc::parse_body(&format!("return {};", help.example), 0, Package::standard()).unwrap(),
    );
    let compiled = calc::analyze(
        program,
        calc::Environment {
            catalogue: &catalogue,
            contracts: &ContractRegistry::new(),
            workspace: &|_| None,
        },
    )
    .unwrap();
    assert_eq!(compiled.calls.len(), 1);
}
