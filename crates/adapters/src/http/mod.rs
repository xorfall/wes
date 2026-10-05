//! HTTP transport owns requests, credentials and response interpretation, never graph state.
mod auth;
mod curl;
mod direct;
mod domain;
pub(crate) mod transport;
pub use domain::HttpFunction;
mod inspection;
pub use direct::direct;
pub(crate) use direct::direct_named;
pub(crate) mod explicit;
mod request;
pub(crate) mod response;
mod streaming;
#[cfg(test)]
mod transport_tests;
pub use auth::Auth;

use crate::codec::Limits;
use indexmap::IndexMap;
use reqwest::{Client, Url};
use std::{fmt, sync::Arc, time::Duration};
use thiserror::Error;
use uuid::Uuid;
use wes_core::{
    ErrorId, ErrorValue, ValidationIssue,
    capability::{Capability, ProviderDescription},
};
use wes_engine::{
    credentials::Credentials,
    driver::CancellationToken,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
};

#[derive(Clone, Copy, Debug)]
pub struct HttpConfig {
    pub connect_timeout: Duration,
    /// Covers network headers and body; joined bounded CPU work is not forcibly preempted.
    pub request_timeout: Duration,
    pub request: Limits,
    pub response: Limits,
    pub url_bytes: usize,
    /// Injected authentication names/values, not the HTTP library's complete wire header block.
    pub header_bytes: usize,
}
impl Default for HttpConfig {
    fn default() -> Self {
        Self {
            connect_timeout: Duration::from_millis(wes_budgets::get("http.connect.ms")),
            request_timeout: Duration::from_millis(wes_budgets::get("http.request.ms")),
            request: Limits {
                bytes: wes_budgets::get("http.request.bytes") as usize,
                ..Limits::default()
            },
            response: Limits {
                bytes: wes_budgets::get("http.response.bytes") as usize,
                ..Limits::default()
            },
            url_bytes: wes_budgets::get("http.url.bytes") as usize,
            header_bytes: wes_budgets::get("http.header.bytes") as usize,
        }
    }
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("invalid HTTP configuration: {0}")]
pub struct HttpConfigError(&'static str);

/// Capabilities and bindings are admitted together. Construction performs no provider request.
pub(crate) struct HttpProvider {
    base: Url,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    offerings: IndexMap<Vec<String>, (Capability, explicit::Operation)>,
}
impl HttpProvider {
    pub(crate) fn offer_explicit(
        &mut self,
        capability: Capability,
        operation: explicit::Operation,
    ) -> Result<(), HttpConfigError> {
        request::route_url(&self.base, &operation.route, self.config.url_bytes)?;
        if self.offerings.len() >= 1000 || self.offerings.contains_key(&capability.path) {
            return Err(HttpConfigError("duplicate or excessive operations"));
        }
        self.offerings
            .insert(capability.path.clone(), (capability, operation));
        Ok(())
    }
    pub fn new(
        base: &str,
        credentials: Arc<dyn Credentials>,
        config: HttpConfig,
    ) -> Result<Self, HttpConfigError> {
        if config.connect_timeout.is_zero()
            || config.request_timeout.is_zero()
            || config.connect_timeout > Duration::from_secs(86_400)
            || config.request_timeout > Duration::from_secs(86_400)
        {
            return Err(HttpConfigError(
                "timeouts must be positive and at most one day",
            ));
        }
        let base = request::base_url(base, config.url_bytes)?;
        Ok(Self {
            base,
            credentials,
            config,
            offerings: IndexMap::new(),
        })
    }
    pub fn build(
        self,
        name: impl Into<String>,
    ) -> Result<(ProviderDescription, HttpInvoker), HttpConfigError> {
        if self.offerings.is_empty() {
            return Err(HttpConfigError("a provider must offer a capability"));
        }
        let description = ProviderDescription::new(
            name,
            self.offerings.values().map(|(cap, _)| cap.clone()),
            self.offerings
                .values()
                .flat_map(|(_, operation)| operation.auth.names())
                .collect::<indexmap::IndexSet<_>>()
                .into_iter()
                .collect(),
        )
        .map_err(|_| HttpConfigError("invalid capability metadata"))?;
        let client = client_builder(self.config)
            .build()
            .map_err(|_| HttpConfigError("HTTP client initialization failed"))?;
        let invoker = HttpInvoker {
            inner: Arc::new(Inner {
                client,
                transport: transport::Transport::Internal,
                base: self.base,
                credentials: self.credentials,
                config: self.config,
                operations: self
                    .offerings
                    .into_iter()
                    .map(|(path, (_, binding))| (path, binding))
                    .collect(),
            }),
        };
        Ok((description, invoker))
    }
    /// Descriptor/UI integrations must surface these warnings when the provider is imported.
    pub fn hazards(&self) -> Vec<String> {
        self.offerings
            .values()
            .flat_map(|(_, operation)| operation.auth.hazards())
            .collect()
    }
}
fn client_builder(config: HttpConfig) -> reqwest::ClientBuilder {
    Client::builder()
        .connect_timeout(config.connect_timeout)
        .retry(reqwest::retry::never())
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("HTTP redirect limit exceeded");
            }
            if let Some(first) = attempt.previous().first()
                && (first.origin() != attempt.url().origin()
                    || !attempt.url().username().is_empty()
                    || attempt.url().password().is_some())
            {
                return attempt.error("HTTP redirect leaves the configured origin");
            }
            attempt.follow()
        }))
        .referer(false)
        .no_proxy()
}
#[derive(Clone)]
pub struct HttpInvoker {
    inner: Arc<Inner>,
}
impl HttpInvoker {
    pub(crate) fn with_transport(mut self, transport: transport::Transport) -> Self {
        Arc::get_mut(&mut self.inner)
            .expect("unshared provider construction")
            .transport = transport;
        self
    }
}
impl fmt::Debug for HttpInvoker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("HttpInvoker")
            .field("operations", &self.inner.operations.len())
            .finish_non_exhaustive()
    }
}
struct Inner {
    client: Client,
    transport: transport::Transport,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    base: Url,
    operations: IndexMap<Vec<String>, explicit::Operation>,
}
impl Invoker for HttpInvoker {
    fn supports_trace(&self, profile: &str) -> bool {
        profile == "http"
    }
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
        self.invoke_with_trace(call, cancellation, None)
    }
    fn invoke_observed(
        &self,
        call: Call,
        cancellation: CancellationToken,
        trace: wes_engine::trace::TraceSink,
    ) -> InvocationFuture {
        self.invoke_with_trace(call, cancellation, Some(trace))
    }
}
impl HttpInvoker {
    fn invoke_with_trace(
        &self,
        call: Call,
        cancellation: CancellationToken,
        trace: Option<wes_engine::trace::TraceSink>,
    ) -> InvocationFuture {
        let inner = self.inner.clone();
        timed(async move {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if call.capability.streaming {
                return Err(Failure::Request("finite invocation cannot open a stream").error());
            }
            // Joining bounded CPU work preserves the executor's physical-lease contract even on cancel.
            let prepare = inner.clone();
            let request = tokio::task::spawn_blocking(move || prepare.prepare(&call))
                .await
                .map_err(|_| Failure::Internal.error())?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let prepared = request.map_err(Failure::error)?;
            if let Some(trace) = &trace {
                inspection::request(trace, &prepared.request, &prepared.redactor);
            }
            let response = inner
                .send_observed(
                    prepared.request,
                    cancellation.clone(),
                    trace.as_ref(),
                    &prepared.redactor,
                )
                .await?;
            let limits = inner.config.response;
            let interpreted = tokio::task::spawn_blocking(move || {
                let expected =
                    prepared
                        .responses
                        .get(&response.status)
                        .map(|r| match &r.contract {
                            Some(c) => response::Expectation::Contract(c),
                            None => response::Expectation::Empty,
                        });
                response::envelope(response, expected, limits)
            })
            .await
            .map_err(|_| Failure::Internal.error())?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            Ok(interpreted)
        })
    }
}
impl Inner {
    async fn read_body(&self, response: reqwest::Response) -> Result<(u16, Vec<u8>), Failure> {
        read_body(response, self.config.response.bytes, None).await
    }
    async fn send_observed(
        &self,
        request: reqwest::Request,
        cancellation: CancellationToken,
        trace: Option<&wes_engine::trace::TraceSink>,
        redactor: &auth::Redactor,
    ) -> Result<transport::Response, InvocationError> {
        let response = self
            .transport
            .send(
                &self.client,
                request,
                self.config,
                cancellation,
                trace,
                redactor,
            )
            .await?;
        if let Some(trace) = trace {
            trace.emit("http.body", inspection::body(&response.body, redactor));
        }
        Ok(response)
    }
}

async fn read_body(
    mut response: reqwest::Response,
    limit: usize,
    trace: Option<&wes_engine::trace::TraceSink>,
) -> Result<(u16, Vec<u8>), Failure> {
    let status = response.status().as_u16();
    if response
        .content_length()
        .is_some_and(|length| length > limit as u64)
    {
        return Err(Failure::Size);
    }
    let mut body = Vec::new();
    let mut last = std::time::Instant::now();
    let mut updates = 0;
    while let Some(chunk) = response.chunk().await.map_err(Failure::transport)? {
        if chunk.len() > limit.saturating_sub(body.len()) {
            return Err(Failure::Size);
        }
        body.extend_from_slice(&chunk);
        if updates < 48 && (updates == 0 || last.elapsed() >= Duration::from_millis(100)) {
            if let Some(trace) = trace {
                trace.emit(
                    "http.progress",
                    wes_engine::trace::record(
                        "HttpProgress",
                        [(
                            "receivedBytes".into(),
                            wes_engine::trace::integer(body.len() as i64),
                        )],
                        wes_core::Provenance::default(),
                    ),
                );
            }
            last = std::time::Instant::now();
            updates += 1;
        }
    }
    Ok((status, body))
}

enum Failure {
    Request(&'static str),
    RequestIssues(Vec<ValidationIssue>),
    Credential(String),
    CredentialLookup,
    CredentialAccess(String),
    CredentialInvalid,
    Transport,
    Timeout,
    Redirect,
    Size,
    Response,
    Status { status: u16, excerpt: String },
    Internal,
}
impl Failure {
    fn transport(error: reqwest::Error) -> Self {
        if error.is_redirect() {
            Self::Redirect
        } else if error.is_timeout() {
            Self::Timeout
        } else {
            Self::Transport
        }
    }
    fn error(self) -> InvocationError {
        let mut issues = vec![];
        let (code, message, transient) = match self {
            Self::Request(message) => ("HTTP001", message.to_owned(), false),
            Self::RequestIssues(details) => {
                issues = explicit::diagnostics::bounded(details);
                let message = issues
                    .first()
                    .map(|i| format!("{}: {}", i.path, i.message))
                    .unwrap_or_else(|| "HTTP request validation failed".into());
                ("HTTP001", message, false)
            }
            Self::Credential(name) => (
                "HTTP002",
                format!("HTTP requires the missing credential '{name}'"),
                false,
            ),
            Self::CredentialLookup => ("HTTP002", "HTTP credential lookup failed".into(), false),
            Self::CredentialAccess(provider) => (
                "HTTP002",
                wes_engine::credentials::CredentialError::AccessDenied(provider).to_string(),
                false,
            ),
            Self::CredentialInvalid => (
                "HTTP002",
                "HTTP credential material is invalid; check its size and authentication format without placing it in command source".into(),
                false,
            ),
            Self::Transport => (
                "HTTP003",
                "HTTP transport failed; the remote outcome is not established".into(),
                true,
            ),
            Self::Timeout => (
                "HTTP004",
                "HTTP request exceeded its local time budget".into(),
                true,
            ),
            Self::Redirect => (
                "HTTP005",
                "HTTP redirect was rejected or exceeded its limit".into(),
                false,
            ),
            Self::Size => (
                "HTTP006",
                "HTTP response exceeds its byte budget".into(),
                false,
            ),
            Self::Response => (
                "HTTP007",
                "HTTP response is invalid or does not satisfy its declared result type".into(),
                false,
            ),
            Self::Status { status, excerpt } => {
                issues.push(ValidationIssue {
                    path: String::new(),
                    code: format!("HTTP_STATUS_{status}"),
                    message: format!("HTTP status {status}"),
                });
                (
                    "HTTP008",
                    format!("HTTP answered {status}: {excerpt}"),
                    status == 429 || (500..600).contains(&status),
                )
            }
            Self::Internal => ("HTTP009", "HTTP boundary worker failed".into(), false),
        };
        if transient {
            issues.push(ValidationIssue {
                path: String::new(),
                code: "HTTP_TRANSIENT".into(),
                message: "A later request may succeed; no retry was performed".into(),
            });
        }
        InvocationError::Failed(
            ErrorValue::new(
                ErrorId::new(Uuid::new_v4().to_string()).expect("UUID"),
                code,
                message,
                issues,
                None,
            )
            .expect("HTTP error fields"),
        )
    }
}

/// Complete finite HTTP attempt, including preparation, timeout/cancellation and decoding.
fn timed(
    work: impl std::future::Future<Output = Result<wes_core::Value, InvocationError>> + Send + 'static,
) -> InvocationFuture {
    Box::pin(async move {
        let span = tracing::info_span!(target: "wes.telemetry", "operation", operation="http", outcome=tracing::field::Empty);
        let result = work.await;
        span.record(
            "outcome",
            match &result {
                Ok(_) => "ok",
                Err(InvocationError::Cancelled) => "cancelled",
                Err(InvocationError::Failed(e)) if e.code() == "HTTP004" => "timeout",
                Err(_) => "error",
            },
        );
        result
    })
}

#[cfg(test)]
mod tests;
