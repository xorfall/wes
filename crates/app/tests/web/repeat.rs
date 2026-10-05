use super::*;
use wes_core::Data;

#[tokio::test]
async fn suffix_repeat_transport_keeps_all_stage_nodes_and_deduplicates_the_selected_range() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "catalog echo value:hello | catalog echo value:input";
    assert_eq!(fixture.source(&generation, "pipe", source).await, 202);
    let planned = events.until("planned").await;
    let nodes = planned["nodes"].clone();
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let request = json!({"request":"submit","client":"web-fixture", "cell":"suffix", "text":source, "repeat":"pipe", "from":nodes[1]});
    assert_eq!(
        fixture.post(&generation, request.clone()).await.status(),
        202
    );
    session.wait_idle().await.unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.post(&generation, request).await.status(), 202);
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"submit","client":"web-fixture", "cell":"suffix", "text":source, "repeat":"pipe"})
            )
            .await
            .status(),
        409
    );
    let repeated = loop {
        let event = events.until("planned").await;
        if event["cell"] == "suffix" {
            break event;
        }
    };
    assert_eq!(repeated["repeatFrom"], nodes[1]);
    assert_eq!(repeated["nodes"], nodes);
    assert_eq!(
        fixture
            .post(
                &generation,
                json!({"request":"cancel-work", "origin":"pipe"})
            )
            .await
            .status(),
        202
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn pipeline_is_one_projected_cell_with_ordered_nodes_and_deduplicated_submission() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = ":calc { return {x: 21}; } | catalog echo value:input.x | :calc { return input * 2; } > answer";
    assert_eq!(fixture.source(&generation, "pipe", source).await, 202);
    let planned = events.until("planned").await;
    assert_eq!(planned["cell"], "pipe");
    let nodes = planned["nodes"].as_array().unwrap();
    assert_eq!(nodes.len(), 3);
    loop {
        let ready = events.until("ready").await;
        if ready["node"] == nodes[2] {
            break;
        }
    }
    let session = fixture.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let snapshot = session.snapshot().await.unwrap();
    assert_eq!(
        snapshot.execution.values[&snapshot.names["answer"].node].data(),
        &Data::Int(42)
    );
    assert_eq!(fixture.source(&generation, "pipe", source).await, 202);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    let observation = session.observe().await.unwrap();
    assert_eq!(
        observation
            .cells
            .iter()
            .filter(|c| c.input.cell() == "pipe")
            .count(),
        1
    );
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn typed_repeat_projects_origin_exact_run_and_failure_without_new_nodes() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = "catalog echo value:hello";
    assert_eq!(fixture.source(&generation, "original", source).await, 202);
    let first = events.until("ready").await;
    let node = first["node"].clone();
    fixture
        .app
        .current()
        .unwrap()
        .session
        .wait_idle()
        .await
        .unwrap();
    let repeat = json!({"request":"submit","client":"web-fixture","cell":"repeat","text":source,"repeat":"original"});
    assert_eq!(
        fixture
            .post(&generation, repeat.clone())
            .await
            .status()
            .as_u16(),
        202
    );
    let plan = loop {
        let event = events.until("planned").await;
        if event["cell"] == "repeat" {
            break event;
        }
    };
    assert_eq!(plan["repeatOf"], "original");
    assert_eq!(plan["nodes"], json!([node]));
    assert!(plan["repeatedRun"].as_str().is_some());
    let ready = events.until("ready").await;
    assert_eq!(ready["node"], node);
    assert_eq!(ready["publication"]["run"], plan["repeatedRun"]);
    assert_eq!(
        fixture.post(&generation, repeat).await.status().as_u16(),
        202
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    let refused = fixture.post(&generation, json!({"request":"submit","client":"web-fixture","cell":"wrong","text":"catalog echo value:changed","repeat":"original"})).await;
    assert_eq!(refused.status().as_u16(), 400);
    let failed = loop {
        let event = events.until("planned").await;
        if event["cell"] == "wrong" {
            break event;
        }
    };
    assert!(
        failed["failure"]
            .as_str()
            .unwrap()
            .contains("source changed")
    );
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 2);
    drop(events);
    fixture.close().await;
}

#[tokio::test]
async fn revision_and_history_transport_preserve_one_work_and_do_not_execute_on_read() {
    let f = Fixture::new().await;
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(&generation, "original", "catalog echo value:old > result")
            .await,
        202
    );
    let original = events.until("planned").await;
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    let request = json!({"request":"submit","client":"web-fixture","cell":"revision","revision_of":"original","text":"catalog echo value:new > result"});
    assert_eq!(f.post(&generation, request.clone()).await.status(), 202);
    assert_eq!(f.post(&generation, request).await.status(), 202);
    let revised = loop {
        let e = events.until("planned").await;
        if e["cell"] == "revision" {
            break e;
        }
    };
    assert_eq!(revised["workOf"], "original");
    assert_eq!(revised["revisionOf"], "original");
    assert_eq!(revised["revisionAccepted"], true);
    assert_ne!(original["nodes"], revised["nodes"]);
    let session = f.app.current().unwrap().session;
    session.wait_idle().await.unwrap();
    let pause = session.checkpoint().await.unwrap();
    pause.resume().await;
    let response = f
        .post(
            &generation,
            json!({"request":"work-history","client":"browser","cell":"revision"}),
        )
        .await;
    assert_eq!(response.status(), 200);
    let history: Value = serde_json::from_str(&response.text().await.unwrap()).unwrap();
    assert_eq!(history["attempts"].as_array().unwrap().len(), 2);
    assert_eq!(history["runs"].as_array().unwrap().len(), 2);
    assert_eq!(f.calls.load(Ordering::SeqCst), 2);
    drop(events);
    f.close().await;
}
