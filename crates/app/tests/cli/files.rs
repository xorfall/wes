use std::{
    path::Path,
    process::{Command, Output},
};

fn run(base: &Path, flags: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_wes"))
        .current_dir(base)
        .args(["--home", "home"])
        .args(flags)
        .output()
        .unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
fn last(output: &Output) -> serde_json::Value {
    let text = String::from_utf8_lossy(&output.stdout);
    serde_json::from_str(text.lines().last().unwrap().split_once(": ").unwrap().1).unwrap()
}

#[test]
fn file_mode_exclusions_and_missing_values_do_not_open_home() {
    for flags in [
        vec!["--file"],
        vec!["--file", "x", "--command", ":help"],
        vec!["--file", "x", "--serve", "0"],
        vec!["--file", "x", "--file", "y"],
        vec!["--file", "x", "--site", "gui"],
    ] {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(run(root.path(), &flags).status.code(), Some(2));
        assert!(!root.path().join("home").exists());
    }
}

#[test]
fn invalid_file_inputs_do_not_open_home() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("binary"), [0xff]).unwrap();
    std::fs::write(
        root.path().join("large"),
        vec![b' '; wes_engine::source::max_source_bytes() + 1],
    )
    .unwrap();
    for name in ["missing", ".", "binary", "large", "-"] {
        let output = run(root.path(), &["--file", name]);
        assert_eq!(output.status.code(), Some(1));
        assert!(!root.path().join("home").exists());
    }
}

#[test]
fn file_batch_matches_inline_without_shell_interpolation() {
    let root = tempfile::tempdir().unwrap();
    let source = ":calc {\r\n return {text: \"$HOME $(whoami) `pwd` 🦀\", values: [1, 2]};\r\n} > first\r\n:calc { return $first; } > final";
    std::fs::write(root.path().join("batch.wes"), source).unwrap();
    let file = run(root.path(), &["--file", "batch.wes"]);
    success(&file);
    let inline = run(root.path(), &["--workspace", "inline", "--command", source]);
    success(&inline);
    assert_eq!(last(&file), last(&inline));
    assert_eq!(last(&file)["text"], "$HOME $(whoami) `pwd` 🦀");
}

#[test]
fn file_syntax_errors_use_utf16_locations_and_reject_entire_batch() {
    let root = tempfile::tempdir().unwrap();
    let source = ":calc { return 12; } > never\r\n:calc { return \"🦀\" + ; }";
    std::fs::write(root.path().join("broken.wes"), source).unwrap();
    let parsed_source = wes_language::SourceText::new("test", source);
    let parsed = wes_language::parse(&parsed_source);
    assert!(!parsed.diagnostics.is_empty());
    let output = run(root.path(), &["--file", "broken.wes"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    for diagnostic in parsed.diagnostics {
        let p = parsed_source.position(diagnostic.span.start()).unwrap();
        assert!(
            stderr.contains(&format!("broken.wes:{}:{}:", p.line, p.column)),
            "{stderr}"
        );
    }
    assert!(output.stdout.is_empty());
    assert!(
        !run(root.path(), &["--command", ":calc { return $never; }"])
            .status
            .success()
    );
}

#[test]
fn file_semantic_errors_keep_preceding_accepted_declarations() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("partial.wes"),
        ":calc { return 42; } > kept\nmissingProvider missingOperation",
    )
    .unwrap();
    let output = run(root.path(), &["--file", "partial.wes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("partial.wes:2:1:"));
    let restored = run(root.path(), &["--command", ":calc { return $kept; }"]);
    success(&restored);
    assert_eq!(last(&restored), 42);
}

#[test]
fn mixed_controls_are_not_reinterpreted_as_sequential_script_lines() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("mixed.wes"),
        ":workspace save \"forbidden\"\n:calc { return 1; }",
    )
    .unwrap();
    let output = run(root.path(), &["--file", "mixed.wes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("ENG005"));
    assert!(output.stdout.is_empty());
}

#[test]
fn file_runtime_errors_identify_file_and_node_without_invented_location() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("runtime.wes"),
        ":calc { return 1 / 0; } > broken",
    )
    .unwrap();
    let output = run(root.path(), &["--file", "runtime.wes"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("runtime.wes: "), "{stderr}");
    assert!(!stderr.contains("runtime.wes:1:"), "{stderr}");
}

#[test]
fn actual_report_project_uses_script_base_and_reopens_without_source_files() {
    let root = tempfile::tempdir().unwrap();
    let project = root.path().join("project");
    std::fs::create_dir(&project).unwrap();
    std::fs::write(
        project.join("report.wes"),
        include_str!("../../../../examples/scripts/report/report.wes"),
    )
    .unwrap();
    std::fs::write(
        project.join("types.yaml"),
        include_str!("../../../../examples/scripts/report/types.yaml"),
    )
    .unwrap();
    let output = run(root.path(), &["--file", "project/report.wes"]);
    success(&output);
    assert_eq!(
        last(&output),
        serde_json::json!({"values": [36, 60], "presentation": "Selected: 36 60"})
    );
    assert!(root.path().join("home").is_dir());
    assert!(!project.join("home").exists());
    std::fs::remove_file(project.join("report.wes")).unwrap();
    std::fs::remove_file(project.join("types.yaml")).unwrap();
    let reopened = run(root.path(), &["--command", ":calc { return $report; }"]);
    success(&reopened);
    assert_eq!(last(&reopened), last(&output));
}

#[test]
fn credential_stdin_cannot_share_a_file_with_interactive_input() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("interactive.wes"),
        "@interactive sh run cmd:\"read answer\"",
    )
    .unwrap();
    let output = run(
        root.path(),
        &["--file", "interactive.wes", "--credentials-stdin"],
    );
    assert_eq!(output.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive"));
    assert!(!root.path().join("home").exists());
}

#[cfg(unix)]
#[test]
fn environment_flags_keep_launch_base_and_target_cwd_stays_explicit() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local, cwd: /}}\nenvironments: {dev: {imports: {pwd: {source: {kind: process, bin: /bin/pwd}, bind: {target: local}}}}}").unwrap();
    std::fs::write(root.path().join("project/run.wes"), "pwd run > location").unwrap();
    let output = run(
        root.path(),
        &[
            "--env-file",
            "env.yaml",
            "--env",
            "dev",
            "--file",
            "project/run.wes",
        ],
    );
    success(&output);
    assert_eq!(last(&output)["stdout"], "Lwo=");
    success(&run(
        root.path(),
        &["--command", ":env export file:env.lock.json"],
    ));
    let locked = run(
        root.path(),
        &[
            "--workspace",
            "locked",
            "--env-lock",
            "env.lock.json",
            "--env",
            "dev",
            "--file",
            "project/run.wes",
        ],
    );
    success(&locked);
    assert_eq!(last(&locked)["stdout"], "Lwo=");
}

#[test]
fn standalone_environment_source_reads_use_script_base() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::fs::write(
        root.path().join("project/env.yaml"),
        "version: 1\nenvironments: {dev: {}}\n",
    )
    .unwrap();
    std::fs::write(
        root.path().join("project/plan.wes"),
        ":env plan file:env.yaml > proposed",
    )
    .unwrap();
    success(&run(root.path(), &["--file", "project/plan.wes"]));
}

#[test]
fn statement_limit_is_preserved() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("large.wes"), ":help\n".repeat(1001)).unwrap();
    let output = run(root.path(), &["--file", "large.wes"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
}

#[test]
fn relative_spec_is_captured_without_invoking_a_service() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    let descriptor = serde_json::json!({"version":1,"provider":"fixture","types":{},"operations":[{"path":["items"],"method":"GET","route":"/items","auth":[],"parameters":[],"responses":{"200":"Text"}}]});
    std::fs::write(
        root.path().join("project/provider.json"),
        descriptor.to_string(),
    )
    .unwrap();
    std::fs::write(
        root.path().join("project/import.wes"),
        ":import spec file:provider.json endpoint:https://example.invalid\n:calc { return 9; }",
    )
    .unwrap();
    let output = run(root.path(), &["--file", "project/import.wes"]);
    success(&output);
    assert_eq!(last(&output), 9);
}

#[cfg(unix)]
#[test]
fn relative_process_import_does_not_change_child_cwd() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("project")).unwrap();
    std::os::unix::fs::symlink("/bin/pwd", root.path().join("project/tool")).unwrap();
    std::fs::write(
        root.path().join("project/import.wes"),
        ":import process bin:./tool as:location\nlocation run",
    )
    .unwrap();
    let output = run(root.path(), &["--file", "project/import.wes"]);
    success(&output);
    let expected = wes_adapters::codec::encode_json(
        &wes_core::Data::Bytes(
            format!("{}\n", root.path().canonicalize().unwrap().display())
                .into_bytes()
                .into(),
        ),
        wes_adapters::codec::Limits::default(),
    )
    .unwrap();
    assert_eq!(
        last(&output)["stdout"],
        serde_json::from_slice::<serde_json::Value>(&expected).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn credential_stdin_remains_separate_from_captured_source() {
    use std::{io::Write, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {secretSlots: {key: {required: true}}, secretRefs: {key: qa/file}, imports: {vars: {source: {kind: process, bin: /usr/bin/printenv}, bind: {target: local, output: private, credentials: {QA_SECRET: {secret: key}}}}}}}").unwrap();
    std::fs::write(root.path().join("private.wes"), "vars run args:QA_SECRET").unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_wes"))
        .current_dir(root.path())
        .args([
            "--home",
            "home",
            "--env-file",
            "env.yaml",
            "--env",
            "dev",
            "--credentials-stdin",
            "--grant-provider",
            "vars",
            "--file",
            "private.wes",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"qa/file":"synthetic-file-private-sentinel"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    success(&output);
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.to_lowercase().contains("private"));
    assert!(!stderr.contains("synthetic-file-private-sentinel"));
}

#[cfg(unix)]
#[test]
fn fifo_is_rejected_without_opening_home() {
    let root = tempfile::tempdir().unwrap();
    assert!(
        Command::new("mkfifo")
            .arg(root.path().join("pipe"))
            .status()
            .unwrap()
            .success()
    );
    assert_eq!(run(root.path(), &["--file", "pipe"]).status.code(), Some(1));
    assert!(!root.path().join("home").exists());
}

#[test]
fn actual_http_inspection_project_is_checked_with_isolated_loopback_and_homes() {
    let checker = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples/http-inspection/check.py");
    let output = std::process::Command::new("python3")
        .arg(checker)
        .args(["--binary", env!("CARGO_BIN_EXE_wes")])
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).starts_with("PASS actual HTTP inspection"));
}

#[test]
fn names_snapshot_has_capture_metadata_without_changing_its_structured_rows() {
    let root = tempfile::tempdir().unwrap();
    let output = run(
        root.path(),
        &["--command", ":calc { return 42; } > total\n:list names"],
    );
    success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    assert!(text.contains("Snapshot · captured at "));
    assert!(text.contains("states and types are from that instant"));
    assert!(text.contains("Refresh with :refresh $id"));
    let rows = last(&output);
    assert!(
        rows.as_array()
            .unwrap()
            .iter()
            .any(|row| row["name"] == "$total")
    );
}

#[test]
fn invalid_stream_limits_fail_before_opening_a_home() {
    for value in ["0", "1025", "-1", "unlimited", "1.5"] {
        let root = tempfile::tempdir().unwrap();
        let output = run(root.path(), &["--max-streams", value, "--command", ":help"]);
        assert_eq!(output.status.code(), Some(2));
        assert!(!root.path().join("home").exists());
    }
}

#[test]
fn command_comments_bindings_and_runtime_locations_use_the_original_script() {
    let root = tempfile::tempdir().unwrap();
    let source = "// preceding comment 🦀\r\n:calc {\r\n  const label = '🦀';\r\n  return div(1,0);\r\n} > error_rate";
    std::fs::write(root.path().join("located.wes"), source).unwrap();
    let output = run(root.path(), &["--file", "located.wes"]);
    assert_eq!(output.status.code(), Some(1));
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("located.wes: line 4, column 10"),
        "{stderr}"
    );
    assert!(!stderr.contains("<input>"));
    assert!(!stderr.contains("source bytes"));
    let unicode = ":calc { const label = '🦀'; return div(1,0); }";
    let expected = wes_language::SourceText::new("fixture", unicode)
        .position(unicode.find("div").unwrap())
        .unwrap();
    let checked = run(
        root.path(),
        &["--workspace", "unicode", "--command", unicode],
    );
    assert!(
        String::from_utf8_lossy(&checked.stderr)
            .contains(&format!("line 1, column {}", expected.column))
    );
    // A named definition retains its definition document, not the later invocation's line map.
    let defined = run(
        root.path(),
        &[
            "--workspace",
            "definition",
            "--command",
            "// definition\n:def bad(input: Int) -> Int as :calc {\n  return div(input,0);\n}",
        ],
    );
    success(&defined);
    let invoked = run(
        root.path(),
        &["--workspace", "definition", "--command", "bad input:1"],
    );
    assert_eq!(invoked.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&invoked.stderr).contains("line 3, column 10"),
        "{}",
        String::from_utf8_lossy(&invoked.stderr)
    );
}

#[test]
fn invalid_binding_and_mixed_known_comparison_are_rejected_before_any_execution() {
    for source in [
        ":calc { return 1; } > error-rate",
        ":calc { return 1 < 1.5; }",
    ] {
        let root = tempfile::tempdir().unwrap();
        let output = run(root.path(), &["--command", source]);
        assert_eq!(output.status.code(), Some(1));
        assert!(output.stdout.is_empty());
    }
    let root = tempfile::tempdir().unwrap();
    let output = run(
        root.path(),
        &[
            "--command",
            ":calc { return 7; } > error_rate\n// read it\n:calc { return $error_rate; }",
        ],
    );
    success(&output);
    assert_eq!(last(&output), serde_json::json!(7));
}

#[test]
fn help_is_readable_by_default_and_json_remains_explicit() {
    let root = tempfile::tempdir().unwrap();
    for (command, expected) in [
        (":help", vec![":workspace", "More help"]),
        (
            ":help calc",
            vec!["Usage", "Examples", "parseJson", "// comment"],
        ),
        (
            ":help http request",
            vec!["url:", "required", "HTTP", "Result  HttpResponse"],
        ),
    ] {
        let output = run(root.path(), &["--command", command]);
        success(&output);
        let text = String::from_utf8_lossy(&output.stdout);
        assert!(!text.contains("\"children\":"), "{text}");
        assert!(!text.contains("Help display limit reached"), "{text}");
        if command == ":help calc" {
            assert_eq!(text.matches("Operations  ").count(), 1);
            assert!(text.contains("some, isSome"));
            assert_eq!(text.matches("Examples  ").count(), 1);
            assert!(!text.contains("Example  "));
            assert!(text.contains("return [1,2,3].map(x => x*2)"));
        }
        for fragment in expected {
            assert!(text.contains(fragment), "{fragment}: {text}");
        }
        let json = run(root.path(), &["--json", "--command", command]);
        success(&json);
        assert!(last(&json)["children"].is_array());
    }
    std::fs::write(root.path().join("help.wes"), ":help calc").unwrap();
    let output = run(root.path(), &["--json", "--file", "help.wes"]);
    success(&output);
    assert_eq!(last(&output)["path"], "calc");
    // Looking like help is insufficient; the result must carry the declared help type.
    let output = run(
        root.path(),
        &["--command", ":calc { return {path:'calc',children:[]}; }"],
    );
    success(&output);
    assert_eq!(last(&output)["path"], "calc");
}

#[test]
fn inspect_missing_dollar_reports_an_actionable_hint_without_executing() {
    let root = tempfile::tempdir().unwrap();
    let output = run(root.path(), &["--command", ":inspect total"]);
    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("Did you mean :inspect $total?"));
}

#[test]
fn predicted_calculation_results_check_later_references_before_execution() {
    for (producer, reference) in [
        (":calc { return 2.4; } > metric", "$metric"),
        (":calc { return {rate:2.4}; } > summary", "$summary.rate"),
    ] {
        let root = tempfile::tempdir().unwrap();
        let script = format!("{producer}\n:calc {{ return {reference} > 2; }} > compared");
        let output = run(root.path(), &["--command", &script]);
        assert_eq!(output.status.code(), Some(1));
        let error = String::from_utf8_lossy(&output.stderr);
        assert!(
            error.contains("CAL004: comparison requires matching kinds"),
            "{error}"
        );
        assert!(!error.contains("id1001: CAL004"), "consumer ran: {error}");
        let valid = run(
            root.path(),
            &[
                "--command",
                &format!(":calc {{ return {reference} > 2.0; }}"),
            ],
        );
        success(&valid);
        assert_eq!(last(&valid), serde_json::json!(true));
    }
}

#[test]
fn file_definition_source_name_survives_reopen_and_invocation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("definitions.wes"),
        "// source\n:def ratio(n:Int) -> Int as :calc {\n return div(n,0);\n}",
    )
    .unwrap();
    success(&run(root.path(), &["--file", "definitions.wes"]));
    let output = run(root.path(), &["--command", "ratio n:1"]);
    assert_eq!(output.status.code(), Some(1));
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(
        error.contains("definitions.wes: line 3, column 9"),
        "{error}"
    );
    assert!(!error.contains("<input>"));
}
#[test]
fn retained_errors_have_portable_source_labels_and_cli_stacks_are_bounded() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("failure.wes");
    std::fs::write(
        &file,
        ":calc { return div(1,0); } *> problem\n:calc { return $problem; } > detail",
    )
    .unwrap();
    let failed = run(root.path(), &["--json", "--file", file.to_str().unwrap()]);
    assert_eq!(failed.status.code(), Some(1));
    let text = String::from_utf8_lossy(&failed.stdout);
    assert!(text.contains("failure.wes"), "{text}");
    assert!(!text.contains(root.path().to_str().unwrap()));
    let reopened = run(root.path(), &["--json", "--command", ":read $detail"]);
    success(&reopened);
    let text = String::from_utf8_lossy(&reopened.stdout);
    assert!(text.contains("failure.wes"));
    assert!(!text.contains(root.path().to_str().unwrap()));
    let recurse = run(
        root.path(),
        &[
            "--workspace",
            "recursion",
            "--command",
            ":calc { function f(){return f();} return f(); }",
        ],
    );
    assert_eq!(recurse.status.code(), Some(1));
    let error = String::from_utf8_lossy(&recurse.stderr);
    assert!(error.contains("more identical frames"), "{error}");
    assert!(error.lines().count() < 30);
}
#[test]
fn successful_producers_explain_unused_error_outputs_without_a_fake_consumer_value() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("ports.wes"),
        ":calc { return 1; } *> problem\n:calc { return $problem.message; } > explanation",
    )
    .unwrap();
    let result = run(root.path(), &["--file", "ports.wes"]);
    success(&result);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("error output was not produced"), "{stderr}");
    assert!(!String::from_utf8_lossy(&result.stdout).contains("explanation"));
}
#[test]
fn package_load_counts_iterator_recipes_as_informational_output() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("types.yaml"),"types:\n  Line: {base: Text}\niterators:\n  Lines: {input: Text, output: 'Iter<Line>', mode: lines}\n").unwrap();
    let result = run(root.path(), &["--command", ":package load path:types.yaml"]);
    success(&result);
    assert!(
        String::from_utf8_lossy(&result.stdout)
            .contains("1 type definitions and 1 iterator recipes")
    );
}
