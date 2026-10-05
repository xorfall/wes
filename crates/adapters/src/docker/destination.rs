//! Resolve one captured destination per operation; callers keep the resulting full ID.
use super::{
    client::{ClientError, DockerEngineClient},
    digest,
};
use serde_json::{Value as Json, json};
use wes_core::environments::{DockerDestination, Target, TargetKind};

#[derive(Clone, Debug)]
pub(super) struct Resolved {
    pub id: String,
    pub image: String,
    pub selector: Option<String>,
}

pub(super) async fn resolve(
    client: &DockerEngineClient,
    api: &str,
    target: &Target,
) -> Result<Resolved, String> {
    let TargetKind::Docker {
        destination, image, ..
    } = target.kind()
    else {
        unreachable!()
    };
    let (reference, selector) = match destination {
        DockerDestination::Container(reference) => (reference.clone(), None),
        DockerDestination::Compose {
            project,
            service,
            replica,
        } => {
            let selector = format!(
                "{project}/{service}{}",
                replica.map(|n| format!("/{n}")).unwrap_or_default()
            );
            let mut labels = vec![
                format!("com.docker.compose.project={project}"),
                format!("com.docker.compose.service={service}"),
                "com.docker.compose.oneoff=False".into(),
            ];
            if let Some(replica) = replica {
                labels.push(format!("com.docker.compose.container-number={replica}"));
            }
            let query = url::form_urlencoded::Serializer::new(String::new())
                .append_pair("all", "false")
                .append_pair(
                    "filters",
                    &json!({"label":labels,"status":["running"]}).to_string(),
                )
                .finish();
            let list = client
                .json(client.http.get(format!("{api}/containers/json?{query}")))
                .await
                .map_err(before_start)?;
            let rows = list
                .as_array()
                .filter(|rows| rows.len() <= 4096)
                .ok_or("ENV034: Docker container list is malformed or exceeds 4096 entries")?;
            let mut ids = std::collections::BTreeSet::new();
            for row in rows {
                // Never trust daemon filtering, truncated IDs or a label subset.
                if matches_labels(row.get("Labels"), destination)
                    && row.get("State").and_then(Json::as_str) == Some("running")
                {
                    let id = row
                        .get("Id")
                        .and_then(Json::as_str)
                        .filter(|id| digest(id))
                        .ok_or("ENV034: invalid Compose container identity")?;
                    ids.insert(id.to_owned());
                }
            }
            match ids.len() {
                0 => {
                    return Err(format!(
                        "ENV038: Compose target {selector} has no running service container"
                    ));
                }
                1 => (ids.pop_first().unwrap(), Some(selector)),
                n => {
                    return Err(format!(
                        "ENV038: Compose target {selector} matches {n} running containers; select one replica"
                    ));
                }
            }
        }
    };
    let observed = client.json(client.http.get(format!("{api}/containers/{reference}/json"))).await.map_err(|error| {
        if matches!(error, ClientError::Status(404)) { format!("ENV032: Docker container {reference} is unavailable; no replacement was selected") } else { before_start(error) }
    })?;
    let id = observed
        .get("Id")
        .and_then(Json::as_str)
        .filter(|id| digest(id))
        .ok_or("ENV034: invalid observed container identity")?;
    if (selector.is_some() || digest(&reference)) && id != reference {
        return Err("ENV034: Docker inspect identity differs from the selected container".into());
    }
    if selector.is_some() && !matches_labels(observed.pointer("/Config/Labels"), destination) {
        return Err(
            "ENV038: inspected container no longer matches the captured Compose selector".into(),
        );
    }
    if observed.pointer("/State/Running").and_then(Json::as_bool) != Some(true)
        || observed.pointer("/State/Paused").and_then(Json::as_bool) == Some(true)
    {
        return Err(
            "ENV032: selected container is stopped or paused; wes does not start it".into(),
        );
    }
    let actual_image = observed
        .get("Image")
        .and_then(Json::as_str)
        .filter(|s| s.starts_with("sha256:") && digest(&s[7..]))
        .ok_or("ENV034: invalid observed image identity")?;
    if image
        .as_ref()
        .is_some_and(|expected| expected != actual_image)
    {
        return Err("ENV035: observed image does not match the captured constraint".into());
    }
    Ok(Resolved {
        id: id.into(),
        image: actual_image.into(),
        selector,
    })
}

fn matches_labels(labels: Option<&Json>, destination: &DockerDestination) -> bool {
    let DockerDestination::Compose {
        project,
        service,
        replica,
    } = destination
    else {
        return true;
    };
    let Some(labels) = labels.and_then(Json::as_object) else {
        return false;
    };
    let label = |name: &str| {
        labels
            .get(&format!("com.docker.compose.{name}"))
            .and_then(Json::as_str)
    };
    label("project") == Some(project.as_str())
        && label("service") == Some(service.as_str())
        && label("oneoff") == Some("False")
        && label("container-number").is_some_and(|n| {
            n.parse::<u32>()
                .is_ok_and(|n| n > 0 && replica.is_none_or(|expected| expected == n))
        })
}

pub(super) fn before_start(error: ClientError) -> String {
    match error {
        ClientError::Status(404) => {
            "ENV032: Docker container or exec instance is unavailable".into()
        }
        ClientError::Status(status) => {
            format!("ENV033: Docker daemon rejected setup (HTTP {status})")
        }
        ClientError::Metadata => "ENV034: invalid or excessive Docker metadata".into(),
        other => format!("ENV031: {other} before exec start"),
    }
}
