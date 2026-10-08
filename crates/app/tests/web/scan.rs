use super::*;
#[tokio::test]
async fn finite_analysis_delivers_real_progress_and_labelled_incomplete_publication() {
    for fail in [false, true] {
        let fixture = Fixture::new().await;
        let mut events = fixture.stream().await;
        let generation = events.generation().await;
        assert_eq!(fixture.source(&generation,"types",r#":package load source:"types: {IntStep: {base: Record, fields: {state: Int, outputs: 'List<Int>'}}}""#).await,202);
        let body = if fail {
            "if(item==2) { return {state:1/0,outputs:[]}; } return {state:state+item,outputs:[item]};"
        } else {
            "return {state:state+item,outputs:[item]};"
        };
        let source = format!(
            ":def sum(state:Int, context:Int, item:Int) -> IntStep as :calc pure {{ {body} }}\n:calc {{ return [1,2,3]; }} > raw\n:scan source:$raw transition:sum initial:0 context:0 profile:TypedRecords > analysis"
        );
        assert_eq!(fixture.source(&generation, "analysis", &source).await, 202);
        let session = fixture.app.current().unwrap().session;
        session.wait_idle().await.unwrap();
        let snapshot = session.snapshot().await.unwrap();
        let node = &snapshot.names["analysis"].node;
        let kind = if fail { "evidence" } else { "ready" };
        let frame = loop {
            let frame = events.until(kind).await;
            if frame["node"] == node.as_str() {
                break frame;
            }
        };
        assert_eq!(frame["private"], false);
        if fail {
            assert_eq!(frame["kind"], "incomplete");
            assert_eq!(frame["state"], "failed");
            assert_eq!(frame["kept"], false);
            assert!(frame["error"]["code"].is_string());
        }
        let body = fixture
            .client
            .get(fixture.url(&format!("/values/{}", frame["handle"].as_str().unwrap())))
            .send()
            .await
            .unwrap();
        assert_eq!(body.status(), 200);
        let value: Value = serde_json::from_slice(&body.bytes().await.unwrap()).unwrap();
        assert_eq!(value["data"]["state"], json!(if fail { 1 } else { 6 }));
        assert_eq!(
            value["data"]["receipt"]["inputRecords"],
            json!(if fail { 1 } else { 3 })
        );
        assert_eq!(
            value["data"]["receipt"]["sourceComplete"],
            json!({"kind":"none"})
        );
        assert_eq!(value["data"]["receipt"]["durableResume"], false);
        let progress = &snapshot.execution.progress[node];
        assert_eq!(
            progress.phase,
            if fail {
                wes_engine::driver::progress::Phase::Stopped
            } else {
                wes_engine::driver::progress::Phase::Complete
            }
        );
        assert_eq!(
            progress.counters.as_ref().unwrap().input_records,
            if fail { 1 } else { 3 }
        );
        println!(
            "ANALYSIS_WIRE_SAMPLE={}",
            json!({"frame":frame,"value":value,"progress":progress})
        );
        drop(events);
        fixture.close().await;
    }
}
