//! User controls for imported HTTP authentication. Secrets never enter source or library artifacts.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;
use wes_engine::session::ProviderAuthorityCommand;

#[derive(Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
enum Action {
    Configure {
        environment: String,
        revision: String,
        provider: String,
        auth: BTreeMap<String, Vec<String>>,
    },
    Supply {
        environment: String,
        revision: String,
        provider: String,
        slot: String,
        value: String,
        #[serde(default)]
        remember: bool,
    },
    Forget {
        environment: String,
        revision: String,
        provider: String,
        slot: String,
    },
    Grant {
        environment: String,
        revision: String,
        provider: String,
    },
    Revoke {
        environment: String,
        revision: String,
        provider: String,
    },
    Enable {
        environment: String,
        revision: String,
        provider: String,
    },
}
fn current(shared: &Shared, request: &Request) -> Result<crate::CurrentSession, Response> {
    let current = shared
        .application
        .current()
        .map_err(|_| StatusCode::GONE.into_response())?;
    if request
        .headers()
        .get("X-Wes-Session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return Err((StatusCode::CONFLICT, "Workspace changed; reopen /env.").into_response());
    }
    Ok(current)
}
pub(super) async fn read(Scoped(shared): Scoped, request: Request) -> Response {
    let current = match current(&shared, &request) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result = shared
        .encoders
        .spawn(async move {
            let _permit = permit;
            report(&current).await
        })
        .await;
    respond(result)
}
pub(super) async fn change(Scoped(shared): Scoped, request: Request) -> Response {
    let current = match current(&shared, &request) {
        Ok(c) => c,
        Err(r) => return r,
    };
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|v| v != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let bytes = match tokio::time::timeout(
        Duration::from_secs(10),
        to_bytes(request.into_body(), 128 * 1024),
    )
    .await
    {
        Ok(Ok(b)) => b,
        _ => return StatusCode::PAYLOAD_TOO_LARGE.into_response(),
    };
    // Do not return deserializer errors: they can quote a submitted secret.
    let action = match serde_json::from_slice::<Action>(&bytes) {
        Ok(a) => a,
        Err(_) => {
            return (StatusCode::BAD_REQUEST, "Invalid authentication request.").into_response();
        }
    };
    let Ok(permit) = shared.queries.clone().try_acquire_owned() else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let result=shared.encoders.spawn(async move {
        let _permit=permit;
        current.session.check_retirement_access().await.map_err(|e|e.to_string())?;
        match action {
            Action::Configure {environment,revision,provider,auth}=>configure(&current,environment,revision,provider,auth).await?,
            action=>{
                let (environment,revision,provider,command)=match action {
                    Action::Supply {environment,revision,provider,slot,value,remember}=>(environment,revision,provider,ProviderAuthorityCommand::SupplyWithPersistence{slot,value:Arc::new(wes_engine::credentials::SecretString::from(value)),remember}),
                    Action::Forget {environment,revision,provider,slot}=>(environment,revision,provider,ProviderAuthorityCommand::Forget{slot}),
                    Action::Grant {environment,revision,provider}=>(environment,revision,provider,ProviderAuthorityCommand::Grant),
                    Action::Revoke {environment,revision,provider}=>(environment,revision,provider,ProviderAuthorityCommand::Revoke),
                    Action::Enable {environment,revision,provider}=>(environment,revision,provider,ProviderAuthorityCommand::Enable),
                    Action::Configure {..}=>unreachable!(),
                };
                current.session.provider_authority(environment,revision.parse().map_err(|_|"Invalid environment revision.".to_string())?,provider,command).await.map_err(|e|e.to_string())?;
            }
        }
        // Acknowledgement is separate from refresh: a failed refresh cannot imply mutation failed.
        Ok(json!({"applied":true,"workspace":current.name.as_str(),"generation":current.generation}))
    }).await;
    respond(result)
}
fn respond(result: Result<Result<Value, String>, tokio::task::JoinError>) -> Response {
    match result {
        Ok(Ok(value)) => {
            let bytes = value.to_string();
            if bytes.len() > 1024 * 1024 {
                return (
                    StatusCode::PAYLOAD_TOO_LARGE,
                    "Authentication metadata exceeds its limit.",
                )
                    .into_response();
            }
            (
                [
                    (header::CONTENT_TYPE, "application/json"),
                    (header::CACHE_CONTROL, "no-store"),
                ],
                bytes,
            )
                .into_response()
        }
        Ok(Err(message)) => (StatusCode::CONFLICT, message).into_response(),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "Authentication operation could not be confirmed; refresh before retrying.",
        )
            .into_response(),
    }
}
async fn report(current: &crate::CurrentSession) -> Result<Value, String> {
    let state = current
        .session
        .environment_authentication()
        .await
        .map_err(|e| e.to_string())?;
    let specs = current
        .session
        .imported_specs()
        .await
        .map_err(|e| e.to_string())?;
    let revisions = current
        .session
        .environment_revisions()
        .await
        .map_err(|e| e.to_string())?;
    if state
        .iter()
        .any(|s| revisions.get(s.environment.name()) != Some(&s.environment.revision()))
    {
        return Err("Environment changed; refresh /env.".into());
    }
    let workspace = current.name.as_str().to_owned();
    let generation = current.generation.clone();
    tokio::task::spawn_blocking(move || {
        let mut providers=Vec::new();
        for spec in specs {
            let Some(env)=state.iter().find(|s|s.environment.name()==spec.environment) else {continue;};
            let Some(import)=env.environment.imports().get(&spec.alias) else {continue;};
            let metadata=wes_adapters::api_library::authentication_metadata(spec.source.as_bytes(),&import.declaration().auth).map_err(|e|e.to_string())?;
            if metadata.as_array().is_none_or(|ops|ops.is_empty()) {continue;}
            let status=&env.providers[&spec.alias];
            providers.push(json!({"environment":spec.environment,"revision":env.environment.revision().to_string(),"provider":spec.alias,"operations":metadata,
                "credentials":import.credential_refs().iter().map(|(slot,reference)|json!({"slot":slot,"reference":reference,"present":status.present.contains(slot),"saved":status.saved.contains(slot)})).collect::<Vec<_>>(),
                "persistenceSupported":status.persistence_supported,"enabled":status.enabled,"grantSeconds":status.grant_seconds}));
        }
        Ok(json!({"workspace":workspace,"generation":generation,"providers":providers}))
    }).await.map_err(|_|"Authentication inspection failed.".to_string())?
}
async fn configure(
    current: &crate::CurrentSession,
    environment: String,
    revision: String,
    provider: String,
    auth: BTreeMap<String, Vec<String>>,
) -> Result<(), String> {
    let expected = revision
        .parse()
        .map_err(|_| "Invalid environment revision.")?;
    let state = current
        .session
        .environment_authentication()
        .await
        .map_err(|e| e.to_string())?;
    let env = state
        .into_iter()
        .find(|s| s.environment.name() == environment && s.environment.revision() == expected)
        .ok_or("Environment changed; refresh /env.")?;
    let import = env
        .environment
        .imports()
        .get(&provider)
        .cloned()
        .ok_or("Provider is no longer available.")?;
    let specs = current
        .session
        .imported_specs()
        .await
        .map_err(|e| e.to_string())?;
    let spec = specs
        .into_iter()
        .find(|s| s.environment == environment && s.alias == provider)
        .ok_or("Captured HTTP spec is unavailable.")?;
    let documents = current
        .session
        .environment_documents()
        .await
        .map_err(|e| e.to_string())?;
    let document = documents
        .into_iter()
        .find(|d| d.environments.contains(&environment))
        .ok_or("Environment document unavailable.")?;
    if current
        .session
        .environment_revisions()
        .await
        .map_err(|e| e.to_string())?
        .get(&environment)
        != Some(&expected)
    {
        return Err("Environment changed; refresh /env.".into());
    }
    let origin = document.origin;
    let source = tokio::task::spawn_blocking(move || {
        let required =
            wes_adapters::api_library::authentication_requirements(spec.source.as_bytes(), &auth)
                .map_err(|e| e.to_string())?;
        wes_adapters::environments::configure_authentication(
            &document.source,
            &environment,
            &provider,
            import.declaration(),
            &auth,
            &required,
        )
        .map_err(|e| e.to_string())
    })
    .await
    .map_err(|_| "Authentication planning failed.")??;
    let plan = current
        .session
        .plan_environment_document(source, origin)
        .await
        .map_err(|e| e.to_string())?;
    current
        .session
        .apply_environments(plan)
        .await
        .map_err(|e| e.to_string())?;
    Ok(())
}
