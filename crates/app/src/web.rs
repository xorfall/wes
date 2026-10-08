//! Loopback-only browser transport. HTTP syntax belongs to Axum/Hyper, session semantics to engine.
mod api_library;
mod budgets;
mod conversations;
mod credential_vault;
mod data_home;
mod environment_authentication;
mod workspace_specs;
pub(crate) use data_home::recovery as listen_data_home_recovery;
mod edit_files;
mod event_socket;
mod fonts;
mod history;
pub mod language;
mod live_view;
mod preferences;
mod presentations;
mod projection;
mod read_admission;
mod services;
mod site;
mod telemetry;
mod terminals;
mod traces;
mod value_errors;
mod view_inputs;
mod view_instances;
mod view_interaction;
mod view_mounts;
mod view_packages;
mod workspace_deletion;
mod workspaces;
use crate::ApplicationHandle;
use axum::{
    Router,
    body::to_bytes,
    extract::{Path, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{
        IntoResponse, Response, Sse,
        sse::{Event, KeepAlive},
    },
    routing::{get, post},
};
use futures_util::stream;
use hyper_util::{
    rt::{TokioIo, TokioTimer},
    service::TowerToHyperService,
};
pub use preferences::DesktopPreferences;
use serde::Deserialize;
pub use services::Services;
use std::{
    convert::Infallible,
    io,
    net::{Ipv4Addr, SocketAddr},
    path::PathBuf,
    sync::Arc,
    time::Duration,
};
use tokio::{
    net::TcpListener,
    sync::{Semaphore, broadcast, watch},
    task::{JoinHandle, JoinSet},
};
use wes_adapters::{
    codec::{Limits, encode_display_value},
    credentials::MemoryCredentials,
};
use wes_engine::{
    credentials::SecretString,
    driver::CancellationToken,
    graph::NodeId,
    session::{RetentionPolicy, SessionError},
    source::SourceInput,
    storage::{StoreWorker, ValueHandle},
};
use workspaces::Scoped;

pub struct Config {
    pub port: u16,
    pub application: ApplicationHandle,
    pub values: StoreWorker,
    pub credentials: Arc<MemoryCredentials>,
    /// Optional built frontend. Loaded once through a confined directory, never resolved per URL.
    pub site: Option<PathBuf>,
    pub services: Services,
}
pub struct Server {
    encoders: tokio_util::task::TaskTracker,
    terminals: crate::terminal::Manager,
    address: SocketAddr,
    stopped: CancellationToken,
    task: JoinHandle<io::Result<()>>,
}
impl Server {
    pub fn has_pending_io(&self) -> bool {
        !self.encoders.is_empty()
    }
    pub fn has_active_terminals(&self) -> bool {
        self.terminals.has_active_sessions()
    }
    pub async fn stopped(&self) {
        self.stopped.cancelled().await;
    }
    pub fn address(&self) -> SocketAddr {
        self.address
    }
    /// Closes listener and joins projection/connection work. Does not shut down the application.
    pub async fn shutdown(self) -> io::Result<()> {
        self.stopped.cancel();
        self.task.await.map_err(io::Error::other)?
    }
}
#[derive(Clone)]
struct Shared {
    application: ApplicationHandle,
    workspaces: Arc<workspaces::Registry>,
    values: StoreWorker,
    credentials: Arc<MemoryCredentials>,
    credential_updates: watch::Sender<()>,
    projections: watch::Receiver<Option<Arc<projection::Projection>>>,
    live: broadcast::Sender<Arc<conversations::Frame>>,
    site: Arc<site::Site>,
    port: u16,
    clients: Arc<Semaphore>,
    reads: Arc<Semaphore>,
    queries: Arc<Semaphore>,
    stopped: CancellationToken,
    services: Arc<Services>,
    storage_reports: watch::Sender<Option<Arc<services::StorageReport>>>,
    encoders: tokio_util::task::TaskTracker,
    terminals: crate::terminal::Manager,
    terminal_calls: Arc<Semaphore>,
    event_sockets: Arc<Semaphore>,
    event_streams: tokio_util::task::TaskTracker,
}
pub async fn listen(mut config: Config) -> io::Result<Server> {
    if config.services.telemetry.is_none() {
        config.services.telemetry = crate::telemetry::global();
    }
    let diagnostics = config.services.telemetry.clone();
    let site = tokio::task::spawn_blocking(move || site::Site::load(config.site))
        .await
        .map_err(io::Error::other)??;
    let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, config.port)).await?;
    let address = listener.local_addr()?;
    let stopped = CancellationToken::new();
    let (publication, projections) = watch::channel(None);
    let (live, _) = broadcast::channel(32);
    let (credential_updates, changes) = watch::channel(());
    let (storage_reports, _) = watch::channel(None);
    let workspaces = Arc::new(workspaces::Registry::new(
        config.application.clone(),
        config.credentials.clone(),
        changes,
        stopped.clone(),
        diagnostics,
    ));
    let encoders = tokio_util::task::TaskTracker::new();
    let event_streams = tokio_util::task::TaskTracker::new();
    let terminals = crate::terminal::Manager::default();
    config
        .application
        .attach_terminals(terminals.clone())
        .map_err(io::Error::other)?;
    let terminal_owner = terminals.clone();
    let server_terminals = terminals.clone();
    let shared = Shared {
        terminals,
        workspaces: workspaces.clone(),
        terminal_calls: Arc::new(Semaphore::new(
            wes_budgets::get("transport.terminals") as usize
        )),
        event_sockets: Arc::new(Semaphore::new(wes_budgets::get("transport.events") as usize)),
        event_streams: event_streams.clone(),
        queries: Arc::new(Semaphore::new(
            wes_budgets::get("transport.queries") as usize
        )),
        stopped: stopped.clone(),
        services: Arc::new(config.services),
        storage_reports,
        encoders: encoders.clone(),
        application: config.application.clone(),
        values: config.values,
        credentials: config.credentials.clone(),
        credential_updates,
        projections,
        live: live.clone(),
        site: Arc::new(site),
        port: address.port(),
        clients: Arc::new(Semaphore::new(
            wes_budgets::get("transport.clients") as usize
        )),
        reads: Arc::new(Semaphore::new(wes_budgets::get("transport.reads") as usize)),
    };
    let router = Router::new()
        .route("/operating-budgets", get(budgets::read).put(budgets::write))
        .route(
            "/credential-vault",
            get(credential_vault::read).post(credential_vault::change),
        )
        .route(
            "/diagnostics",
            get(telemetry::status).post(telemetry::control),
        )
        .route("/diagnostics/client", post(telemetry::client))
        .route("/data-home", get(data_home::read).post(data_home::switch))
        .route("/terminals", post(terminals::action))
        .route("/terminal-bridge", post(terminals::bridge))
        .route("/traces/{node}", get(traces::read))
        .route("/workspace-specs", get(workspace_specs::read))
        .route("/events", get(events))
        .route("/events/socket", get(event_socket::open))
        .route("/workspaces", post(workspaces::open))
        .route("/workspace-deletion", post(workspace_deletion::change))
        .route("/submit", post(submit))
        .route("/sandbox", get(read_sandbox))
        .route("/values/{handle}", get(value))
        .route("/live-view/{node}", get(live_view::read))
        .route("/view-mounts/{node}/{instance}", post(view_mounts::change))
        .route("/view-inputs/{node}/{instance}", get(view_inputs::read))
        .route(
            "/view-interaction/{node}/{instance}",
            get(view_interaction::state).put(view_interaction::state),
        )
        .route(
            "/view-instances/{node}/{instance}",
            get(view_instances::read),
        )
        .route("/complete", get(services::complete))
        .route("/language/calc", get(language::calc))
        .route("/language/yaml", get(language::yaml))
        .route("/language/views", get(language::views))
        .route("/view-packages/{digest}", get(view_packages::read))
        .route("/presentations", get(presentations::read))
        .route("/fonts", get(fonts::read))
        .route("/edit-files", post(edit_files::save))
        .route("/environment-documents", get(edit_files::environments))
        .route(
            "/environment-authentication",
            get(environment_authentication::read).post(environment_authentication::change),
        )
        .route(
            "/client-preferences",
            get(preferences::read).put(preferences::write),
        )
        .route("/history", get(history::read))
        .route(
            "/api-library",
            get(api_library::status).post(api_library::action),
        )
        .fallback(site_file)
        .layer(middleware::from_fn_with_state(shared.clone(), origin))
        .with_state(shared);
    let server_encoders = encoders.clone();
    let cancelled = stopped.clone();
    let task = tokio::spawn(async move {
        let publishing_stop = cancelled.clone();
        let publisher = tokio::spawn(async move {
            let result = workspaces.run(publication, live).await;
            publishing_stop.cancel();
            result
        });
        let connections = connections(listener, router, cancelled.clone()).await;
        cancelled.cancel();
        terminal_owner.shutdown().await;
        event_streams.close();
        event_streams.wait().await;
        encoders.close();
        encoders.wait().await;
        let published = publisher.await.map_err(io::Error::other)?;
        connections.and(published)
    });
    Ok(Server {
        encoders: server_encoders,
        terminals: server_terminals,
        address,
        stopped,
        task,
    })
}
async fn connections(
    listener: TcpListener,
    router: Router,
    stopped: CancellationToken,
) -> io::Result<()> {
    let mut tasks = JoinSet::new();
    let capacity = Arc::new(Semaphore::new(32));
    let mut failure = None;
    loop {
        tokio::select! {
            biased;
            _ = stopped.cancelled() => break,
            Some(result) = tasks.join_next(), if !tasks.is_empty() => { if let Err(error) = result { failure=Some(io::Error::other(error)); break; } },
            accepted = listener.accept() => {
                let (socket, _) = match accepted { Ok(value)=>value, Err(error)=>{failure=Some(error);break;} };
                let Ok(permit) = capacity.clone().try_acquire_owned() else { drop(socket); continue; };
                let service = TowerToHyperService::new(router.clone()); let stop = stopped.clone();
                tasks.spawn(async move {
                    let _permit = permit;
                    let mut builder = hyper::server::conn::http1::Builder::new();
                    builder.timer(TokioTimer::new()).header_read_timeout(Duration::from_secs(10)).max_buf_size(32*1024).max_headers(32);
                    let connection = builder.serve_connection(TokioIo::new(socket),service).with_upgrades();
                    tokio::pin!(connection);
                    tokio::select! { _ = &mut connection => {}, _ = stop.cancelled() => {
                        connection.as_mut().graceful_shutdown();
                        let _ = tokio::time::timeout(Duration::from_secs(2), &mut connection).await;
                        // Expired drain drops the owned socket/future; no detached connection task.
                    }}
                });
            }
        }
    }
    drop(listener);
    stopped.cancel();
    while let Some(result) = tasks.join_next().await {
        if let Err(error) = result {
            failure.get_or_insert_with(|| io::Error::other(error));
        }
    }
    failure.map_or(Ok(()), Err)
}
// Owns the existing publication inputs plus the process capture lifecycle.
#[allow(clippy::too_many_arguments)]
async fn publish(
    application: ApplicationHandle,
    credentials: Arc<MemoryCredentials>,
    mut credential_changes: watch::Receiver<()>,
    mut inventory_changes: watch::Receiver<Option<Arc<services::StorageReport>>>,
    output: watch::Sender<Option<Arc<projection::Projection>>>,
    live: broadcast::Sender<Arc<conversations::Frame>>,
    stopped: CancellationToken,
    diagnostics: Option<Arc<crate::telemetry::Controller>>,
) -> io::Result<()> {
    let mut first = true;
    let mut selected = application.subscribe();
    let mut names = application.subscribe_workspace_names();
    let mut capacity = application.subscribe_capacity();
    let mut last_name = None;
    loop {
        if stopped.is_cancelled() {
            return Ok(());
        }
        if !first && let Some(c) = &diagnostics {
            c.stop_capture();
        }
        first = false;
        let current = selected.borrow_and_update().clone();
        let Some(current) = current else {
            let mut closed = projection::Projection::closed(
                last_name.as_deref(),
                &names.borrow(),
                application.cleanup_warning(),
            );
            closed.capacity(*capacity.borrow_and_update());
            output.send_replace(Some(Arc::new(closed)));
            tokio::select! { _ = stopped.cancelled() => return Ok(()), changed=selected.changed()=> {if changed.is_err(){return Ok(())}}, changed=names.changed()=> {if changed.is_err(){return Ok(())}}, changed=capacity.changed()=> {if changed.is_err(){return Ok(())} tokio::time::sleep(Duration::from_millis(100)).await;} }
            continue;
        };
        last_name = Some(current.name.as_str().to_owned());
        let mut updates = current
            .session
            .subscribe_updates()
            .map_err(io::Error::other)?;
        let mut conversations = current
            .session
            .subscribe_conversations()
            .map_err(io::Error::other)?;
        let period = Duration::from_millis(100);
        let mut next_projection = tokio::time::Instant::now();
        'session: loop {
            // Observation is a latest-state consumer, not an event replay. Coalesce BEFORE
            // cloning/encoding the workspace, and never run catch-up frames after a delay.
            tokio::select! {
                biased;
                _ = stopped.cancelled() => return Ok(()),
                change = selected.changed() => { change.map_err(io::Error::other)?; break 'session; },
                _ = tokio::time::sleep_until(next_projection) => {},
            }
            updates = updates.resubscribe();
            next_projection = tokio::time::Instant::now() + period;
            // The registry owns one receiver. An unobserved background workspace needs no
            // repeated projection work; first subscription is serviced at the next cadence.
            if output.receiver_count() <= 1 && output.borrow().is_some() {
                continue;
            }
            names.borrow_and_update();
            let observation = current.session.observe().await;
            if !application
                .current()
                .is_ok_and(|now| now.generation == current.generation)
            {
                break;
            }
            let observation = match observation {
                Ok(value) => value,
                Err(_) => {
                    tokio::select! { _=stopped.cancelled()=>return Ok(()), changed=selected.changed()=> {if changed.is_err(){return Ok(())}} }
                    break;
                }
            };
            let generation = current.generation.clone();
            let workspace = current.name.as_str().to_owned();
            let credentials = credentials.clone();
            // CPU/encoding work is bounded and joined even if shutdown arrives during this call.
            let storage = inventory_changes.borrow_and_update().clone();
            let storage_warning = current.storage_warning.clone();
            let mut projection = match tokio::task::spawn_blocking(move || {
                let timing = wes_engine::diagnostics::Operation::start("projection");
                let mut projection = projection::build(
                    generation,
                    workspace,
                    observation,
                    &credentials,
                    storage.as_deref(),
                )?;
                if let Some(message) = storage_warning {
                    projection.storage_warning(message)?;
                }
                timing.finish("ok");
                Ok::<_, io::Error>(projection)
            })
            .await
            {
                Ok(Ok(projection)) => projection,
                // A bounded client view is not ownership of the engine. Clear stale displayed state
                // and keep requests/value reads alive so an explicit load can recover this client.
                _ => projection::Projection::unavailable(&current.generation),
            };
            if !application
                .current()
                .is_ok_and(|now| now.generation == current.generation)
            {
                break;
            }
            projection.capacity(*capacity.borrow_and_update());
            output.send_replace(Some(Arc::new(projection)));
            loop {
                tokio::select! {
                    biased;
                    _ = stopped.cancelled() => return Ok(()),
                    change = selected.changed() => { change.map_err(io::Error::other)?; break 'session; },
                    change = names.changed() => { change.map_err(io::Error::other)?; break; },
                    change = inventory_changes.changed() => { change.map_err(io::Error::other)?; break; },
                    change = credential_changes.changed() => { change.map_err(io::Error::other)?; break; },
                    _ = updates.recv() => break,
                    change = capacity.changed() => {
                        change.map_err(io::Error::other)?;
                        // Slot churn never rebuilds the workspace or queues a replay. Publish only
                        // the newest counts at the same bounded cadence as other observations.
                        tokio::select! {
                            _ = stopped.cancelled() => return Ok(()),
                            change = selected.changed() => { change.map_err(io::Error::other)?; break 'session; },
                            _ = tokio::time::sleep(period) => {},
                        }
                        let snapshot = *capacity.borrow_and_update();
                        let previous = output.borrow().clone();
                        if let Some(previous) = previous {
                            let mut projection = (*previous).clone();
                            projection.capacity(snapshot);
                            output.send_replace(Some(Arc::new(projection)));
                        }
                    },
                    event = conversations.recv() => {
                        use wes_engine::driver::ConversationEvent;
                        let frame = match event {
                            Ok(event) => match event.as_ref() {
                                ConversationEvent::Started(_) | ConversationEvent::Ended(_) => break,
                                ConversationEvent::Output { run, batch } => {
                                    let generation = current.generation.clone();
                                    let run = run.clone();
                                    let batch = batch.clone();
                                    match tokio::task::spawn_blocking(move || conversations::Frame::output(generation, run, &batch)).await {
                                        Ok(Ok(frame)) => frame,
                                        _ => conversations::Frame::gap(current.generation.clone()),
                                    }
                                },
                            },
                            Err(broadcast::error::RecvError::Lagged(_)) => conversations::Frame::gap(current.generation.clone()),
                            Err(broadcast::error::RecvError::Closed) => break 'session,
                        };
                        if application.current().is_ok_and(|now| now.generation == current.generation) {
                            let _ = live.send(Arc::new(frame));
                        }
                    },
                }
            }
        }
    }
}
pub(super) fn origin_allowed(headers: &axum::http::HeaderMap, port: u16) -> bool {
    let host = headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let valid_host = host == format!("127.0.0.1:{}", port) || host == format!("localhost:{}", port);
    let same_origin = headers.get(header::ORIGIN).is_none_or(|origin| {
        origin
            .to_str()
            .is_ok_and(|origin| origin == format!("http://{host}"))
    });
    let cross_site = headers
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site");
    valid_host && same_origin && !cross_site
}
async fn origin(State(shared): State<Shared>, request: Request, next: Next) -> Response {
    if !origin_allowed(request.headers(), shared.port) {
        return (
            StatusCode::FORBIDDEN,
            "a same-origin loopback request is required",
        )
            .into_response();
    }
    if let Err(response) = workspaces::validate_binding(&shared, &request) {
        return response;
    }
    // Hold admission for every finite endpoint, including blocking API-library work.
    // SSE itself is read-only and remains open while the host checks/quiesces the engine.
    let _admission = if let Some(home) = &shared.services.data_home {
        use std::sync::atomic::Ordering;
        if home.retired.load(Ordering::Acquire) || !home.ready.load(Ordering::Acquire) {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "The data folder is changing; wait for the desktop to reopen.",
            )
                .into_response();
        }
        if matches!(
            request.uri().path(),
            "/data-home" | "/events" | "/events/socket"
        ) {
            None
        } else {
            match home.gate.clone().try_read_owned() {
                Ok(guard) => Some(guard),
                Err(_) => {
                    return (
                        StatusCode::CONFLICT,
                        "The data folder is changing. Retry after it opens.",
                    )
                        .into_response();
                }
            }
        }
    } else {
        None
    };
    let timing = (!request.uri().path().starts_with("/diagnostics")
        && !matches!(request.uri().path(), "/events" | "/events/socket"))
    .then(|| wes_engine::diagnostics::Operation::start("web"));
    let mut response = next.run(request).await;
    if let Some(timing) = timing {
        timing.finish(if response.status().is_server_error() {
            "error"
        } else if response.status().is_client_error() {
            "rejected"
        } else {
            "ok"
        });
    }
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, "no-store".parse().unwrap());
    response
        .headers_mut()
        .insert("x-content-type-options", "nosniff".parse().unwrap());
    response
}
async fn events(Scoped(shared): Scoped) -> Response {
    let Ok(permit) = shared.clients.clone().try_acquire_owned() else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            "event client limit reached",
        )
            .into_response();
    };
    let state = conversations::Client::new(shared.projections, shared.live.subscribe(), permit);
    let events = stream::unfold(state, |mut state| async move {
        let frame = state.next().await?;
        Some((
            Ok::<_, Infallible>(Event::default().data(frame.as_ref())),
            state,
        ))
    });
    Sse::new(events)
        .keep_alive(KeepAlive::new().interval(Duration::from_secs(10)))
        .into_response()
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DocumentInput {
    source: String,
}

#[derive(Deserialize)]
#[serde(tag = "request", rename_all = "lowercase", deny_unknown_fields)]
enum Command {
    #[serde(rename = "work-grant")]
    WorkGrant {
        actor: String,
        cells: Vec<String>,
    },
    #[serde(rename = "source-grant")]
    SourceGrant {
        actor: String,
        cells: Vec<String>,
    },
    EnvironmentSecret {
        reference: String,
        value: String,
    },
    EnvironmentForget {
        reference: String,
    },
    EnvironmentGrant {
        environment: String,
        revision: String,
        provider: String,
        seconds: u64,
    },
    EnvironmentRevoke {
        environment: String,
        revision: String,
        provider: String,
    },
    EnvironmentTransfer {
        origin: String,
        destination: String,
        revision: String,
        seconds: u64,
    },
    #[serde(rename = "cancel-work")]
    CancelWork {
        origin: String,
    },
    #[serde(rename = "work-history")]
    WorkHistory {
        client: String,
        cell: String,
        run: Option<String>,
    },
    #[serde(rename = "protect-run")]
    ProtectRun {
        client: String,
        cell: String,
        run: String,
    },
    Submit {
        #[serde(default)]
        document: Option<DocumentInput>,
        revision_of: Option<String>,
        #[serde(default)]
        from: Option<String>,
        #[serde(default)]
        repeat: Option<String>,
        #[serde(default)]
        acknowledge_effects: bool,
        #[serde(default)]
        console: bool,
        cell: String,
        text: String,
        client: String,
        #[serde(default)]
        environments: Option<EnvironmentContext>,
    },
    Cancel {
        node: String,
    },
    Secret {
        name: String,
        value: String,
    },
    Keep {
        handle: String,
    },
    #[serde(rename = "delete-work-preview")]
    DeleteWorkPreview {
        cell: String,
        client: String,
    },
    #[serde(rename = "delete-work")]
    DeleteWork {
        token: String,
        client: String,
        dependents: bool,
        protected: bool,
    },
    Release {
        token: String,
        client: String,
    },
    #[serde(rename = "release-preview")]
    ReleasePreview {
        handle: String,
        client: String,
    },
    Keeping {
        automatic: bool,
        under: u64,
    },
    Storage,
    Input {
        node: String,
        run: String,
        text: String,
    },
    Eof {
        node: String,
        run: String,
    },
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentContext {
    selected: Option<String>,
    revisions: std::collections::BTreeMap<String, String>,
}

fn sandbox_response(reply: &wes_engine::session::sandbox::Reply, generation: &str) -> Response {
    let value = wes_core::Value::new(
        wes_core::Shape::Unknown,
        reply.data.clone(),
        wes_core::Provenance::default(),
    )
    .expect("unknown shape");
    match wes_adapters::codec::encode_display_value(&value, wes_adapters::codec::Limits::default())
    {
        Ok(bytes) => {
            let value: serde_json::Value = match serde_json::from_slice(&bytes) {
                Ok(value) => value,
                Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
            };
            let mut response = services::management_json(
                StatusCode::OK,
                &serde_json::json!({"sandbox":{"name":reply.name,"generation":generation,"state":reply.state.as_str(),"reference":reply.view.as_ref().map(|v|&v.0),"inspect":reply.view.as_ref().map(|v|v.1),"value":value}}),
            );
            response.headers_mut().insert(
                header::CACHE_CONTROL,
                header::HeaderValue::from_static("no-store"),
            );
            response
        }
        Err(_) => (
            StatusCode::PAYLOAD_TOO_LARGE,
            "Sandbox observation exceeds its display budget.",
        )
            .into_response(),
    }
}
async fn read_sandbox(Scoped(shared): Scoped, request: Request) -> Response {
    let Ok(current) = shared.application.current() else {
        return StatusCode::GONE.into_response();
    };
    if request
        .headers()
        .get("x-wes-session")
        .and_then(|v| v.to_str().ok())
        != Some(current.generation.as_str())
    {
        return StatusCode::CONFLICT.into_response();
    }
    let query: std::collections::BTreeMap<_, _> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or("").as_bytes())
            .into_owned()
            .collect();
    let Some(reference) = query.get("reference").filter(|r| r.len() <= 512) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match current
        .session
        .read_sandbox(reference, query.get("inspect").is_some_and(|i| i == "true"))
        .await
    {
        Ok(reply) => sandbox_response(&reply, &current.generation),
        Err(error) => (StatusCode::CONFLICT, error.to_string()).into_response(),
    }
}
async fn submit(Scoped(shared): Scoped, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let json = parts
        .headers
        .get(header::CONTENT_TYPE)
        .and_then(|s| s.to_str().ok())
        .is_some_and(|s| {
            s.split(';')
                .next()
                .is_some_and(|s| s.trim().eq_ignore_ascii_case("application/json"))
        });
    if !json {
        return (StatusCode::UNSUPPORTED_MEDIA_TYPE, "commands are JSON").into_response();
    }
    let Ok(current) = shared.application.current() else {
        return (StatusCode::SERVICE_UNAVAILABLE, "application stopped").into_response();
    };
    if parts
        .headers
        .get("x-wes-session")
        .and_then(|s| s.to_str().ok())
        != Some(current.generation.as_str())
    {
        return (
            StatusCode::CONFLICT,
            "session changed; reconnect before sending a new request",
        )
            .into_response();
    }
    let bytes = match tokio::time::timeout(Duration::from_secs(10), to_bytes(body, 8 * 1024 * 1024))
        .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(_)) => {
            return (
                StatusCode::PAYLOAD_TOO_LARGE,
                "invalid or oversized request body",
            )
                .into_response();
        }
        Err(_) => return (StatusCode::REQUEST_TIMEOUT, "request body timed out").into_response(),
    };
    let command: Command = match serde_json::from_slice(&bytes) {
        Ok(c) => c,
        Err(_) => return (StatusCode::BAD_REQUEST, "invalid request").into_response(),
    };
    if current.session.check_retirement_access().await.is_err() {
        return services::management_error(crate::retention::ManagementError::Retiring);
    }
    // Bound once before body reading. A concurrent load never silently redirects this request.
    let result = match command {
        Command::WorkGrant { actor, cells } => current.session.grant_work(actor, cells).await,
        Command::SourceGrant { actor, cells } => current.session.grant_sources(actor, cells).await,
        Command::EnvironmentSecret { reference, value } => {
            current
                .session
                .environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Supply {
                    reference,
                    value: Arc::new(SecretString::from(value)),
                })
                .await
        }
        Command::EnvironmentForget { reference } => {
            current
                .session
                .environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Forget {
                    reference,
                })
                .await
        }
        Command::EnvironmentGrant {
            environment,
            revision,
            provider,
            seconds,
        } => {
            let Ok(revision) = revision.parse() else {
                return (StatusCode::BAD_REQUEST, "invalid revision").into_response();
            };
            current
                .session
                .environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Grant {
                    environment,
                    revision,
                    provider,
                    seconds,
                })
                .await
        }
        Command::EnvironmentRevoke {
            environment,
            revision,
            provider,
        } => {
            let Ok(revision) = revision.parse() else {
                return (StatusCode::BAD_REQUEST, "invalid revision").into_response();
            };
            current
                .session
                .environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Revoke {
                    environment,
                    revision,
                    provider,
                })
                .await
        }
        Command::EnvironmentTransfer {
            origin,
            destination,
            revision,
            seconds,
        } => {
            let Ok(revision) = revision.parse() else {
                return (StatusCode::BAD_REQUEST, "invalid revision").into_response();
            };
            current
                .session
                .environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Transfer {
                    origin,
                    destination,
                    revision,
                    seconds,
                })
                .await
        }
        Command::WorkHistory { client, cell, run } => {
            return match shared
                .application
                .work_history(current.generation, client, cell, run)
                .await
            {
                Ok(value) => services::management_json(StatusCode::OK, &value),
                Err(error) => services::management_error(error),
            };
        }
        Command::ProtectRun { client, cell, run } => {
            return match shared
                .application
                .protect_run(current.generation, client, cell, run)
                .await
            {
                Ok(value) => services::management_json(StatusCode::OK, &value),
                Err(error) => services::management_error(error),
            };
        }
        Command::CancelWork { origin } => current.session.cancel_work(origin).await,
        Command::Submit {
            document,
            revision_of,
            from,
            repeat,
            acknowledge_effects,
            console,
            cell,
            text,
            client,
            environments,
        } => {
            let text = if console {
                let observation = match current.session.observe().await {
                    Ok(value) => value,
                    Err(_) => return StatusCode::GONE.into_response(),
                };
                terminals::console_source(
                    text,
                    &observation,
                    environments.as_ref().and_then(|e| e.selected.as_ref()),
                )
            } else {
                text
            };
            match SourceInput::new(cell, text).and_then(|input| {
                if console && document.is_some() {
                    return Err(wes_engine::source::SourceError::Identity);
                }
                let input = input.with_document(document.map(|d| d.source))?;
                let input = match repeat {
                    Some(origin) if !console => input
                        .with_repeat(origin, acknowledge_effects)?
                        .with_repeat_from(
                            from.map(NodeId::new)
                                .transpose()
                                .map_err(|_| wes_engine::source::SourceError::Identity)?,
                        )?,
                    Some(_) => return Err(wes_engine::source::SourceError::Identity),
                    None if from.is_none() => input,
                    None => return Err(wes_engine::source::SourceError::Identity),
                };
                let input = match revision_of {
                    Some(origin) if !console => input.with_revision(origin)?,
                    Some(_) => return Err(wes_engine::source::SourceError::Identity),
                    None => input,
                };
                let input = input.with_client(client)?;
                match environments {
                    None => Ok(input),
                    Some(context) => {
                        input.with_environments(wes_core::environments::EnvironmentContext {
                            selected: context.selected,
                            revisions: context
                                .revisions
                                .into_iter()
                                .map(|(n, r)| {
                                    Ok((
                                        n,
                                        r.parse().map_err(|_| {
                                            wes_engine::source::SourceError::Identity
                                        })?,
                                    ))
                                })
                                .collect::<Result<_, wes_engine::source::SourceError>>()?,
                        })
                    }
                }
            }) {
                Ok(input) => match current.session.submit(input).await {
                    Ok(reply) => {
                        if let Some(sandbox) = &reply.sandbox {
                            return sandbox_response(sandbox, &current.generation);
                        }
                        Ok(())
                    }
                    Err(error) => Err(error),
                },
                Err(_) => return (StatusCode::BAD_REQUEST, "invalid source").into_response(),
            }
        }
        Command::Cancel { node } => match NodeId::new(node) {
            Ok(node) => current.session.cancel(node).await,
            Err(_) => return (StatusCode::BAD_REQUEST, "invalid node").into_response(),
        },
        Command::Secret { name, value } => {
            if shared
                .credentials
                .remember(name, SecretString::from(value))
                .is_err()
            {
                return (StatusCode::BAD_REQUEST, "credential refused").into_response();
            }
            shared.credential_updates.send_replace(());
            Ok(())
        }
        Command::Keep { handle } => match ValueHandle::new(&handle) {
            Ok(h) => current.session.keep(h).await.and_then(|r| {
                if r.problem.is_some() {
                    Err(SessionError::Recording)
                } else {
                    Ok(())
                }
            }),
            Err(_) => return (StatusCode::BAD_REQUEST, "invalid handle").into_response(),
        },
        Command::ReleasePreview { handle, client } => {
            let Ok(handle) = ValueHandle::new(&handle) else {
                return (StatusCode::BAD_REQUEST, "invalid handle").into_response();
            };
            return match shared
                .application
                .preview_release(current.generation, client, handle)
                .await
            {
                Ok(preview) => services::management_json(StatusCode::OK, &preview),
                Err(error) => services::management_error(error),
            };
        }
        Command::DeleteWorkPreview { cell, client } => {
            return match shared
                .application
                .preview_delete_work(current.generation, client, cell)
                .await
            {
                Ok(preview) => services::management_json(StatusCode::OK, &preview),
                Err(error) => services::management_error(error),
            };
        }
        Command::DeleteWork {
            token,
            client,
            dependents,
            protected,
        } => {
            return match shared
                .application
                .delete_work(current.generation, client, token, dependents, protected)
                .await
            {
                Ok(()) => {
                    services::management_json(StatusCode::OK, &serde_json::json!({"deleted": true}))
                }
                Err(error) => services::management_error(error),
            };
        }
        Command::Release { token, client } => {
            return match shared
                .application
                .confirm_release(current.generation, client, token)
                .await
            {
                Ok(()) => {
                    services::management_json(StatusCode::OK, &serde_json::json!({"released":true}))
                }
                Err(error) => services::management_error(error),
            };
        }
        Command::Keeping { automatic, under } => {
            current
                .session
                .set_retention_policy(RetentionPolicy { automatic, under })
                .await
        }
        Command::Storage => return services::storage(&shared, current).await,
        Command::Input { node, run, text } => {
            match (NodeId::new(node), wes_engine::runtime::RunId::new(run)) {
                (Ok(node), Ok(run)) => current.session.input(node, run, text.as_bytes()).await,
                _ => {
                    return (StatusCode::BAD_REQUEST, "invalid conversation identity")
                        .into_response();
                }
            }
        }
        Command::Eof { node, run } => {
            match (NodeId::new(node), wes_engine::runtime::RunId::new(run)) {
                (Ok(node), Ok(run)) => current.session.eof(node, run).await,
                _ => {
                    return (StatusCode::BAD_REQUEST, "invalid conversation identity")
                        .into_response();
                }
            }
        }
    };
    match result {
        Ok(()) => (
            StatusCode::ACCEPTED,
            [(header::CONTENT_TYPE, "application/json")],
            "{\"accepted\":true}",
        )
            .into_response(),
        Err(error) => {
            let not_started = matches!(&error, SessionError::AdmissionRefused(_));
            let status = match error {
                SessionError::Conflict
                | SessionError::HistoricalReplyUnavailable
                | SessionError::CheckpointBusy => StatusCode::CONFLICT,
                SessionError::Capacity => StatusCode::TOO_MANY_REQUESTS,
                SessionError::Stopped => StatusCode::SERVICE_UNAVAILABLE,
                SessionError::Driver(wes_engine::driver::DriverError::Conversation(error)) => {
                    match error {
                        wes_engine::conversations::ConversationError::Busy => {
                            StatusCode::TOO_MANY_REQUESTS
                        }
                        wes_engine::conversations::ConversationError::Capacity => {
                            StatusCode::PAYLOAD_TOO_LARGE
                        }
                        _ => StatusCode::CONFLICT,
                    }
                }
                _ => StatusCode::BAD_REQUEST,
            };
            let mut response = (status, error.to_string()).into_response();
            if not_started {
                response.headers_mut().insert(
                    "x-wes-submission-outcome",
                    header::HeaderValue::from_static("not-started"),
                );
            }
            response
        }
    }
}
async fn value(Scoped(shared): Scoped, Path(handle): Path<String>, request: Request) -> Response {
    let Ok(handle) = ValueHandle::new(&handle) else {
        return value_errors::Failure::InvalidHandle.response(None);
    };
    let Ok(permit) = read_admission::acquire(&shared.reads, &shared.stopped).await else {
        return value_errors::Failure::Busy.response(Some(&handle));
    };
    let Ok(current) = shared.application.current() else {
        return value_errors::Failure::SessionUnavailable.response(Some(&handle));
    };
    if request
        .headers()
        .get("X-Wes-Session")
        .is_some_and(|generation| generation.to_str().ok() != Some(current.generation.as_str()))
    {
        return StatusCode::CONFLICT.into_response();
    }
    if current.session.check_retirement_access().await.is_err() {
        return value_errors::Failure::Retiring.response(Some(&handle));
    }
    let value = match shared.values.read(handle.clone()).await {
        Ok(Some(value)) => value,
        Ok(None) => return value_errors::Failure::Missing.response(Some(&handle)),
        Err(error) => return value_errors::Failure::Storage(error).response(Some(&handle)),
    };
    if value.value.provenance().policy().is_private() {
        let Ok(observation) = current.session.observe().await else {
            return value_errors::Failure::SessionUnavailable.response(Some(&handle));
        };
        if observation
            .values
            .as_ref()
            .is_none_or(|v| !v.outputs.values().any(|p| p.handle() == Some(&handle)))
        {
            return value_errors::Failure::PrivateUnavailable.response(Some(&handle));
        }
    }
    let encoded = shared
        .encoders
        .spawn_blocking(move || {
            // Admission bounds storage reads and encoding, not client delivery. Keep ownership
            // in the tracked encoder even after disconnect, then release before returning bytes.
            // A slow/unread HTTP body must not prevent frames or shared selection from loading.
            let _permit = permit;
            encode_display_value(&value.value, Limits::default())
        })
        .await;
    match encoded {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, "application/json")], bytes).into_response(),
        _ => value_errors::Failure::Encoding.response(Some(&handle)),
    }
}
async fn site_file(State(shared): State<Shared>, request: Request) -> Response {
    if request.method() != axum::http::Method::GET {
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    shared.site.response(request.uri().path())
}
