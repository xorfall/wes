//! Narrow finite observations. No raw inspect payload, ambient daemon lookup or private cache.
mod lifecycle;
mod logs;
mod metrics;
mod streaming;

use super::{
    client::{ClientError, DockerEngineClient},
    digest,
};
use serde_json::Value as Json;
use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use wes_core::{
    Data, Primitive, Provenance, RecordShape, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, ResourceProjection, Safety},
    environments::Binding,
};
use wes_engine::{
    driver::CancellationToken,
    environments::Authority,
    imports::ImportProduct,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
};

#[derive(Clone)]
struct Observation {
    connection: String,
    client: DockerEngineClient,
    binding: Binding,
    authority: Authority,
}
fn primitive(p: Primitive) -> Shape {
    Shape::Primitive(p)
}
fn record(name: &str, fields: Vec<(&str, Shape)>) -> Shape {
    Shape::Record(
        RecordShape::new(
            name,
            fields.into_iter().map(|(key, shape)| (key.into(), shape)),
        )
        .expect("static observation fields"),
    )
}
fn option(shape: Shape) -> Shape {
    Shape::Option(Box::new(shape))
}
fn row_shape(inspect: bool) -> Shape {
    let mut fields = vec![
        ("id", primitive(Primitive::Text)),
        ("name", primitive(Primitive::Text)),
        ("image", primitive(Primitive::Text)),
        ("state", primitive(Primitive::Text)),
        ("created", primitive(Primitive::Text)),
        ("project", option(primitive(Primitive::Text))),
        ("service", option(primitive(Primitive::Text))),
        ("replica", option(primitive(Primitive::Text))),
    ];
    if inspect {
        fields.extend([
            ("running", primitive(Primitive::Bool)),
            ("exit_code", primitive(Primitive::Int)),
            ("oom_killed", primitive(Primitive::Bool)),
            ("restarts", primitive(Primitive::Int)),
            ("health", option(primitive(Primitive::Text))),
            ("health_failures", option(primitive(Primitive::Int))),
        ]);
    }
    record(
        if inspect {
            "ContainerInspection"
        } else {
            "Container"
        },
        fields,
    )
}
fn envelope(inspect: bool) -> Shape {
    record(
        if inspect {
            "DockerInspection"
        } else {
            "DockerInventory"
        },
        vec![
            ("observed_at_ns", primitive(Primitive::Int)),
            ("rows", Shape::List(Box::new(row_shape(inspect)))),
            ("returned", primitive(Primitive::Int)),
            ("omitted", primitive(Primitive::Int)),
            ("complete", primitive(Primitive::Bool)),
        ],
    )
}
pub(crate) fn build(
    alias: &str,
    binding: &Binding,
    authority: Authority,
) -> Result<ImportProduct, &'static str> {
    build_at(alias, binding.import().source().bytes(), binding, authority)
}
pub(crate) fn build_at(
    alias: &str,
    socket: &str,
    binding: &Binding,
    authority: Authority,
) -> Result<ImportProduct, &'static str> {
    let import = binding.import();
    if !crate::execution_targets::driver(import.target()).local_provider_io()
        || import.target().cwd().is_some()
        || !import.target().variables().is_empty()
        || (import.endpoint().is_some() && import.source().format() != "builtin/v1")
        || !import.credential_refs().is_empty()
    {
        return Err(
            "Docker requires a local target without process settings or credentials; use a built-in Docker source to bind an endpoint",
        );
    }
    if import.timeout_ms().is_some_and(|ms| ms > 3_600_000) {
        return Err("Docker timeout must not exceed one hour");
    }
    let invoker = Arc::new(Observation {
        connection: format!("docker:{socket}"),
        client: DockerEngineClient::new(socket)?,
        binding: binding.clone(),
        authority,
    });
    ImportProduct::new(description(alias)?, invoker.clone(), vec![])
        .map(|product| product.with_streams(invoker))
        .map_err(|_| "Docker observation metadata exceeds budget")
}
pub(super) fn description(alias: &str) -> Result<ProviderDescription, &'static str> {
    let mut containers = Capability::new(["containers"], envelope(false), Safety::Safe);
    containers.summary =
        "List a bounded inventory; refresh this node to update container suggestions".into();
    containers.parameters = vec![
        Parameter::new("project", primitive(Primitive::Text), false),
        Parameter::new("limit", primitive(Primitive::Int), false),
    ];
    containers.resources = Some(ResourceProjection {
        registry: "container".into(),
        rows: "rows".into(),
        key: "id".into(),
        label: "name".into(),
        detail: "state".into(),
        observed_at: "observed_at_ns".into(),
    });
    let mut inspect = Capability::new(["inspect"], envelope(true), Safety::Safe);
    inspect.summary = "Inspect one exact full container ID; excludes environment, command, mounts and raw health logs".into();
    inspect.parameters =
        vec![Parameter::new("container", primitive(Primitive::Text), true).suggesting("container")];
    ProviderDescription::new(
        alias,
        [
            containers,
            inspect,
            logs::capability(),
            logs::follow_capability(),
            metrics::stats_capability(),
            metrics::events_capability(),
        ]
        .into_iter()
        .chain(lifecycle::capabilities()),
        vec![],
    )
    .map_err(|_| "invalid Docker observation metadata")
}
pub(super) fn failure(code: &str, message: impl Into<String>) -> InvocationError {
    let message = message.into();
    let error = wes_engine::runtime::RuntimeCode::ExecutionFailed.error(&message, None);
    InvocationError::Failed(
        wes_core::ErrorValue::new(error.id().clone(), code, message, vec![], None)
            .expect("stable diagnostic"),
    )
}
fn transport(error: ClientError) -> InvocationError {
    let code = match error {
        ClientError::Version(..) => "DOCKER_VERSION",
        ClientError::Metadata => "DOCKER_RESPONSE",
        ClientError::Status(_) => "DOCKER_HTTP",
        ClientError::Transport => "DOCKER_TRANSPORT",
    };
    failure(code, error.to_string())
}
impl Invoker for Observation {
    fn invoke(&self, call: Call, token: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            if call.capability.safety == Safety::Unsafe {
                return this.effect(call, token).await;
            }
            let timeout =
                Duration::from_millis(this.binding.import().timeout_ms().unwrap_or(15_000) as u64);
            tokio::select! { biased;
                ()=token.cancelled()=>Err(InvocationError::Cancelled),
                value=tokio::time::timeout(timeout,this.read(call))=>value.unwrap_or_else(|_|Err(failure("DOCKER_TIMEOUT","Docker observation timed out; no mutation was requested"))),
            }
        })
    }
}
impl Observation {
    fn permitted(&self) -> Result<(), InvocationError> {
        if !self
            .authority
            .available(self.binding.environment().identity())
        {
            return Err(failure(
                "ENV020",
                "Docker observation environment is disabled",
            ));
        }
        Ok(())
    }
    async fn read(&self, call: Call) -> Result<Value, InvocationError> {
        self.permitted()?;
        match call.capability.path.as_slice() {
            [operation] if operation == "logs" => self.logs(call).await,
            [operation] if operation == "inspect" || operation == "containers" => {
                self.inventory(call).await
            }
            _ => Err(failure(
                "DOCKER_ARGUMENT",
                "Unknown Docker observation capability",
            )),
        }
    }
    async fn inventory(&self, call: Call) -> Result<Value, InvocationError> {
        let inspect = call.capability.path == ["inspect"];
        let limit = match call.arguments.get("limit").map(Value::data) {
            None => 100,
            Some(Data::Int(n)) if (1..=1000).contains(n) => *n as usize,
            _ => {
                return Err(failure(
                    "DOCKER_ARGUMENT",
                    "limit must be between 1 and 1000",
                ));
            }
        };
        let container = match call.arguments.get("container").map(Value::data) {
            Some(Data::Text(id)) if digest(id) => Some(id.clone()),
            None if !inspect => None,
            _ => {
                return Err(failure(
                    "DOCKER_ARGUMENT",
                    "container requires a full 64-character Docker ID; run containers to select one",
                ));
            }
        };
        let api = self.client.endpoint().await.map_err(transport)?;
        self.permitted()?;
        let request = if let Some(id) = &container {
            self.client.http.get(format!("{api}/containers/{id}/json"))
        } else {
            let mut url =
                url::Url::parse(&format!("{api}/containers/json?all=true")).expect("constant URL");
            if let Some(Data::Text(project)) = call.arguments.get("project").map(Value::data) {
                if project.is_empty()
                    || project.len() > 128
                    || project.chars().any(char::is_control)
                {
                    return Err(failure(
                        "DOCKER_ARGUMENT",
                        "project must be 1..128 printable characters",
                    ));
                }
                url.query_pairs_mut().append_pair(
                    "filters",
                    &serde_json::json!({"label":[format!("com.docker.compose.project={project}")]})
                        .to_string(),
                );
            }
            self.client.http.get(url)
        };
        let json=self.client.json(request).await.map_err(|error| if matches!(error,ClientError::Status(404)) && container.is_some() {failure("DOCKER_MISSING",format!("Container {} no longer exists; refresh inventory and explicitly select its replacement",container.as_deref().unwrap()))} else {transport(error)})?;
        self.permitted()?;
        let mut entries = if inspect {
            vec![json]
        } else {
            json.as_array().ok_or_else(malformed)?.clone()
        };
        entries.sort_by(|a, b| {
            a.get("Id")
                .and_then(Json::as_str)
                .cmp(&b.get("Id").and_then(Json::as_str))
        });
        // Validate all bounded rows before applying row selection. No partial success on malformed input.
        let rows = entries
            .iter()
            .map(|entry| row(entry, inspect))
            .collect::<Result<Vec<_>, _>>()?;
        if let Some(id) = &container
            && entries[0].get("Id").and_then(Json::as_str) != Some(id.as_ref())
        {
            return Err(failure(
                "DOCKER_IDENTITY",
                "Docker returned a different container identity",
            ));
        }
        let omitted = rows.len().saturating_sub(limit);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| malformed())?
            .as_nanos()
            .try_into()
            .map_err(|_| malformed())?;
        let data = Data::Record(indexmap::IndexMap::from_iter([
            ("observed_at_ns".into(), Data::Int(now)),
            ("returned".into(), Data::Int(rows.len().min(limit) as i64)),
            ("omitted".into(), Data::Int(omitted as i64)),
            ("complete".into(), Data::Bool(omitted == 0)),
            (
                "rows".into(),
                Data::List(rows.into_iter().take(limit).collect()),
            ),
        ]));
        Value::new(
            envelope(inspect),
            data,
            Provenance::default().with_fact("docker.api", "1.45"),
        )
        .map_err(|_| malformed())
    }
}
fn malformed() -> InvocationError {
    failure(
        "DOCKER_RESPONSE",
        "Docker returned malformed observation fields; no partial result was published",
    )
}
fn text(value: &Json) -> Result<Data, InvocationError> {
    let s = value.as_str().ok_or_else(malformed)?;
    if s.len() > 4096 || s.chars().any(char::is_control) {
        return Err(malformed());
    }
    Ok(Data::Text(s.into()))
}
fn optional(value: Option<&Json>, kind: Primitive) -> Result<Data, InvocationError> {
    Ok(Data::Option(
        value
            .filter(|v| !v.is_null())
            .map(|v| match kind {
                Primitive::Int => v.as_i64().map(Data::Int).ok_or_else(malformed),
                _ => text(v),
            })
            .transpose()?
            .map(Box::new),
    ))
}
fn row(value: &Json, inspect: bool) -> Result<Data, InvocationError> {
    let id = value
        .get("Id")
        .and_then(Json::as_str)
        .filter(|s| digest(s))
        .ok_or_else(malformed)?;
    let name = if inspect {
        value.get("Name").and_then(Json::as_str)
    } else {
        value
            .get("Names")
            .and_then(Json::as_array)
            .and_then(|n| n.first())
            .and_then(Json::as_str)
    }
    .ok_or_else(malformed)?;
    let state = if inspect {
        &value["State"]["Status"]
    } else {
        &value["State"]
    };
    let created = if inspect {
        text(&value["Created"])?
    } else {
        Data::Text(
            value["Created"]
                .as_i64()
                .ok_or_else(malformed)?
                .to_string()
                .into(),
        )
    };
    let mut fields = indexmap::IndexMap::from_iter([
        ("id".into(), Data::Text(id.into())),
        (
            "name".into(),
            text(&Json::String(name.trim_start_matches('/').into()))?,
        ),
        (
            "image".into(),
            text(&value[if inspect { "Image" } else { "ImageID" }])?,
        ),
        ("state".into(), text(state)?),
        ("created".into(), created),
    ]);
    let labels = if inspect {
        &value["Config"]["Labels"]
    } else {
        &value["Labels"]
    };
    for (key, label) in [
        ("project", "project"),
        ("service", "service"),
        ("replica", "container-number"),
    ] {
        fields.insert(
            key.into(),
            optional(
                labels.get(format!("com.docker.compose.{label}")),
                Primitive::Text,
            )?,
        );
    }
    if inspect {
        for (key, path) in [("running", "Running"), ("oom_killed", "OOMKilled")] {
            fields.insert(
                key.into(),
                Data::Bool(value["State"][path].as_bool().ok_or_else(malformed)?),
            );
        }
        fields.insert(
            "exit_code".into(),
            Data::Int(value["State"]["ExitCode"].as_i64().ok_or_else(malformed)?),
        );
        fields.insert(
            "restarts".into(),
            Data::Int(value["RestartCount"].as_i64().ok_or_else(malformed)?),
        );
        fields.insert(
            "health".into(),
            optional(value.pointer("/State/Health/Status"), Primitive::Text)?,
        );
        fields.insert(
            "health_failures".into(),
            optional(value.pointer("/State/Health/FailingStreak"), Primitive::Int)?,
        );
    }
    Ok(Data::Record(fields))
}
