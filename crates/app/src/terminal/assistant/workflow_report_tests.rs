#[test]
fn reserved_steps_distinguish_live_waits_stopped_work_and_missing_evidence() {
    let progress = SequentialProgress {
        state: SequentialState::Running,
        step: 1,
        phase: SequentialPhase::Executing,
    };
    let waiting = unstarted_step(Some(progress), 2, "step-two").unwrap();
    assert_eq!(waiting["status"], "waiting_for_predecessor");
    assert_eq!(waiting["waitingForStep"], 1);
    assert_eq!(waiting["admitted"], false);
    assert_eq!(waiting["settled"], false);
    // Already submitted steps cannot be relabelled merely because a cell is absent.
    assert!(unstarted_step(Some(progress), 0, "retired").is_none());
    assert!(unstarted_step(Some(progress), 1, "missing-current").is_none());
    assert!(unstarted_step(None, 2, "restored-reservation").is_none());

    for phase in [
        SequentialPhase::AwaitingAdmission,
        SequentialPhase::Admitting,
    ] {
        let admission =
            unstarted_step(Some(SequentialProgress { phase, ..progress }), 1, "current").unwrap();
        assert_eq!(admission["status"], "admission_pending");
        assert_eq!(
            admission["admissionStarted"],
            phase == SequentialPhase::Admitting
        );
        if phase == SequentialPhase::Admitting {
            assert!(admission.get("admitted").is_none());
        } else {
            assert_eq!(admission["admitted"], false);
        }
    }
    for state in [
        SequentialState::Failed,
        SequentialState::Cancelled,
        SequentialState::Interrupted,
    ] {
        let stopped = SequentialProgress { state, ..progress };
        let later = unstarted_step(Some(stopped), 2, "unstarted").unwrap();
        assert_eq!(later["status"], "not_admitted");
        assert_eq!(later["workflowStatus"], state.as_str());
        assert_eq!(later["settled"], true);
        assert!(unstarted_step(Some(stopped), 1, "uncertain-started").is_none());
    }
    assert!(
        unstarted_step(
            Some(SequentialProgress {
                state: SequentialState::Completed,
                step: 3,
                ..progress
            }),
            2,
            "retired-completed"
        )
        .is_none()
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn mcp_sequential_wait_reports_do_not_replay_a_gated_provider() {
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        sync::oneshot,
    };
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let (entered, started) = oneshot::channel();
    let (release, released) = oneshot::channel();
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        while !request.windows(4).any(|bytes| bytes == b"\r\n\r\n") {
            let mut bytes = [0; 1024];
            let length = stream.read(&mut bytes).await.unwrap();
            assert_ne!(length, 0);
            request.extend_from_slice(&bytes[..length]);
            assert!(request.len() <= 8192);
        }
        entered.send(()).unwrap();
        released.await.unwrap();
        stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: 12\r\nConnection: close\r\n\r\n{\"reading\":42}").await.unwrap();
        // A replay would leave another connection in this owned listener's queue.
        assert!(
            tokio::time::timeout(Duration::from_millis(50), listener.accept())
                .await
                .is_err()
        );
    });
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(
        &manager,
        &runtime,
        &home,
        root.path(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
    let arguments = json!({"request_id":"gated-workflow","context":context["context"],"source":format!("http request url:\"http://{address}/\" > response\n:calc pure {{ return $response.status; }} > status\n:calc pure {{ return $status + 1; }} > successor"),"sequential":true,"wait_ms":0});
    invoke(&terminal, &runtime.handle, "execute", arguments.clone()).await;
    tokio::time::timeout(Duration::from_secs(5), started)
        .await
        .unwrap()
        .unwrap();
    let report = invoke(
        &terminal,
        &runtime.handle,
        "execution_read",
        json!({"request_id":"gated-workflow"}),
    )
    .await;
    assert_eq!(report["status"], "running", "{report}");
    assert_eq!(
        report["steps"][0]["nodes"][0]["state"], "running",
        "{report}"
    );
    for step in &report["steps"].as_array().unwrap()[1..] {
        assert_eq!(step["status"], "waiting_for_predecessor", "{report}");
        assert_eq!(step["waitingForStep"], 0);
        assert_eq!(step["admitted"], false);
        assert!(step.get("retry").is_none());
    }
    let duplicate = invoke(&terminal, &runtime.handle, "execute", arguments).await;
    assert_eq!(duplicate["duplicate"], true);
    assert_eq!(
        duplicate["execution"]["steps"][1]["status"],
        "waiting_for_predecessor"
    );
    release.send(()).unwrap();
    let completed = invoke(
        &terminal,
        &runtime.handle,
        "execution_read",
        json!({"request_id":"gated-workflow","wait_ms":1000}),
    )
    .await;
    assert_eq!(completed["status"], "completed", "{completed}");
    assert!(
        completed["steps"]
            .as_array()
            .unwrap()
            .iter()
            .all(|step| step["nodes"][0]["state"] == "ready")
    );
    assert_eq!(
        invoke(
            &terminal,
            &runtime.handle,
            "value_read",
            json!({"name":"successor"})
        )
        .await,
        201
    );

    let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
    let failed = invoke(&terminal, &runtime.handle, "execute", json!({"request_id":"failed-workflow","context":context["context"],"source":":calc pure { return 1 / 0; }\n:calc pure { return 42; } > never","sequential":true,"wait_ms":1000})).await;
    assert_eq!(failed["execution"]["status"], "failed", "{failed}");
    assert_eq!(failed["execution"]["steps"][1]["status"], "not_admitted");
    assert_eq!(failed["execution"]["steps"][1]["workflowStatus"], "failed");
    assert!(
        invoke(&terminal, &runtime.handle, "values_list", json!({}))
            .await
            .as_array()
            .unwrap()
            .iter()
            .all(|name| name != "never")
    );
    server.await.unwrap();
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}

#[cfg(unix)]
#[tokio::test(flavor = "multi_thread")]
async fn mcp_definition_notices_confirm_commit_without_exporting_raw_details() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    std::fs::write(
        root.path().join("contracts.yaml"),
        "version: 2\ntypes:\n  PrivateContractMarker:\n    base: Int\n    min: 0\n",
    )
    .unwrap();
    let runtime = launch(RuntimeOptions::new(home.clone(), root.path().into()))
        .await
        .unwrap();
    let manager = Manager::default();
    let terminal = test_terminal(
        &manager,
        &runtime,
        &home,
        root.path(),
        &uuid::Uuid::new_v4().to_string(),
    )
    .await;
    let _stop_on_failure = terminal.stopped.clone().drop_guard();
    let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
    let source = ":package load path:\"contracts.yaml\"\n:def PrivateTemplateMarker(value:Int) -> Int as :calc pure { return value + 1; }";
    let result = invoke(&terminal, &runtime.handle, "execute", json!({"request_id":"definitions","context":context["context"],"source":source,"sequential":true,"wait_ms":1000})).await;
    assert_eq!(result["execution"]["status"], "completed", "{result}");
    let steps = result["execution"]["steps"].as_array().unwrap();
    for (step, code, verb) in [
        (0, "TYP000", "validated and installed"),
        (1, "TMP000", "registered"),
    ] {
        let notice = steps[step]["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["code"] == code)
            .unwrap();
        assert_eq!(notice["severity"], "info");
        assert!(
            notice["message"].as_str().unwrap().contains(verb),
            "{notice}"
        );
        assert!(
            !notice["message"].as_str().unwrap().contains("Private"),
            "{notice}"
        );
        assert!(notice["subject"].is_string());
        // A shared reader without source access gets the useful message, not source.
        let observed = runtime
            .handle
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap();
        let cell = observed
            .cells
            .iter()
            .find(|cell| cell.input.cell() == steps[step]["cell"].as_str().unwrap())
            .unwrap();
        let hidden = super::super::inspection::cell(&observed, cell, false, None);
        let hidden_notice = hidden["diagnostics"]
            .as_array()
            .unwrap()
            .iter()
            .find(|item| item["code"] == code)
            .unwrap();
        assert_eq!(hidden_notice["message"], notice["message"]);
        assert!(hidden_notice.get("subject").is_none());
        assert!(!hidden.to_string().contains("Private"));
    }
    let context = invoke(&terminal, &runtime.handle, "workspace_context", json!({})).await;
    let rejected = invoke(&terminal, &runtime.handle, "execute", json!({"request_id":"rejected-definition","context":context["context"],"source":":def RefusedTemplate(value: Int) -> MissingContract as :calc pure { return value; }","wait_ms":1000})).await;
    let diagnostics = rejected["execution"]["diagnostics"]
        .as_array()
        .unwrap_or_else(|| panic!("missing rejection diagnostics: {rejected}"));
    assert!(diagnostics.iter().all(|notice| notice["code"] != "TMP000"));
    assert!(
        diagnostics
            .iter()
            .any(|notice| notice["severity"] == "error")
    );
    manager.shutdown().await;
    runtime.shutdown().await.unwrap();
}
