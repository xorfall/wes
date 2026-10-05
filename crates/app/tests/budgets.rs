//! Each process gets one immutable startup policy; fixtures never read the user's settings.
use std::{collections::BTreeMap, process::Command};
use wes::budgets::{Change, Store};
#[test]
fn reopening_applies_saved_budgets_and_saving_never_reconfigures_the_current_process() {
    let home = tempfile::tempdir().unwrap();
    let store = Store::for_user_home(home.path());
    store
        .save(Change {
            revision: 0,
            values: [
                ("execution.operations".into(), 8),
                ("execution.streams".into(), 48),
                ("history.window.entries".into(), 20),
                ("calc.work".into(), 64),
                ("view.instances".into(), 256),
                ("ui.panes".into(), 8),
            ]
            .into(),
        })
        .unwrap();
    for operations in [8, 12] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args(["--exact", "budget_profile_child", "--nocapture"])
            .env("WES_BUDGET_FIXTURE_HOME", home.path())
            .env("WES_BUDGET_FIXTURE_OPERATIONS", operations.to_string())
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    assert_eq!(wes_budgets::get("execution.operations"), 4);
}
#[tokio::test]
async fn budget_profile_child() {
    let Some(home) = std::env::var_os("WES_BUDGET_FIXTURE_HOME") else {
        return;
    };
    let home = std::path::PathBuf::from(home);
    let operations: usize = std::env::var("WES_BUDGET_FIXTURE_OPERATIONS")
        .unwrap()
        .parse()
        .unwrap();
    wes::budgets::initialize(&home).unwrap();
    let options = wes::runtime::RuntimeOptions::new(home.join("data"), home.clone());
    assert_eq!(options.concurrency.get(), operations);
    assert_eq!(options.max_streams.get(), 48);
    assert_eq!(wes_engine::calc::Limits::default().work, 64);
    assert_eq!(wes_engine::views::Limits::default().instances, 256);
    assert_eq!(wes_budgets::get("history.window.entries"), 20);
    assert_eq!(wes_budgets::get("ui.panes"), 8);
    let store = wes::budgets::configured_store().unwrap();
    let profile = store.read().unwrap();
    if operations == 8 {
        store
            .save(Change {
                revision: profile.revision,
                values: [("execution.operations".into(), 12)].into(),
            })
            .unwrap();
    }
    assert_eq!(wes_budgets::get("execution.operations"), operations as u64);
    assert!(wes_budgets::activate(BTreeMap::new()).is_err());
    // Mount the actual native composition without source execution, Docker discovery or credentials.
    let mut options = options;
    options.docker_candidates = Some(vec![home.join("synthetic-daemon.sock")]);
    let runtime = wes::runtime::launch(options).await.unwrap();
    let services = wes::runtime::browser_services(runtime.home().to_owned(), home).unwrap();
    assert!(services.budgets.is_some());
    runtime.shutdown().await.unwrap();
}
