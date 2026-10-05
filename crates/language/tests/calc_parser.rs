use wes_language::{
    Expression, SourceText,
    calc::{self, Binary, ExprKind, Package},
};

#[test]
fn flat_ast_uses_package_precedence_and_captures_functions_and_loops() {
    let source = "return 1 + 2 * 3;";
    let normal = calc::parse_body(source, 0, Package::standard()).unwrap();
    assert!(matches!(
        normal.expressions.last().unwrap().kind,
        ExprKind::Binary(Binary::Add, _, _)
    ));
    let changed = calc::DEFAULT_PACKAGE.replace(
        "operation: add, precedence: 5",
        "operation: add, precedence: 7",
    );
    let changed = calc::parse_body(
        source,
        0,
        std::sync::Arc::new(Package::load(&changed).unwrap()),
    )
    .unwrap();
    assert!(matches!(
        changed.expressions.last().unwrap().kind,
        ExprKind::Binary(Binary::Mul, _, _)
    ));
    let code = "function fact(n) { if (n <= 1) return 1; return n * fact(n - 1); } let sum = 0; for (const x of [1,2]) { sum = sum + x; } while (false) { continue; break; } return [1,2].map(x => fact(x));";
    let parsed = calc::parse_body(code, 0, Package::standard()).unwrap();
    assert_eq!(parsed.functions.len(), 2);
}

#[test]
fn calc_context_keeps_comments_quotes_and_output_bindings_separate() {
    let text = ":calc /* { ignored */ {\nconst text = '}'; // }\nreturn { name: text, emoji: '\\uD83D\\uDE00' };\n} > result *> failed\n:help";
    let source = SourceText::new("test", text);
    let parsed = wes_language::parse(&source);
    assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
    assert_eq!(parsed.script.statements.len(), 2);
    let statement = &parsed.script.statements[0];
    assert_eq!(statement.binding.as_ref().unwrap().name.text, "result");
    assert_eq!(
        statement.error_binding.as_ref().unwrap().name.text,
        "failed"
    );
    let Expression::Calculation(body) = &statement.expression else {
        panic!("calc")
    };
    assert!(body.text.contains("ignored"));
    let canonical = parsed.script.to_string();
    assert!(
        wes_language::parse(&SourceText::new("printed", canonical))
            .diagnostics
            .is_empty()
    );
    let ordinary = wes_language::parse(&SourceText::new(
        "data",
        "catalog echo value:\":calc { return 3; }\"",
    ));
    assert!(matches!(
        &ordinary.script.statements[0].expression,
        Expression::Call(_)
    ));
    assert!(
        !wes_language::parse(&SourceText::new(
            "template",
            ":def computation as :calc { return 1; }"
        ))
        .diagnostics
        .is_empty()
    );
}

#[test]
fn malformed_calc_and_unicode_diagnostics_are_bounded_and_source_aligned() {
    for code in [
        ":calc { return 'unterminated; }",
        ":calc { return '\\uD800'; }",
        ":calc { return 1 + ; }",
        ":calc { return { x: 1 };",
        ":calc { const f = (a,) => a; return f(1); }",
    ] {
        assert!(
            !wes_language::parse(&SourceText::new("bad", code))
                .diagnostics
                .is_empty(),
            "{code}"
        );
    }
    let text = "catalog echo value:hi\n:calc { const s = '😀'; return ; }";
    let source = SourceText::new("unicode", text);
    let parsed = wes_language::parse(&source);
    let error = parsed.diagnostics.first().unwrap();
    assert_eq!(source.slice(error.span).unwrap(), ";");
    assert_eq!(
        source.utf16_offset(error.span.start()).unwrap() + 2,
        error.span.start()
    );
    let deep = format!("return {}1{};", "(".repeat(300), ")".repeat(300));
    assert!(calc::parse_body(&deep, 0, Package::standard()).is_err());
    // A wide left-associative chain is flat; cloning/dropping it must not recurse through nodes.
    let long = format!("return {}1;", "1 + ".repeat(10_000));
    let program = calc::parse_body(&long, 0, Package::standard()).unwrap();
    assert!(program.clone().expressions.len() > 20_000);
}

#[test]
fn nested_syntax_budget_is_safe_on_a_normal_worker_stack() {
    // Do not enlarge the test thread stack: this protects normal analysis worker callers.
    for depth in [48, 64, 128, 256, 1_000] {
        for (open, close) in [("(", ")"), ("[", "]"), ("{x:", "}")] {
            let source = format!("return {}1{};", open.repeat(depth), close.repeat(depth));
            let result = calc::parse_body(&source, 0, Package::standard());
            if depth == 48 {
                assert!(result.is_ok(), "{open} {depth}");
            } else {
                assert!(result.is_err(), "{open} {depth}");
            }
        }
        let functions = format!(
            "{}return 1;{}",
            "function f(){".repeat(depth),
            "} return f();".repeat(depth)
        );
        assert!(calc::parse_body(&functions, 0, Package::standard()).is_err());
    }
}
