//! Explicit workspace routing. One engine publisher is shared by every client of a live workspace.
use super::*;
use axum::extract::FromRequestParts;
use std::collections::BTreeMap;
use tokio::sync::Mutex;

#[derive(Clone)]
struct Workspace {
    application: ApplicationHandle,
    projections: watch::Receiver<Option<Arc<projection::Projection>>>,
    live: broadcast::Sender<Arc<conversations::Frame>>,
    storage: watch::Sender<Option<Arc<services::StorageReport>>>,
    clients: Arc<Semaphore>,
}
pub(super) struct Registry {
    application: ApplicationHandle,
    credentials: Arc<MemoryCredentials>,
    credentials_changed: watch::Receiver<()>,
    stopped: CancellationToken,
    diagnostics: Option<Arc<crate::telemetry::Controller>>,
    entries: Mutex<BTreeMap<String, Workspace>>,
    publishers: tokio_util::task::TaskTracker,
}
impl Registry {
    pub(super) fn new(
        application: ApplicationHandle,
        credentials: Arc<MemoryCredentials>,
        credentials_changed: watch::Receiver<()>,
        stopped: CancellationToken,
        diagnostics: Option<Arc<crate::telemetry::Controller>>,
    ) -> Self {
        Self {
            application,
            credentials,
            credentials_changed,
            stopped,
            diagnostics,
            entries: Mutex::new(BTreeMap::new()),
            publishers: tokio_util::task::TaskTracker::new(),
        }
    }
    async fn workspace(&self, name: &str) -> Result<Workspace, crate::ApplicationError> {
        let application = self.application.bound(name)?;
        let mut entries = self.entries.lock().await;
        if let Some(workspace) = entries.get(name) {
            if workspace.application.current().is_ok() {
                return Ok(workspace.clone());
            }
        }
        let (output, projections) = watch::channel(None);
        let (live, _) = broadcast::channel(32);
        let (storage, inventory) = watch::channel(None);
        let workspace = Workspace {
            application: application.clone(),
            projections,
            live: live.clone(),
            storage,
            clients: Arc::new(Semaphore::new(8)),
        };
        let credentials = self.credentials.clone();
        let changes = self.credentials_changed.clone();
        let stopped = self.stopped.clone();
        let diagnostics = self.diagnostics.clone();
        self.publishers.spawn(async move {
            // A failed workspace projection must not stop unrelated live workspaces.
            let _ = publish(
                application,
                credentials,
                changes,
                inventory,
                output,
                live,
                stopped,
                diagnostics,
            )
            .await;
        });
        entries.insert(name.to_owned(), workspace.clone());
        Ok(workspace)
    }
    pub(super) async fn run(
        &self,
        output: watch::Sender<Option<Arc<projection::Projection>>>,
        live: broadcast::Sender<Arc<conversations::Frame>>,
    ) -> io::Result<()> {
        let mut sessions = self.application.subscribe_sessions();
        let mut selected = self.application.subscribe();
        let mut names = self.application.subscribe_workspace_names();
        let result = async {
            loop {
                if self.stopped.is_cancelled() { return Ok(()); }
                let all = sessions.borrow_and_update().clone();
                for current in all { self.workspace(current.name.as_str()).await.map_err(io::Error::other)?; }
                let selected_value = selected.borrow_and_update().clone();
                let Some(current) = selected_value else {
                    output.send_replace(Some(Arc::new(projection::Projection::closed(None,&names.borrow_and_update(),self.application.cleanup_warning()))));
                    tokio::select! {_=self.stopped.cancelled()=>return Ok(()),change=selected.changed()=>{change.map_err(io::Error::other)?;},change=sessions.changed()=>{change.map_err(io::Error::other)?;},change=names.changed()=>{change.map_err(io::Error::other)?;}}
                    continue;
                };
                let workspace = self.workspace(current.name.as_str()).await.map_err(io::Error::other)?;
                let mut projections = workspace.projections;
                let mut frames = workspace.live.subscribe();
                loop {
                    output.send_replace(projections.borrow_and_update().clone());
                    tokio::select! {
                        biased;
                        _ = self.stopped.cancelled() => return Ok(()),
                        changed = selected.changed() => {
                            changed.map_err(io::Error::other)?;
                            let unchanged = selected.borrow_and_update().as_ref().is_some_and(|next| next.generation == current.generation);
                            if !unchanged {
                                if let Some(diagnostics) = &self.diagnostics { diagnostics.stop_capture(); }
                                break;
                            }
                        },
                        changed = sessions.changed() => {
                            changed.map_err(io::Error::other)?;
                            let all = sessions.borrow_and_update().clone();
                            for current in all { self.workspace(current.name.as_str()).await.map_err(io::Error::other)?; }
                        },
                        changed = projections.changed() => { changed.map_err(io::Error::other)?; },
                        frame = frames.recv() => match frame {
                            Ok(frame) => { let _ = live.send(frame); },
                            Err(broadcast::error::RecvError::Lagged(_)) => { let _ = live.send(Arc::new(conversations::Frame::gap(current.generation.clone()))); },
                            Err(broadcast::error::RecvError::Closed) => break,
                        },
                    }
                }
            }
        }.await;
        self.stopped.cancel();
        self.publishers.close();
        self.publishers.wait().await;
        result
    }
}

// Fetch header values are ASCII. Decode encodeURIComponent names without treating '+' as a space.
fn decode_name(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    String::from_utf8(bytes).ok()
}
fn binding(
    headers: &axum::http::HeaderMap,
    uri: &axum::http::Uri,
) -> Result<Option<String>, Response> {
    let invalid = || (StatusCode::BAD_REQUEST, "invalid workspace binding").into_response();
    let mut header_values = headers.get_all("x-wes-workspace").iter();
    let mut selected = header_values
        .next()
        .map(|value| {
            value
                .to_str()
                .ok()
                .and_then(decode_name)
                .ok_or_else(invalid)
        })
        .transpose()?;
    if header_values.next().is_some() {
        return Err(invalid());
    }
    if matches!(uri.path(), "/events" | "/events/socket") {
        let mut query_workspace = None;
        for pair in uri
            .query()
            .unwrap_or("")
            .split('&')
            .filter(|pair| !pair.is_empty())
        {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let key = decode_name(&key.replace('+', " ")).ok_or_else(invalid)?;
            if key == "workspace" {
                let value = decode_name(&value.replace('+', " ")).ok_or_else(invalid)?;
                if query_workspace.replace(value).is_some() {
                    return Err(invalid());
                }
            }
        }
        if let Some(query) = query_workspace {
            if selected.as_ref().is_some_and(|header| header != &query) {
                return Err(invalid());
            }
            selected = Some(query);
        }
    }
    if let Some(name) = &selected {
        wes_engine::workspace::WorkspaceName::new(name.clone()).map_err(|_| invalid())?;
    }
    Ok(selected)
}
pub(super) fn validate_binding(shared: &Shared, request: &Request) -> Result<(), Response> {
    if let Some(name) = binding(request.headers(), request.uri())? {
        shared
            .application
            .bound(&name)
            .map_err(|_| (StatusCode::NOT_FOUND, "workspace is not open").into_response())?;
    }
    Ok(())
}
pub(super) struct Scoped(pub(super) Shared);
impl FromRequestParts<Shared> for Scoped {
    type Rejection = Response;
    async fn from_request_parts(
        parts: &mut axum::http::request::Parts,
        shared: &Shared,
    ) -> Result<Self, Self::Rejection> {
        let explicit = binding(&parts.headers, &parts.uri)?;
        if explicit.is_none() && matches!(parts.uri.path(), "/events" | "/events/socket") {
            return Ok(Self(shared.clone()));
        }
        let name = match explicit {
            Some(name) => name,
            None => shared
                .application
                .current()
                .map_err(|_| StatusCode::GONE.into_response())?
                .name
                .as_str()
                .to_owned(),
        };
        let workspace = shared
            .workspaces
            .workspace(&name)
            .await
            .map_err(|_| (StatusCode::NOT_FOUND, "workspace is not open").into_response())?;
        let mut scoped = shared.clone();
        scoped.application = workspace.application;
        scoped.projections = workspace.projections;
        scoped.live = workspace.live;
        scoped.storage_reports = workspace.storage;
        scoped.clients = workspace.clients;
        Ok(Self(scoped))
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Open {
    #[serde(default)]
    identity: Option<String>,
    name: String,
    #[serde(default)]
    create: bool,
}
pub(super) async fn open(State(shared): State<Shared>, request: Request) -> Response {
    if request
        .headers()
        .get(header::CONTENT_TYPE)
        .is_none_or(|value| value != "application/json")
    {
        return StatusCode::UNSUPPORTED_MEDIA_TYPE.into_response();
    }
    let Ok(Ok(bytes)) =
        tokio::time::timeout(Duration::from_secs(10), to_bytes(request.into_body(), 4096)).await
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(request) = serde_json::from_slice::<Open>(&bytes) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Ok(name) = wes_engine::workspace::WorkspaceName::new(request.name) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    match shared
        .application
        .open_workspace_identity(name, request.create, request.identity)
        .await
    {
        Ok(current) => services::management_json(
            StatusCode::OK,
            &serde_json::json!({"workspace":current.name.as_str(),"generation":current.generation,"identity":current.identity}),
        ),
        Err(error) => (StatusCode::CONFLICT, error.to_string()).into_response(),
    }
}
