//! UI lifecycle and authenticated child bridge have separate request types and authority.
use super::*;
fn json_response(value: &impl serde::Serialize) -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        serde_json::to_vec(value).expect("JSON response"),
    )
        .into_response()
}
use serde_json::json;
mod review;
use review::target_review;
#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "lowercase", deny_unknown_fields)]
enum Action {
    Targets,
    Resolve {
        #[serde(default)]
        environment: Option<String>,
        #[serde(default)]
        target: Option<String>,
    },
    Start {
        #[serde(default)]
        target: Option<TargetSelection>,
        #[serde(default)]
        cwd: Option<PathBuf>,
        #[serde(default)]
        history: Option<String>,
    },
    Forget {
        history: String,
    },
    CommandClaim {
        id: String,
        request: String,
    },
    CommandReply {
        id: String,
        request: String,
        error: Option<String>,
    },
    EditorReply {
        id: String,
        request: String,
        result: serde_json::Value,
    },
    Poll {
        id: String,
        cursor: u64,
        #[serde(default)]
        wait_ms: u64,
    },
    Write {
        id: String,
        text: String,
    },
    Resize {
        id: String,
        cols: u16,
        rows: u16,
    },
    Close {
        id: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetSelection {
    environment: String,
    revision: String,
    target: String,
}
#[derive(Deserialize)]
struct UiRequest {
    client: String,
    #[serde(flatten)]
    action: Action,
}
/// Resolve command intent from one observation, without network I/O or changing selection.
/// Start revalidates the captured revision and acquires the existing authority lease.
fn resolve_terminal(
    observation: &wes_engine::session::SessionObservation,
    client: &str,
    environment: Option<String>,
    target: Option<String>,
) -> io::Result<serde_json::Value> {
    let selected = environment.or_else(|| {
        observation.environment_clients.get(client).map_or_else(
            || observation.default_environment.clone(),
            |context| context.selected.clone(),
        )
    });
    let Some(environment) = selected else {
        if !observation.environment_managed && target.is_none() {
            return Ok(json!({"target":null}));
        }
        return Err(io::Error::other(
            "No environment selected; use xterm env:NAME.",
        ));
    };
    let entries = observation
        .execution_targets
        .get(&environment)
        .ok_or_else(|| {
            io::Error::other(format!(
                "Environment '{environment}' is unavailable; use an existing runnable environment."
            ))
        })?;
    if !observation
        .environment_enabled
        .get(&environment)
        .copied()
        .unwrap_or(false)
    {
        return Err(io::Error::other(format!(
            "Environment '{environment}' execution is disabled; enable it before opening a terminal."
        )));
    }
    let target = match target {
        Some(target) => target,
        None if entries.len() == 1 => entries.keys().next().expect("one target").clone(),
        None if entries.is_empty() => {
            return Err(io::Error::other(format!(
                "Environment '{environment}' has no execution target; attach one before opening a terminal."
            )));
        }
        None => {
            return Err(io::Error::other(format!(
                "Environment '{environment}' has multiple execution targets; specify target:NAME. Targets: {}",
                entries.keys().cloned().collect::<Vec<_>>().join(", ")
            )));
        }
    };
    let captured = entries
        .get(&target)
        .ok_or_else(|| {
            io::Error::other(format!(
                "Target '{target}' is not attached to environment '{environment}'."
            ))
        })?
        .as_ref()
        .map_err(|error| {
            io::Error::other(format!(
                "Target '{target}' in '{environment}': {}",
                error.message
            ))
        })?;
    wes_adapters::execution_targets::terminal_support(captured).map_err(|reason| {
        io::Error::other(format!(
            "Target '{target}' in '{environment}' cannot open a terminal: {reason}"
        ))
    })?;
    Ok(
        json!({"target":{"environment":environment,"revision":observation.environment_revisions[&environment].to_string(),"target":target}}),
    )
}

pub(super) async fn action(super::Scoped(shared): super::Scoped, request: Request) -> Response {
    let Ok(current) = shared.application.current() else {
        return StatusCode::GONE.into_response();
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|h| h.to_str().ok())
        != Some(current.generation.as_str())
    {
        return StatusCode::CONFLICT.into_response();
    }
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        != Some("application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(Ok(bytes)) = tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), 128 * 1024),
    )
    .await
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(request) = serde_json::from_slice::<UiRequest>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let result = shared
        .encoders
        .clone()
        .spawn(async move {
            let client = &request.client;
            let manager = &shared.terminals;
            let generation = &current.generation;
            match request.action {
                Action::Resolve { environment, target } => {
                    let observation = current.session.observe().await.map_err(io::Error::other)?;
                    resolve_terminal(&observation, client, environment, target)
                }
                Action::Targets => {
                    let observation=current.session.observe().await.map_err(io::Error::other)?;
                    let selected=observation.environment_clients.get(client).map_or_else(|| observation.default_environment.clone(), |context| context.selected.clone());
                    let mut targets=vec![];
                    if let Some(environment)=selected.as_ref() && let Some(entries)=observation.execution_targets.get(environment) {
                        for (name,target) in entries {
                            let support=target.as_ref().map_err(|e|e.message.as_str()).and_then(|target|wes_adapters::execution_targets::terminal_support(target));
                            let enabled=observation.environment_enabled.get(environment).copied().unwrap_or(false);
                            let reason=if !enabled {Some("Environment execution is disabled")} else {support.as_ref().err().copied()};
                            targets.push(json!({"environment":environment,"revision":observation.environment_revisions[environment].to_string(),"target":name,
                                "label":support.as_ref().map(|s|s.label).unwrap_or("Unavailable"),"workspace_tools":support.as_ref().is_ok_and(|s|s.workspace_tools),"available":enabled && support.is_ok(),"reason":reason}));
                        }
                    }
                    Ok(json!({"environment":selected,"targets":targets}))
                }
                Action::CommandClaim { id, request } => Ok(json!({"claimed": manager.command_claim(&id, generation, client, &request)?})),
                Action::CommandReply { id, request, error } => {
                    manager.command_reply(&id, generation, client, &request, error)?;
                    Ok(json!({"accepted":true}))
                }
                Action::EditorReply {
                    id,
                    request,
                    result,
                } => {
                    manager.editor_reply(&id, generation, client, &request, result)?;
                    Ok(json!({"accepted":true}))
                }
                Action::Forget { history } => {
                    let home = shared.services.terminal.as_ref().and_then(|c| c.history_home.as_deref())
                        .ok_or_else(|| io::Error::other("Terminal history is unavailable in this server."))?;
                    manager.forget(home, &history, client).await?;
                    Ok(json!({"forgotten":true}))
                }
                Action::Start { cwd, history, target } => {
                    let mut config = shared.services.terminal.clone().ok_or_else(|| {
                        io::Error::other("Terminal is not configured in this server.")
                    })?;
                    if target.is_some() && cwd.is_some() {return Err(io::Error::other("A captured target owns its working directory; remove the host cwd override"));}
                    let lease=match target {
                        Some(target)=>match current.session.prepare_execution_target(target.environment,target.revision.parse().map_err(io::Error::other)?,target.target).await.map_err(io::Error::other)? {
                            wes_engine::execution::TargetPreparation::Ready(lease) => Some(lease),
                            wes_engine::execution::TargetPreparation::Review(review) => return target_review(&review),
                        },
                        None=>None,
                    };
                    let workspace_tools=match &lease {Some(lease)=>wes_adapters::execution_targets::terminal_support(&lease.target).map_err(io::Error::other)?.workspace_tools,None=>true};
                    let label=lease.as_ref().map(|lease|format!("{} / {} @ {}",lease.environment,lease.target.name(),lease.revision));
                    if let Some(cwd) = cwd {
                        if !cwd.is_absolute() || !cwd.is_dir() {
                            return Err(io::Error::other("Saved terminal directory is unavailable; close this pane and open a new terminal."));
                        }
                        config.cwd = cwd;
                    }
                    config.history = history;
                    let directory = if workspace_tools {Some(lease.as_ref().and_then(|lease|lease.target.cwd().map(PathBuf::from)).unwrap_or_else(||config.cwd.clone()))} else {None};
                    let id = manager
                        .start_target(
                            config,
                            current.clone(),
                            request.client.clone(),
                            shared.port,
                            shared.application.clone(),
                            lease,
                        )
                        .await?;
                    let destination = manager.poll(&id, generation, client, 0)?.destination;
                    Ok::<_, io::Error>(json!({"id": id, "cwd": directory, "workspace_tools":workspace_tools,"target":label,"destination":destination}))
                }
                Action::Poll {
                    id,
                    cursor,
                    wait_ms,
                } => Ok(json!(
                    manager
                        .poll_wait(&id, generation, client, cursor, wait_ms)
                        .await?
                )),
                Action::Write { id, text } => {
                    manager.write(&id, generation, client, text)?;
                    Ok(json!({"accepted":true}))
                }
                Action::Resize { id, cols, rows } => {
                    manager.resize(&id, generation, client, cols, rows)?;
                    Ok(json!({"accepted":true}))
                }
                Action::Close { id } => {
                    let problem=manager.close_joined(&id, generation, client).await?;
                    Ok(json!({"closed":true,"problem":problem}))
                }
            }
        })
        .await;
    match result {
        Ok(Ok(json)) => json_response(&json),
        Ok(Err(error)) => {
            let status = if error
                .get_ref()
                .is_some_and(|e| e.is::<crate::terminal::Unavailable>())
            {
                StatusCode::GONE
            } else {
                StatusCode::BAD_REQUEST
            };
            (status, error.to_string()).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}
pub(super) async fn bridge(State(shared): State<Shared>, request: Request) -> Response {
    let Some(token) = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| s.len() <= 128)
        .map(str::to_owned)
    else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|h| h.to_str().ok())
        != Some("application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(permit) = shared.terminal_calls.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Ok(Ok(bytes)) = tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), 128 * 1024),
    )
    .await
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(request) = serde_json::from_slice::<crate::terminal::BridgeRequest>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let result = shared
        .encoders
        .clone()
        .spawn(async move {
            let _permit = permit;
            shared
                .terminals
                .bridge(&token, request, shared.application)
                .await
        })
        .await;
    match result {
        Ok(reply) => json_response(&reply),
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Store the selected mode explicitly in source so replay/refresh cannot reinterpret it later.
/// Only direct, single process calls acquire the console default; scripts/templates keep their
/// authored semantics. Explicit annotations always remain authoritative.
pub(super) fn console_source(
    text: String,
    observation: &wes_engine::session::SessionObservation,
    selected: Option<&String>,
) -> String {
    let parsed = wes_language::parse(&wes_language::SourceText::new("<console>", &text));
    if !parsed.diagnostics.is_empty() || parsed.script.statements.len() != 1 {
        return text;
    }
    let statement = &parsed.script.statements[0];
    let wes_language::Expression::Call(call) = &statement.expression else {
        return text;
    };
    if call.marker.is_some() || !statement.annotations.is_empty() {
        return text;
    }
    let Some(provider) = call.path.first() else {
        return text;
    };
    if observation
        .interactive_providers
        .get(&selected.cloned())
        .is_some_and(|names| names.contains(&provider.text))
    {
        format!("@interactive {text}")
    } else {
        text
    }
}

#[cfg(test)]
mod context_tests {
    use super::*;
    #[tokio::test]
    async fn command_resolution_has_one_rule_and_never_falls_back_from_a_managed_context() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::runtime::launch(crate::runtime::RuntimeOptions::new(
            root.path().join("home"),
            root.path().into(),
        ))
        .await
        .unwrap();
        let current = runtime.handle.current().unwrap();
        let mut observation = current.session.observe().await.unwrap();
        let original = resolve_terminal(&observation, "ui", None, None).unwrap();
        assert_eq!(original["target"]["environment"], "default");
        assert_eq!(original["target"]["target"], "local");
        assert_eq!(
            resolve_terminal(
                &observation,
                "ui",
                Some("default".into()),
                Some("local".into())
            )
            .unwrap(),
            original
        );
        assert!(
            resolve_terminal(&observation, "ui", Some("missing".into()), None)
                .unwrap_err()
                .to_string()
                .contains("'missing'")
        );
        assert!(
            resolve_terminal(&observation, "ui", None, Some("missing".into()))
                .unwrap_err()
                .to_string()
                .contains("not attached")
        );
        observation
            .environment_enabled
            .insert("default".into(), false);
        assert!(
            resolve_terminal(&observation, "ui", None, None)
                .unwrap_err()
                .to_string()
                .contains("disabled")
        );
        observation
            .environment_enabled
            .insert("default".into(), true);
        let local = observation.execution_targets["default"]["local"].clone();
        observation
            .execution_targets
            .get_mut("default")
            .unwrap()
            .insert("another".into(), local);
        assert!(
            resolve_terminal(&observation, "ui", None, None)
                .unwrap_err()
                .to_string()
                .contains("specify target:NAME")
        );
        assert_eq!(
            resolve_terminal(&observation, "ui", None, Some("local".into())).unwrap(),
            original
        );
        observation
            .execution_targets
            .get_mut("default")
            .unwrap()
            .clear();
        assert!(
            resolve_terminal(&observation, "ui", None, None)
                .unwrap_err()
                .to_string()
                .contains("no execution target")
        );
        observation
            .execution_targets
            .get_mut("default")
            .unwrap()
            .insert(
                "broken".into(),
                Err(wes_core::environments::EnvironmentError {
                    code: "ENV003",
                    message: "conflicting captures".into(),
                }),
            );
        assert!(
            resolve_terminal(&observation, "ui", None, None)
                .unwrap_err()
                .to_string()
                .contains("conflicting captures")
        );
        observation.environment_clients.insert(
            "ui".into(),
            wes_core::environments::EnvironmentContext {
                selected: None,
                revisions: observation.environment_revisions.clone(),
            },
        );
        assert!(
            resolve_terminal(&observation, "ui", None, None)
                .unwrap_err()
                .to_string()
                .contains("No environment selected")
        );
        observation.environment_managed = false;
        assert_eq!(
            resolve_terminal(&observation, "ui", None, None).unwrap(),
            json!({"target":null})
        );
        runtime.shutdown().await.unwrap();
    }
}
