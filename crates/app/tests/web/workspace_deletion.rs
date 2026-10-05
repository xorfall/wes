use super::*;
#[tokio::test]
async fn deleting_last_workspace_keeps_server_available_and_rejects_old_identity() {
    let f = Fixture::new().await;
    let old = f.app.current().unwrap();
    let response = f
        .client
        .post(f.url("/workspace-deletion"))
        .header("X-Wes-Session", &old.generation)
        .header("Content-Type", "application/json")
        .body(json!({"action":"preview","client":"qa"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let preview: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert!(f.app.current().is_ok());
    let response = f.client.post(f.url("/workspace-deletion"))
        .header("X-Wes-Session", &old.generation).header("Content-Type", "application/json").body(json!({"action":"confirm","client":"qa","token":preview["token"],"stop":false,"protected":false}).to_string()).send().await.unwrap();
    assert_eq!(response.status(), 200, "{}", response.text().await.unwrap());
    assert!(f.app.current().is_err());
    let response = f
        .client
        .post(f.url("/workspaces"))
        .header("Content-Type", "application/json")
        .body(json!({"name":old.name.as_str(),"create":true}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 200);
    let fresh: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_ne!(fresh["generation"], old.generation);
    assert_ne!(fresh["identity"], json!(old.identity));
    let response = f
        .client
        .post(f.url("/workspaces"))
        .header("Content-Type", "application/json")
        .body(json!({"name":old.name.as_str(),"create":false,"identity":old.identity}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    let response = f
        .client
        .post(f.url("/workspace-deletion"))
        .header("X-Wes-Session", &old.generation)
        .header("Content-Type", "application/json")
        .body(json!({"action":"preview","client":"qa"}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 409);
    f.close().await;
}

#[tokio::test]
async fn source_management_uses_normal_cells_and_confirms_deletion() {
    let f = Fixture::new().await;
    let old = f.app.current().unwrap();
    for (cell, text) in [
        ("plan", ":workspace plan delete > plan"),
        ("inspect", ":inspect $plan"),
        ("delete", ":workspace delete $plan"),
    ] {
        let response = f
            .client
            .post(f.url("/submit"))
            .header("X-Wes-Session", &old.generation)
            .header("Content-Type", "application/json")
            .body(json!({"request":"submit","cell":cell,"client":"qa","text":text}).to_string())
            .send()
            .await
            .unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();
        assert_eq!(status, 202, "{cell}: {body}");
        assert!(!body.contains("metaValue"));
        if cell != "delete" {
            old.session.wait_idle().await.unwrap();
            let observed = old.session.observe().await.unwrap();
            let output = observed
                .cells
                .iter()
                .find(|c| c.input.cell() == cell)
                .expect("source result cell");
            let result = output.reply.as_ref().unwrap().as_ref().unwrap();
            assert_eq!(result.nodes.len(), 1);
            assert!(
                observed
                    .state
                    .execution
                    .values
                    .contains_key(&result.nodes[0])
            );
        }
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while f.app.current().is_ok() {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    f.close().await;
}
