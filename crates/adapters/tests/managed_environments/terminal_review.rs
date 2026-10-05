use super::*;
use wes_engine::execution::TargetPreparation;

#[tokio::test]
async fn terminal_review_is_not_a_lease_and_requires_the_exact_current_revision() {
    let yaml = "version: 1\ntargets: {local: {kind: local, cwd: /}}\nenvironments: {qa: {targets: [local]}}";
    let f = Fixture::new(yaml);
    f.install().await;
    let first = f.handle.environment_revisions().await.unwrap()["qa"];
    assert!(matches!(
        f.handle
            .prepare_execution_target("qa".into(), first, "local".into())
            .await
            .unwrap(),
        TargetPreparation::Ready(_)
    ));
    std::fs::write(
        f.root.path().join("env.yaml"),
        yaml.replace("cwd: /", "cwd: /tmp"),
    )
    .unwrap();
    f.install().await;
    let TargetPreparation::Review(review) = f
        .handle
        .prepare_execution_target("qa".into(), first, "local".into())
        .await
        .unwrap()
    else {
        panic!("review required")
    };
    assert_eq!(review.previous.as_ref().unwrap().revision(), first);
    assert_eq!(review.target.cwd(), Some("/tmp"));
    let second = review.current.revision();
    assert!(matches!(
        f.handle
            .prepare_execution_target("qa".into(), second, "local".into())
            .await
            .unwrap(),
        TargetPreparation::Ready(_)
    ));
    std::fs::write(
        f.root.path().join("env.yaml"),
        yaml.replace("cwd: /", "cwd: /var"),
    )
    .unwrap();
    f.install().await;
    assert!(matches!(
        f.handle
            .prepare_execution_target("qa".into(), second, "local".into())
            .await
            .unwrap(),
        TargetPreparation::Review(_)
    ));
    let absent = format!("sha256:{}", "0".repeat(64)).parse().unwrap();
    let TargetPreparation::Review(missing) = f
        .handle
        .prepare_execution_target("qa".into(), absent, "local".into())
        .await
        .unwrap()
    else {
        panic!("review required")
    };
    assert!(missing.previous.is_none());
    assert!(
        f.handle
            .prepare_execution_target("qa".into(), first, "removed".into())
            .await
            .is_err()
    );
    f.submit("one", ":env disable \"qa\"").await;
    assert!(
        f.handle
            .prepare_execution_target("qa".into(), first, "local".into())
            .await
            .is_err()
    );
    f.submit("one", ":env enable \"qa\"").await;
    f.submit("one", ":env retire \"qa\"").await;
    assert!(
        f.handle
            .prepare_execution_target("qa".into(), first, "local".into())
            .await
            .is_err()
    );
    // Retaining the review evidence must not hold an execution reference or block deletion.
    f.submit("one", ":env delete \"qa\"").await;
    assert!(
        !f.handle
            .environment_revisions()
            .await
            .unwrap()
            .contains_key("qa")
    );
    drop((review, missing));
    f.stop().await;
}
