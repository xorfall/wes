use super::*;

#[tokio::test]
async fn structured_failure_records_reach_live_and_reconnected_ui() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(&generation, "failure-record", ":calc { return div(1,0); }")
            .await,
        202
    );
    let failed = events.until("failed").await;
    assert_eq!(failed["error"]["code"], "CAL005");
    assert_eq!(failed["reason"], failed["error"]["message"]);
    assert!(failed["error"]["issues"].is_array());
    let mut reconnected = fixture.stream().await;
    assert_eq!(reconnected.generation().await, generation);
    assert_eq!(reconnected.until("failed").await["error"], failed["error"]);
    fixture.close().await;
}
#[tokio::test]
async fn calculation_browser_result_duplicate_submit_and_reconnect_preserve_one_execution() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = ":calc {\n let sum=0; for(const x of [1,2,3]) { sum=sum+call('catalog',['echo'],{value:x}); }\n return sum; } > total";
    assert_eq!(fixture.source(&generation, "calc", source).await, 202);
    let ready = events.until("ready").await;
    assert_eq!(ready["constructionComplete"], false);
    let bytes = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["data"], 6);
    assert_eq!(fixture.source(&generation, "calc", source).await, 202);
    let mut reconnect = fixture.stream().await;
    assert_eq!(reconnect.generation().await, generation);
    assert_eq!(reconnect.until("ready").await["handle"], ready["handle"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    fixture.close().await;
}

#[tokio::test]
async fn nullable_json_contract_pipeline_call_and_saved_workspace_reopen_without_effect_repetition()
{
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = ":calc { const rows=check('List<Option<Int>>',parseJson('[1,null,3]','List<Option<Int>>')); const sum=rows.filter(x=>isSome(x)).map(x=>unwrapOr(x,0)).reduce((a,x)=>a+x,0); return call('catalog',['echo'],{value:sum}); } > total";
    assert_eq!(fixture.source(&generation, "pipeline", source).await, 202);
    let ready = events.until("ready").await;
    let bytes = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    assert_eq!(serde_json::from_slice::<Value>(&bytes).unwrap()["data"], 4);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"calc-copy\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"calc-copy\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_ne!(next, generation);
    assert_eq!(events.until("ready").await["handle"], ready["handle"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.source(&next,"invalid",":calc { const x=parseJson('[null]','List<Int>'); return call('catalog',['echo'],{value:x}); }").await,202);
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 1);
    assert!(
        current
            .session
            .snapshot()
            .await
            .unwrap()
            .execution
            .errors
            .values()
            .any(|e| e.code() == "CAL017")
    );
    fixture.close().await;
}
#[tokio::test]
async fn iter_browser_consumption_duplicate_and_reconnect_do_not_repeat_effects() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    let source = ":calc {\n let sum=0; for(const x of iter.items([1,2,3,4]).take(3)) { sum=sum+call('catalog',['echo'],{value:x}); }\n return sum; } > total";
    assert_eq!(fixture.source(&generation, "calc", source).await, 202);
    let ready = events.until("ready").await;
    let bytes = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let data: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(data["data"], 6);
    assert_eq!(fixture.source(&generation, "calc", source).await, 202);
    let mut reconnect = fixture.stream().await;
    assert_eq!(reconnect.generation().await, generation);
    assert_eq!(reconnect.until("ready").await["handle"], ready["handle"]);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 3);
    fixture.close().await;
}

#[tokio::test]
async fn kept_iter_reopens_as_metadata_then_explicitly_consumes_snapshot() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        fixture
            .source(
                &generation,
                "recipe",
                ":calc { return iter.lines('private snapshot\\nsecond').take(1); } > rows"
            )
            .await,
        202
    );
    let ready = events.until("ready").await;
    let handle = ready["handle"].as_str().unwrap();
    let value = fixture
        .client
        .get(fixture.url(&format!("/values/{handle}")))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&value).unwrap();
    assert_eq!(value["type"]["kind"], "iter");
    assert!(!value.to_string().contains("private snapshot"));
    let storage_handle = wes_engine::storage::ValueHandle::new(handle).unwrap();
    assert!(!fixture.worker.is_kept(storage_handle).await.unwrap());
    assert!(
        fixture
            .post(&generation, json!({"request":"keep","handle":handle}))
            .await
            .status()
            .is_success()
    );
    assert_eq!(
        fixture
            .source(&generation, "save", ":workspace save \"iter-copy\"")
            .await,
        202
    );
    assert_eq!(
        fixture
            .source(&generation, "load", ":workspace load \"iter-copy\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_ne!(generation, next);
    assert_eq!(events.until("ready").await["handle"], handle);
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture
            .source(
                &next,
                "consume",
                ":calc { return $rows.collect(); } > preview"
            )
            .await,
        202
    );
    let collected = events.until("ready").await;
    let result = fixture
        .client
        .get(fixture.url(&format!(
            "/values/{}",
            collected["handle"].as_str().unwrap()
        )))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let result: Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(result["data"], json!(["private snapshot"]));
    fixture.close().await;
}

#[tokio::test]
async fn typed_calculation_vocabulary_exposes_signature_without_body_and_executes_once() {
    let fixture = Fixture::new().await;
    let mut events = fixture.stream().await;
    let generation = events.generation().await;
    assert_eq!(fixture.source(&generation, "define", ":def label(input: Int) -> Text as :calc pure { return 'private-body-canary-' + text(input); }").await, 202);
    let current = fixture.app.current().unwrap();
    current.session.wait_idle().await.unwrap();
    let mut reconnect = fixture.stream().await;
    reconnect.generation().await;
    let vocabulary = loop {
        let value = reconnect.until("vocabulary").await;
        if value["templates"]
            .as_array()
            .is_some_and(|entries| entries.iter().any(|entry| entry["name"] == "label"))
        {
            break value;
        }
    };
    let template = vocabulary["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["name"] == "label")
        .unwrap();
    assert_eq!(template["body"], ":calc");
    assert_eq!(template["parameters"][0]["type"], "Int");
    assert!(!vocabulary.to_string().contains("private-body-canary"));
    assert_eq!(
        fixture
            .source(
                &generation,
                "call",
                ":calc { return 21; } | label > labelled"
            )
            .await,
        202
    );
    events.until("ready").await; // Pipeline source.
    let ready = events.until("ready").await;
    let bytes = fixture
        .client
        .get(fixture.url(&format!("/values/{}", ready["handle"].as_str().unwrap())))
        .send()
        .await
        .unwrap()
        .bytes()
        .await
        .unwrap();
    let value: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["data"], "private-body-canary-21");
    assert_eq!(fixture.calls.load(Ordering::SeqCst), 0);
    fixture.close().await;
}

#[tokio::test]
async fn run_history_keeps_the_selected_failure_after_a_successful_revision() {
    let f = Fixture::new().await;
    let mut events = f.stream().await;
    let generation = events.generation().await;
    let source = ":calc { return div(1,0); }";
    assert_eq!(f.source(&generation, "old-failure", source).await, 202);
    let failed = events.until("failed").await;
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    let request = json!({"request":"work-history","cell":"old-failure","client":"alice"});
    let history: Value = serde_json::from_str(
        &f.post(&generation, request.clone())
            .await
            .text()
            .await
            .unwrap(),
    )
    .unwrap();
    let run = history["runs"][0]["run"].clone();
    assert_eq!(history["runs"][0]["state"], "Failed");
    assert_eq!(f.post(&generation, json!({"request":"submit","client":"web-fixture","cell":"new-success","text":":calc { return 42; }","revision_of":"old-failure"})).await.status(), 202);
    events.until("ready").await;
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    let detail: Value = serde_json::from_str(
        &f.post(
            &generation,
            json!({"request":"work-history","cell":"old-failure","client":"alice","run":run}),
        )
        .await
        .text()
        .await
        .unwrap(),
    )
    .unwrap();
    assert_eq!(detail["error"]["code"], failed["error"]["code"]);
    assert_eq!(detail["error"]["message"], failed["error"]["message"]);
    assert_eq!(detail["definition"]["text"], source);
    assert_eq!(detail["run"]["state"], "Failed");
    assert!(detail["run"]["handle"].is_null());
    assert!(detail["trace"].is_null());
    assert_eq!(detail["canProtect"], false);
    assert_eq!(f.calls.load(Ordering::SeqCst), 0);
    f.close().await;
}
