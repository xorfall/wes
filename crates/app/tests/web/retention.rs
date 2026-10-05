use super::*;

#[tokio::test]
async fn work_delete_transport_uses_confirmation_and_replaces_the_authoritative_projection() {
    let f = Fixture::new().await;
    let mut stream = f.stream().await;
    let generation = f.app.current().unwrap().generation;
    assert_eq!(
        f.source(
            &generation,
            "work",
            "catalog echo value:synthetic > disposable"
        )
        .await,
        202
    );
    stream.until("ready").await;
    assert_eq!(
        f.post(
            &generation,
            json!({"request":"delete-work","cell":"work","client":"alice"})
        )
        .await
        .status(),
        400
    );
    let response = f
        .post(
            &generation,
            json!({"request":"delete-work-preview","cell":"work","client":"alice"}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let preview: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(preview["cells"], json!(["work"]));
    let refused = f.post(&generation, json!({"request":"delete-work","token":preview["token"],"client":"bob","dependents":false,"protected":false})).await;
    assert_eq!(refused.status(), 409);
    let error: Value = serde_json::from_str(&refused.text().await.unwrap()).unwrap();
    assert_eq!(error["code"], "STO003");
    assert_eq!(error["mayHaveApplied"], false);
    let deleted = f.post(&generation, json!({"request":"delete-work","token":preview["token"],"client":"alice","dependents":false,"protected":false})).await;
    assert_eq!(deleted.status(), 200);
    loop {
        let event = stream.next().await;
        assert_ne!(
            event["event"], "session",
            "deletion must not replace the live session"
        );
        if event["event"] == "work-retired" {
            assert_eq!(event["cells"], json!(["work"]));
            break;
        }
    }
    assert_eq!(f.app.current().unwrap().generation, generation);
    assert!(
        f.app
            .current()
            .unwrap()
            .session
            .observe()
            .await
            .unwrap()
            .cells
            .is_empty()
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn storage_management_requires_preview_and_preserves_structured_owner_errors() {
    let f = Fixture::new().await;
    let mut stream = f.stream().await;
    let generation = f.app.current().unwrap().generation;
    assert_eq!(
        f.source(
            &generation,
            "retention",
            "catalog echo value:fixture > result"
        )
        .await,
        202
    );
    let ready = loop {
        let event = stream.next().await;
        if event["event"] == "ready" {
            break event;
        }
    };
    assert_eq!(ready["retention"], "automatic");
    let handle = ready["handle"].as_str().unwrap();
    assert_eq!(
        f.post(&generation, json!({"request":"release","handle":handle}))
            .await
            .status(),
        400
    );
    let response = f
        .post(
            &generation,
            json!({"request":"release-preview","handle":handle,"client":"alice"}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let preview: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    let refused = f
        .post(
            &generation,
            json!({"request":"release","token":preview["token"],"client":"ben"}),
        )
        .await;
    assert_eq!(refused.status(), 409);
    let error: Value = serde_json::from_str(&refused.text().await.unwrap()).unwrap();
    assert_eq!(error["code"], "STO003");
    assert_eq!(error["mayHaveApplied"], false);
    assert_eq!(error["retryable"], false);
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("nothing was deleted")
    );
    assert_eq!(
        f.post(
            &generation,
            json!({"request":"release","token":preview["token"],"client":"alice"})
        )
        .await
        .status(),
        200
    );
    assert!(
        f.worker
            .read(wes_engine::storage::ValueHandle::new(handle).unwrap())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}

#[tokio::test]
async fn delete_preview_reports_blocking_downstream_identity_and_lifecycle_without_mutation() {
    let f = Fixture::new().await;
    let generation = f.app.current().unwrap().generation;
    for (cell, source) in [
        ("root", "catalog echo value:original > root"),
        ("child", "catalog echo value:$root > child"),
        ("change", ":change $child value:changed"),
    ] {
        f.app
            .submit(wes_engine::source::SourceInput::new(cell.into(), source.into()).unwrap())
            .await
            .unwrap();
        f.app.current().unwrap().session.wait_idle().await.unwrap();
    }
    let before = f.app.current().unwrap().session.observe().await.unwrap();
    let response = f
        .post(
            &generation,
            json!({"request":"delete-work-preview","cell":"root","client":"alice"}),
        )
        .await;
    assert_eq!(response.status(), 503);
    let error: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(error["code"], "STO007");
    assert_eq!(error["mayHaveApplied"], false);
    assert_eq!(error["retryable"], false);
    assert_eq!(
        error["blockers"][0]["node"],
        before.state.names["child"].node.to_string()
    );
    assert_eq!(error["blockers"][0]["cells"], json!(["child"]));
    assert_eq!(error["blockers"][0]["state"], "Stale");
    assert!(
        error["blockers"][0]["reason"]
            .as_str()
            .unwrap()
            .contains("not completed")
    );
    let after = f.app.current().unwrap().session.observe().await.unwrap();
    assert_eq!(after.cells.len(), before.cells.len());
    assert_eq!(
        after.state.execution.graph.len(),
        before.state.execution.graph.len()
    );
    f.close().await;
}

#[tokio::test]
async fn retirement_fence_refuses_result_trace_and_history_inspection_but_preserves_internal_observation()
 {
    let f = Fixture::new().await;
    let current = f.app.current().unwrap();
    f.app
        .submit(
            wes_engine::source::SourceInput::new(
                "work".into(),
                "catalog echo value:fixture > result".into(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    current.session.wait_idle().await.unwrap();
    let observed = current.session.observe().await.unwrap();
    let node = observed.state.names["result"].node.clone();
    let handle = observed.values.as_ref().unwrap().outputs[&node]
        .handle()
        .unwrap()
        .clone();
    let checkpoint = current.session.checkpoint().await.unwrap();
    current
        .session
        .fence_retirement_at_checkpoint(&checkpoint)
        .await
        .unwrap();
    let result = f
        .client
        .get(f.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 503);
    let body: Value = serde_json::from_str(&result.text().await.unwrap()).unwrap();
    assert_eq!(body["error"]["code"], "VALUE_RETIREMENT_BUSY");
    assert_eq!(body["error"]["retryable"], false);
    let trace = f
        .client
        .get(f.url(&format!("/traces/{node}")))
        .header("X-Wes-Session", &current.generation)
        .send()
        .await
        .unwrap();
    assert_eq!(trace.status(), 503);
    let body: Value = serde_json::from_str(&trace.text().await.unwrap()).unwrap();
    assert_eq!(body["code"], "STO015");
    let history = f
        .post(
            &current.generation,
            json!({"request":"work-history","client":"alice","cell":"work"}),
        )
        .await;
    assert_eq!(history.status(), 503);
    let body: Value = serde_json::from_str(&history.text().await.unwrap()).unwrap();
    assert_eq!(body["code"], "STO015");
    let history_page = f
        .client
        .get(f.url("/history"))
        .header("X-Wes-Session", &current.generation)
        .send()
        .await
        .unwrap();
    assert_eq!(history_page.status(), 503);
    let body: Value = serde_json::from_str(&history_page.text().await.unwrap()).unwrap();
    assert_eq!(body["code"], "STO015");
    // Status is still available for atomic owner validation and cleanup.
    assert_eq!(current.session.observe().await.unwrap().cells.len(), 1);
    checkpoint.resume().await;
    let result = f
        .client
        .get(f.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap();
    assert_eq!(result.status(), 200);
    assert_eq!(f.calls.load(Ordering::SeqCst), 1);
    f.close().await;
}
