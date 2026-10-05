//! Isolated release comparisons. No live providers, credentials, browser, or user state.
use std::{hint::black_box, time::Instant};
use wes::telemetry::{Controller, Mode};
use wes_engine::{diagnostics::Operation, source::SourceInput};
#[tokio::main]
async fn main() {
    let args: Vec<String> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("off");
    let scenario = args.get(2).map(String::as_str).unwrap_or("operations");
    let n: usize =
        args.get(3)
            .and_then(|s| s.parse().ok())
            .unwrap_or(if scenario == "operations" {
                100_000
            } else {
                100
            });
    assert!(["off", "basic", "diagnostic"].contains(&mode));
    let root = tempfile::tempdir().unwrap();
    let c = Controller::open(root.path().join("diagnostics"), None).unwrap();
    if mode == "off" {
        c.set_mode(Mode::Off).unwrap();
    }
    if mode == "diagnostic" {
        c.start_capture().unwrap();
    }
    tracing::dispatcher::set_global_default(c.dispatch()).unwrap();
    let start = Instant::now();
    match scenario {
        "operations" => {
            // Work cannot be optimized out, and every mode performs the same arithmetic.
            for i in 0..n {
                let op = Operation::start("execution");
                let mut x = i as u64;
                for _ in 0..100 {
                    x = black_box(x.wrapping_mul(1664525).wrapping_add(1013904223));
                }
                black_box(x);
                op.finish("ok");
            }
        }
        "engine" => {
            let runtime = wes::runtime::launch(wes::runtime::RuntimeOptions::new(
                root.path().join("home"),
                root.path().into(),
            ))
            .await
            .unwrap();
            let session = runtime.handle.current().unwrap().session;
            for i in 0..n {
                session
                    .submit(
                        SourceInput::new(
                            format!("synthetic-{i}"),
                            format!(":calc {{ return {i} + 1; }} > result{i}"),
                        )
                        .unwrap(),
                    )
                    .await
                    .unwrap();
                session.wait_idle().await.unwrap();
            }
            assert_eq!(session.observe().await.unwrap().cells.len(), n);
            runtime.shutdown().await.unwrap();
        }
        "web" => {
            let runtime = wes::runtime::launch(wes::runtime::RuntimeOptions::new(
                root.path().join("home"),
                root.path().into(),
            ))
            .await
            .unwrap();
            let server = runtime.serve(0, None).await.unwrap();
            let client = reqwest::Client::builder().no_proxy().build().unwrap();
            let url = format!("http://{}/language/calc", server.address());
            for _ in 0..n {
                let response = client.get(&url).send().await.unwrap();
                assert!(response.status().is_success());
                black_box(response.bytes().await.unwrap());
            }
            server.shutdown().await.unwrap();
            runtime.shutdown().await.unwrap();
        }
        "idle" => {
            tokio::time::sleep(std::time::Duration::from_secs(n as u64)).await;
        }
        _ => panic!("scenario must be operations, engine, web, or idle"),
    }
    let elapsed = start.elapsed().as_secs_f64();
    let status = c.status();
    c.shutdown();
    println!(
        "{}",
        serde_json::json!({"mode":mode,"scenario":scenario,"iterations":n,"wall_seconds":elapsed,"status":status})
    );
}
