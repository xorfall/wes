use std::process::{Command, Output};
fn run(home: &std::path::Path, source: &str, sequential: bool) -> Output {
    let mut command = Command::new(env!("CARGO_BIN_EXE_wes"));
    command.arg("--home").arg(home).args(["--command", source]);
    if sequential {
        command.arg("--sequential");
    }
    command.output().unwrap()
}
fn success(output: &Output) {
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn sequential_deletion_keeps_plan_authority_in_one_live_session() {
    let root = tempfile::tempdir().unwrap();
    success(&run(root.path(), ":workspace save \"snapshot\"", false));
    let deleted = run(
        root.path(),
        ":workspace plan delete workspace:\"snapshot\" > proposed\n:workspace delete $proposed",
        true,
    );
    success(&deleted);
    assert!(String::from_utf8_lossy(&deleted.stdout).contains("snapshot"));
    assert_eq!(
        String::from_utf8_lossy(&deleted.stdout)
            .matches("Workspace deletion completed.")
            .count(),
        1
    );
    let listed = run(root.path(), ":list workspaces", false);
    success(&listed);
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("snapshot"));
    // Deleting the issuing workspace still reports the completed receipt after it disappears.
    let own = run(
        root.path(),
        ":workspace plan delete > own\n:workspace delete $own",
        true,
    );
    success(&own);
    assert_eq!(
        String::from_utf8_lossy(&own.stdout)
            .matches("Workspace deletion completed.")
            .count(),
        1
    );
}
#[test]
fn sequential_mode_does_not_restore_or_transfer_plan_authority() {
    let root = tempfile::tempdir().unwrap();
    success(&run(root.path(), ":workspace save \"snapshot\"", false));
    success(&run(
        root.path(),
        ":workspace plan delete workspace:\"snapshot\" > proposed",
        false,
    ));
    let denied = run(root.path(), ":workspace delete $proposed", true);
    assert!(!denied.status.success());
    assert!(String::from_utf8_lossy(&denied.stderr).contains("live WorkspaceDeletePlan"));
    let mixed = run(
        root.path(),
        ":workspace plan delete workspace:\"snapshot\" > proposed\n:workspace delete $proposed",
        false,
    );
    assert!(!mixed.status.success());
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("ENG005"));
    assert!(String::from_utf8_lossy(&mixed.stderr).contains("--sequential"));
}

#[test]
fn management_output_is_multiline_for_people_and_compact_for_json() {
    for source in [
        ":workspace plan delete > preview",
        ":sandbox { :calc { return 1; } > item } > preview",
    ] {
        let root = tempfile::tempdir().unwrap();
        let human = run(root.path(), source, false);
        success(&human);
        let text = String::from_utf8_lossy(&human.stdout);
        assert!(text.contains("{\n  \""), "{text}");
        let root = tempfile::tempdir().unwrap();
        let machine = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path())
            .args(["--command", source, "--json"])
            .output()
            .unwrap();
        success(&machine);
        let text = String::from_utf8_lossy(&machine.stdout);
        assert!(!text.contains("{\n"), "{text}");
    }
}

#[test]
fn sandbox_argument_errors_and_single_later_step_are_clear() {
    let root = tempfile::tempdir().unwrap();
    let unnamed = run(root.path(), ":sandbox { :calc { return 1; } }", false);
    assert!(!unnamed.status.success());
    assert!(String::from_utf8_lossy(&unnamed.stderr).contains("CMD001: Name the sandbox"));
    success(&run(
        root.path(),
        ":sandbox { :calc { return 1; } } > preview",
        false,
    ));
    let scope = run(root.path(), ":remove $preview", false);
    assert!(!scope.status.success());
    let text = String::from_utf8_lossy(&scope.stderr);
    assert!(text.contains("scope:downstream"));
    assert!(!text.contains("Node removal"));
    let deleted = run(
        root.path(),
        ":workspace plan delete > own\n:workspace delete $own\n:help",
        true,
    );
    assert!(!deleted.status.success());
    assert!(String::from_utf8_lossy(&deleted.stderr).contains("1 later workflow step was not run"));
}

#[test]
fn stale_deletion_plan_does_not_speculate_about_prior_deletions() {
    let root = tempfile::tempdir().unwrap();
    let stale = run(
        root.path(),
        ":workspace plan delete > proposed\n:calc { return 1; } > item\n:workspace delete $proposed",
        true,
    );
    assert!(!stale.status.success());
    let text = String::from_utf8_lossy(&stale.stderr);
    assert!(
        text.contains("nothing was deleted by this request"),
        "{text}"
    );
    assert!(!text.contains("earlier admitted deletion"), "{text}");
}
#[test]
fn sequential_preflight_and_failed_steps_prevent_later_effects() {
    let root = tempfile::tempdir().unwrap();
    let unopened = root.path().join("unopened");
    assert!(
        !run(&unopened, ":workspace save \"never\"\n:calc {", true)
            .status
            .success()
    );
    assert!(!unopened.exists());
    assert!(
        !run(
            &unopened,
            ":workspace save \"never\"\n:calc { return 1 + ; }",
            true
        )
        .status
        .success()
    );
    assert!(!unopened.exists());
    let stopped = run(
        root.path(),
        ":calc { return 1 / 0; }\n:workspace save \"never\"",
        true,
    );
    assert!(!stopped.status.success());
    assert!(String::from_utf8_lossy(&stopped.stderr).contains("CAL005"));
    let listed = run(root.path(), ":list workspaces", false);
    success(&listed);
    assert!(!String::from_utf8_lossy(&listed.stdout).contains("never"));
}
#[test]
fn sequential_file_errors_keep_original_unicode_line_positions() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("workflow.wes");
    std::fs::write(&file, ":calc { return 'é'; } > first\n// comment\n:calc { return 1/0; }\n:workspace save \"never\"").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path().join("home"))
        .arg("--file")
        .arg(file)
        .arg("--sequential")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("workflow.wes"), "{error}");
    assert!(error.contains("line 3"), "{error}");
}
#[test]
fn saved_destination_replacement_and_success_output_are_explicit() {
    let root = tempfile::tempdir().unwrap();
    let first = run(root.path(), ":workspace save \"snapshot\"", false);
    success(&first);
    assert!(String::from_utf8_lossy(&first.stdout).contains("saved workspace 'snapshot'"));
    assert!(!String::from_utf8_lossy(&first.stderr).contains("MET010"));
    let second = run(root.path(), ":workspace save \"snapshot\"", false);
    success(&second);
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("replaced saved workspace 'snapshot'")
    );
    let current = run(root.path(), ":workspace save \"default\"", false);
    success(&current);
    assert!(!String::from_utf8_lossy(&current.stdout).contains("replaced"));
    let json = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path())
        .args(["--command", ":workspace save \"default\"", "--json"])
        .output()
        .unwrap();
    success(&json);
    assert!(String::from_utf8_lossy(&json.stderr).contains("\"severity\":\"info\""));
    assert!(String::from_utf8_lossy(&json.stderr).contains("MET010"));
}
#[test]
fn malformed_arguments_name_the_flag_and_offer_short_help() {
    let root = tempfile::tempdir().unwrap();
    for (flags, expected) in [
        (vec!["--workspac"], "unknown argument: --workspac"),
        (vec!["--workspace"], "--workspace requires a value"),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_wes"))
            .arg("--home")
            .arg(root.path().join("unopened"))
            .args(flags)
            .output()
            .unwrap();
        assert_eq!(output.status.code(), Some(2));
        let text = String::from_utf8_lossy(&output.stderr);
        assert!(text.contains(expected));
        assert!(text.contains("wes --help"));
        assert!(text.lines().count() <= 3);
        assert!(!root.path().join("unopened").exists());
    }
}
#[test]
fn sandbox_cli_observations_listing_and_joined_removal_are_visible() {
    let root = tempfile::tempdir().unwrap();
    let source = ":sandbox { :calc { return 42; } > value } > preview\n:read $preview\n:inspect $preview\n:list sandboxes\n:cancel $preview\n:inspect $preview\n:remove $preview scope:downstream\n:list sandboxes";
    let output = run(root.path(), source, true);
    success(&output);
    let text = String::from_utf8_lossy(&output.stdout);
    for expected in [
        "active",
        "\"value\": 42",
        "Int",
        "[\"preview\"]",
        "stopped",
        "removed",
        ": []",
    ] {
        assert!(text.contains(expected), "{text}");
    }
    let reopened = run(root.path(), ":list sandboxes", false);
    success(&reopened);
    assert!(String::from_utf8_lossy(&reopened.stdout).contains(": []"));
    assert!(!String::from_utf8_lossy(&reopened.stdout).contains("preview"));
}
#[test]
fn unavailable_map_results_keep_provable_declared_types_after_reopen() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path())
        .args([
            "--no-auto-keep",
            "--command",
            ":calc { return [1,2].map(x=>text(x)); } > labels",
        ])
        .output()
        .unwrap();
    success(&output);
    let listed = run(root.path(), ":list names", false);
    success(&listed);
    let text = String::from_utf8_lossy(&listed.stdout);
    assert!(text.contains("labels"));
    assert!(text.contains("TEXT"), "{text}");
    assert!(text.contains("STALE"));
}

#[test]
fn restored_sandbox_definitions_are_listed_without_restarting_members() {
    let root = tempfile::tempdir().unwrap();
    success(&run(
        root.path(),
        ":sandbox { :calc { return 7; } > value } > preview",
        false,
    ));
    let reopened = run(root.path(), ":list sandboxes", false);
    success(&reopened);
    assert!(String::from_utf8_lossy(&reopened.stdout).contains("[\"preview\"]"));
    let inspected = run(root.path(), ":inspect $preview", false);
    success(&inspected);
    assert!(String::from_utf8_lossy(&inspected.stdout).contains("not run"));
}
#[test]
fn active_workspace_deletion_reports_later_steps_not_run() {
    let root = tempfile::tempdir().unwrap();
    let output = run(
        root.path(),
        ":workspace plan delete > own\n:workspace delete $own\n:workspace save \"later\"",
        true,
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stdout).contains("deletion completed"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("1 later workflow step was not run"));
}
#[test]
fn sequential_history_stores_only_each_statement_and_unicode_columns_are_original() {
    let root = tempfile::tempdir().unwrap();
    let prefix = ":calc { return 'é🦀'; } > first\n    ";
    let source = format!("{prefix}:calc {{ let label='é🦀'; return 1/0; }}");
    let file = root.path().join("workflow.wes");
    std::fs::write(&file, &source).unwrap();
    let home = root.path().join("home");
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(&home)
        .arg("--file")
        .arg(&file)
        .arg("--sequential")
        .output()
        .unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    let column = wes_language::SourceText::new("workflow.wes", &source)
        .position(source.find("1/0").unwrap())
        .unwrap()
        .column;
    assert!(
        error.contains(&format!("line 2, column {column}")),
        "{error}"
    );
    fn scan(path: &std::path::Path, sources: &mut Vec<String>) {
        for entry in std::fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                scan(&path, sources);
            } else if let Ok(text) = std::fs::read_to_string(path) {
                for line in text.lines() {
                    if let Ok(value) = serde_json::from_str::<serde_json::Value>(line) {
                        if matches!(
                            value["entry"]["record"].as_str(),
                            Some("command" | "submitted")
                        ) {
                            if let Some(text) = value["entry"]["text"].as_str() {
                                sources.push(text.into());
                            }
                        }
                    }
                }
            }
        }
    }
    let mut sources = vec![];
    scan(&home, &mut sources);
    assert!(!sources.is_empty());
    assert!(
        sources.iter().all(|text| text.starts_with(":calc")),
        "{sources:?}"
    );
    assert!(sources.iter().all(|text| text.len() < source.len()));
}
#[test]
fn json_submission_errors_are_structured_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_wes"))
        .arg("--home")
        .arg(root.path())
        .args(["--command", ":inspect missing", "--json"])
        .output()
        .unwrap();
    assert!(!output.status.success());
    let text = String::from_utf8_lossy(&output.stderr);
    let diagnostics: Vec<serde_json::Value> = text
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect();
    assert!(diagnostics.iter().any(|d| d["severity"] == "error"));
}

#[cfg(unix)]
#[test]
fn private_sandbox_member_is_withheld_without_failing_the_workflow() {
    use std::{io::Write, process::Stdio};
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("env.yaml"), "version: 1\ntargets: {local: {kind: local}}\nenvironments: {dev: {secretSlots: {key: {required: true}}, secretRefs: {key: qa/sandbox}, imports: {vars: {source: {kind: process, bin: /usr/bin/printenv}, bind: {target: local, output: private, credentials: {QA_SECRET: {secret: key}}}}}}}").unwrap();
    let mut child=Command::new(env!("CARGO_BIN_EXE_wes")).current_dir(root.path()).args([
        "--home","home","--env-file","env.yaml","--env","dev","--credentials-stdin","--grant-provider","vars","--sequential","--command",
        ":sandbox { vars run args:QA_SECRET > secret } > preview\n:read $preview.secret\n:calc { return 42; }"
    ]).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(br#"{"qa/sandbox":"synthetic-sandbox-private-sentinel"}"#)
        .unwrap();
    let output = child.wait_with_output().unwrap();
    success(&output);
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stdout.contains(": 42"), "{stdout}");
    assert!(stderr.contains("Private value withheld"), "{stderr}");
    assert!(!stdout.contains("synthetic-sandbox-private-sentinel"));
    assert!(!stderr.contains("synthetic-sandbox-private-sentinel"));
}
