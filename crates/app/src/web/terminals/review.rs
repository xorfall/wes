//! Public, redacted terminal review evidence. Values used as secrets are never serialized.
use super::*;
use std::collections::BTreeMap;
use wes_core::environments::{DockerDestination, Target, TargetKind};

fn target_summary(target: &Target) -> serde_json::Value {
    let (kind, destination, shell) = match target.kind() {
        TargetKind::Local => ("Local", "This computer".into(), "Login shell".into()),
        TargetKind::Ssh(ssh) => (
            "SSH",
            format!("{}@{}:{}", ssh.user, ssh.host, ssh.port),
            "Remote POSIX shell".into(),
        ),
        TargetKind::Docker {
            destination, shell, ..
        } => (
            "Docker",
            match destination {
                DockerDestination::Container(name) => name.clone(),
                DockerDestination::Compose {
                    project,
                    service,
                    replica,
                } => format!(
                    "{project}/{service}{}",
                    replica.map(|n| format!(" replica {n}")).unwrap_or_default()
                ),
            },
            shell.clone().unwrap_or_else(|| "/bin/sh".into()),
        ),
    };
    let transport = match target.kind() {
        TargetKind::Local => json!({}),
        TargetKind::Ssh(ssh) => {
            json!({"SSH client":ssh.client,"Identity file":ssh.identity_file,"Known hosts":ssh.known_hosts})
        }
        TargetKind::Docker { socket, image, .. } => json!({"Docker socket":socket,"Image":image}),
    };
    json!({"transport":transport,"kind":kind,"destination":destination,"shell":shell,"cwd":target.cwd(),"variables":target.variables().keys().collect::<Vec<_>>()})
}
fn changed_keys<T: PartialEq>(
    before: &BTreeMap<String, T>,
    after: &BTreeMap<String, T>,
) -> Vec<String> {
    before
        .keys()
        .chain(after.keys())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .filter(|name| before.get(*name) != after.get(*name))
        .cloned()
        .collect()
}
pub(super) fn target_review(
    review: &wes_engine::execution::TargetReview,
) -> io::Result<serde_json::Value> {
    wes_adapters::execution_targets::terminal_support(&review.target).map_err(io::Error::other)?;
    let current = &review.current;
    let previous_target = review
        .previous
        .as_ref()
        .and_then(|old| old.execution_targets().remove(review.target.name()))
        .and_then(Result::ok);
    let providers: Vec<_> = current.imports().keys().cloned().collect();
    let changes = review.previous.as_ref().map(|old| {
        let added: Vec<_> = current.imports().keys().filter(|key| !old.imports().contains_key(*key)).cloned().collect();
        let removed: Vec<_> = old.imports().keys().filter(|key| !current.imports().contains_key(*key)).cloned().collect();
        let updated: Vec<_> = current.imports().iter().filter(|(key,value)| old.imports().get(*key).is_some_and(|before| before != *value)).map(|(key,_)|key.clone()).collect();
        json!({"added":added,"removed":removed,"updated":updated,
            "configuration":changed_keys(old.config(),current.config()),
            "credentialReferences":changed_keys(old.secret_refs(),current.secret_refs()),
            "targetChanged":previous_target.as_ref() != Some(&review.target),
            "variables":previous_target.as_ref().map(|before| changed_keys(before.variables(),review.target.variables()))})
    });
    Ok(json!({"review":{
        "target":{"environment":current.name(),"revision":current.revision().to_string(),"target":review.target.name()},
        "previousRevision":review.previous_revision.to_string(),"previousAvailable":review.previous.is_some(),
        "before":previous_target.as_deref().map(target_summary),"after":target_summary(&review.target),
        "providers":providers,"changes":changes
    }}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_engine::execution::TargetPreparation;

    #[tokio::test]
    async fn review_shows_transport_and_provider_changes_without_disclosing_values() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::runtime::launch(crate::runtime::RuntimeOptions::new(
            root.path().join("home"),
            root.path().into(),
        ))
        .await
        .unwrap();
        let current = runtime.handle.current().unwrap();
        let mut recipe = json!({"version":1, "targets":{"review-target":{"kind":"local","env":{"QA_VALUE":"private-before"}}},
            "environments":{"review-qa":{"targets":["review-target"],
                "parameters":{"setting":{"type":"Text"}},"config":{"setting":"private-config-before"},
                "secretSlots":{"token":{"required":false}}, "secretRefs":{"token":"private-ref-before"},
                "imports":{"echo":{"source":{"kind":"process","bin":"/bin/echo"},"bind":{"target":"review-target"}}}}}});
        async fn apply(
            root: &std::path::Path,
            session: &wes_engine::session::SessionHandle,
            recipe: &serde_json::Value,
        ) {
            std::fs::write(root.join("review.yaml"), recipe.to_string()).unwrap();
            let plan = session
                .plan_environment_file("review.yaml".into(), false)
                .await
                .unwrap();
            session.apply_environments(plan).await.unwrap();
        }
        apply(root.path(), &current.session, &recipe).await;
        let first = current.session.environment_revisions().await.unwrap()["review-qa"];
        std::fs::write(root.path().join("key"), b"synthetic key").unwrap();
        std::fs::write(root.path().join("hosts"), b"synthetic hosts").unwrap();
        recipe["targets"]["review-target"] = json!({"kind":"ssh", "host":"synthetic.invalid", "user":"qa", "client":"/synthetic/ssh", "identity_file":root.path().join("key"), "known_hosts":root.path().join("hosts"), "shell":"posix","inherit":"remote", "env":{"QA_VALUE":"private-after"}});
        recipe["environments"]["review-qa"]["config"]["setting"] = json!("private-config-after");
        recipe["environments"]["review-qa"]["secretRefs"]["token"] = json!("private-ref-after");
        recipe["environments"]["review-qa"]["imports"]["vars"] = json!({"source":{"kind":"process","bin":"/usr/bin/printenv"},"bind":{"target":"review-target"}});
        apply(root.path(), &current.session, &recipe).await;
        let TargetPreparation::Review(review) = current
            .session
            .prepare_execution_target("review-qa".into(), first, "review-target".into())
            .await
            .unwrap()
        else {
            panic!("review required")
        };
        let response = target_review(&review).unwrap();
        let evidence = &response["review"];
        assert_eq!(evidence["before"]["kind"], "Local");
        assert_eq!(evidence["after"]["kind"], "SSH");
        assert_eq!(evidence["after"]["destination"], "qa@synthetic.invalid:22");
        assert_eq!(
            evidence["after"]["transport"]["Identity file"],
            root.path().join("key").to_str().unwrap()
        );
        assert_eq!(evidence["changes"]["added"], json!(["vars"]));
        assert_eq!(evidence["changes"]["updated"], json!(["echo"]));
        assert_eq!(evidence["changes"]["configuration"], json!(["setting"]));
        assert_eq!(
            evidence["changes"]["credentialReferences"],
            json!(["token"])
        );
        assert_eq!(evidence["changes"]["variables"], json!(["QA_VALUE"]));
        assert!(!response.to_string().contains("private-"));
        let second = review.current.revision();
        recipe["targets"]["review-target"] = json!({"kind":"docker", "socket":"/synthetic/docker.sock", "container":"fixture", "shell":"/bin/bash", "inherit":"container"});
        recipe["environments"]["review-qa"]["imports"]
            .as_object_mut()
            .unwrap()
            .remove("vars");
        apply(root.path(), &current.session, &recipe).await;
        let TargetPreparation::Review(review) = current
            .session
            .prepare_execution_target("review-qa".into(), second, "review-target".into())
            .await
            .unwrap()
        else {
            panic!("review required")
        };
        let response = target_review(&review).unwrap();
        assert_eq!(response["review"]["after"]["kind"], "Docker");
        assert_eq!(
            response["review"]["after"]["transport"]["Docker socket"],
            "/synthetic/docker.sock"
        );
        assert_eq!(response["review"]["changes"]["removed"], json!(["vars"]));
        let missing = wes_engine::execution::TargetReview {
            previous: None,
            ..review
        };
        let response = target_review(&missing).unwrap();
        assert_eq!(response["review"]["previousAvailable"], false);
        assert!(response["review"]["changes"].is_null());
        assert!(response["review"]["before"].is_null());
        runtime.shutdown().await.unwrap();
    }
}
