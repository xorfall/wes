use super::*;
use wes::telemetry::{Controller, Mode};
async fn control(f: &Fixture, generation: &str, action: Value) -> reqwest::Response {
    f.client
        .post(f.url("/diagnostics"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", generation)
        .body(action.to_string())
        .send()
        .await
        .unwrap()
}
#[tokio::test]
async fn diagnostics_are_local_bounded_session_checked_and_independent_of_history() {
    let mut controller = None;
    let f = Fixture::with_services(|root| {
        let c = Controller::open(root.join("diagnostics"), None).unwrap();
        controller = Some(c.clone());
        wes::web::Services {
            telemetry: Some(c),
            ..Default::default()
        }
    })
    .await;
    let c = controller.unwrap();
    let status: Value = serde_json::from_slice(
        &f.client
            .get(f.url("/diagnostics"))
            .send()
            .await
            .unwrap()
            .bytes()
            .await
            .unwrap(),
    )
    .unwrap();
    let generation = status["generation"].as_str().unwrap();
    assert_eq!(status["mode"], "basic");
    assert_eq!(status["remote_export"], false);
    assert_eq!(
        control(&f, "stale", json!({"action":"start"}))
            .await
            .status(),
        409
    );
    assert_eq!(
        f.client
            .post(f.url("/diagnostics"))
            .header("Origin", "https://untrusted.invalid")
            .header("Content-Type", "application/json")
            .header("X-Wes-Session", generation)
            .body("{\"action\":\"start\"}")
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    assert_eq!(
        control(
            &f,
            generation,
            json!({"action":"start","secret":"SYNTHETIC_SECRET"})
        )
        .await
        .status(),
        400
    );
    assert_eq!(
        control(&f, generation, json!({"action":"set","mode":"diagnostic"}))
            .await
            .status(),
        409
    );
    assert!(
        control(&f, generation, json!({"action":"start"}))
            .await
            .status()
            .is_success()
    );
    let good = f
        .client
        .post(f.url("/diagnostics/client"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", generation)
        .body(
            json!({"events":[{"kind":"error"},{"kind":"submit","elapsed_us":1200,"outcome":"ok"}]})
                .to_string(),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(good.status(), 204);
    let bad = f
        .client
        .post(f.url("/diagnostics/client"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", generation)
        .body(json!({"events":[{"kind":"error","message":"SYNTHETIC_SECRET"}]}).to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 400);
    let large = f
        .client
        .post(f.url("/diagnostics"))
        .header("Content-Type", "application/json")
        .header("X-Wes-Session", generation)
        .body("x".repeat(4097))
        .send()
        .await
        .unwrap();
    assert_eq!(large.status(), 413);
    let exported = control(&f, generation, json!({"action":"export"})).await;
    assert!(
        exported.headers()["content-disposition"]
            .to_str()
            .unwrap()
            .contains("wes-diagnostics.json")
    );
    let body = exported.text().await.unwrap();
    assert!(!body.contains("SYNTHETIC_SECRET"));
    assert!(body.contains("ui_error"));
    assert!(
        control(&f, generation, json!({"action":"set","mode":"off"}))
            .await
            .status()
            .is_success()
    );
    assert_eq!(c.mode(), Mode::Off);
    // Off is application telemetry only; normal source admission still succeeds.
    assert_eq!(
        f.source(generation, "ordinary", ":calc 2 + 3 > result")
            .await,
        202
    );
    assert_eq!(c.status()["recent_count"], 0);
    f.close().await;
    c.shutdown();
}
#[tokio::test]
async fn changing_workspace_stops_capture_and_old_generation_cannot_enable_it() {
    let mut controller = None;
    let f = Fixture::with_services(|root| {
        let c = Controller::open(root.join("diagnostics"), None).unwrap();
        controller = Some(c.clone());
        wes::web::Services {
            telemetry: Some(c),
            ..Default::default()
        }
    })
    .await;
    let c = controller.unwrap();
    let mut events = f.stream().await;
    let generation = events.generation().await;
    assert_eq!(
        f.source(&generation, "save", ":workspace save \"checkpoint\"")
            .await,
        202
    );
    f.app.current().unwrap().session.wait_idle().await.unwrap();
    assert!(
        control(&f, &generation, json!({"action":"start"}))
            .await
            .status()
            .is_success()
    );
    assert_eq!(
        f.source(&generation, "load", ":workspace load \"checkpoint\"")
            .await,
        202
    );
    let next = events.generation().await;
    assert_ne!(generation, next);
    assert_eq!(c.mode(), Mode::Basic);
    assert_eq!(
        control(&f, &generation, json!({"action":"start"}))
            .await
            .status(),
        409
    );
    drop(events);
    f.close().await;
    c.shutdown();
}
