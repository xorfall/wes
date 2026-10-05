//! Ad-hoc HTTP is an ordinary, unsafe finite provider; environment resolution still controls access.
use super::{Failure, HttpConfig, HttpConfigError, auth::Redactor, client_builder, inspection};
use indexmap::IndexMap;
use wes_core::{
    Data, Primitive, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
    trace::TraceSink,
};

pub fn direct(config: HttpConfig) -> Result<(ProviderDescription, DirectHttp), HttpConfigError> {
    direct_named("http", config)
}
pub(crate) fn direct_named(
    name: &str,
    config: HttpConfig,
) -> Result<(ProviderDescription, DirectHttp), HttpConfigError> {
    if config.connect_timeout.is_zero()
        || config.request_timeout.is_zero()
        || config.connect_timeout > std::time::Duration::from_secs(86_400)
        || config.request_timeout > std::time::Duration::from_secs(86_400)
    {
        return Err(HttpConfigError(
            "timeouts must be positive and at most one day",
        ));
    }
    let mut cap = Capability::new(["request"], super::response::shape(), Safety::Unsafe);
    cap.summary = "Sends one HTTP(S) request; every HTTP status is returned as data".into();
    cap.parameters = vec![
        Parameter::new("method", Shape::Primitive(Primitive::Text), false),
        Parameter::new("url", Shape::Primitive(Primitive::Text), true),
        Parameter::new("headers", Shape::Unknown, false),
        Parameter::new("body", Shape::Unknown, false),
    ];
    let description = ProviderDescription::new(name, [cap], vec![])
        .map_err(|_| HttpConfigError("invalid direct HTTP metadata"))?;
    let client = client_builder(config)
        .build()
        .map_err(|_| HttpConfigError("HTTP client initialization failed"))?;
    Ok((
        description,
        DirectHttp {
            client,
            config,
            transport: super::transport::Transport::Internal,
        },
    ))
}
#[derive(Clone)]
pub struct DirectHttp {
    transport: super::transport::Transport,
    client: reqwest::Client,
    config: HttpConfig,
}
impl Invoker for DirectHttp {
    fn supports_trace(&self, profile: &str) -> bool {
        profile == "http"
    }
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
        self.run(call, cancellation, None)
    }
    fn invoke_observed(
        &self,
        call: Call,
        cancellation: CancellationToken,
        trace: TraceSink,
    ) -> InvocationFuture {
        self.run(call, cancellation, Some(trace))
    }
}
impl DirectHttp {
    pub(crate) fn with_transport(mut self, transport: super::transport::Transport) -> Self {
        self.transport = transport;
        self
    }
    fn run(
        &self,
        call: Call,
        cancellation: CancellationToken,
        trace: Option<TraceSink>,
    ) -> InvocationFuture {
        let this = self.clone();
        super::timed(async move {
            let config = this.config;
            let (request, redactor) =
                tokio::task::spawn_blocking(move || prepare(call.arguments, config))
                    .await
                    .map_err(|_| Failure::Internal.error())?
                    .map_err(Failure::error)?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if let Some(trace) = &trace {
                inspection::request(trace, &request, &redactor);
            }
            let response = this
                .transport
                .send(
                    &this.client,
                    request,
                    config,
                    cancellation.clone(),
                    trace.as_ref(),
                    &redactor,
                )
                .await?;
            if let Some(trace) = &trace {
                trace.emit("http.body", inspection::body(&response.body, &redactor));
            }
            let value = tokio::task::spawn_blocking(move || {
                super::response::envelope(response, None, config.response)
            })
            .await
            .map_err(|_| Failure::Internal.error())?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            Ok(value)
        })
    }
}
fn prepare(
    mut args: IndexMap<String, Value>,
    config: HttpConfig,
) -> Result<(reqwest::Request, Redactor), Failure> {
    crate::codec::check_arguments(&args, config.request)
        .map_err(|_| Failure::Request("HTTP arguments exceed their encoding budget"))?;
    let url = args
        .shift_remove("url")
        .ok_or(Failure::Request("url is required"))?;
    let Data::Text(url) = url.data() else {
        return Err(Failure::Request("url must be Text"));
    };
    if url.len() > config.url_bytes || url.chars().any(char::is_control) || url.contains('\\') {
        return Err(Failure::Request("invalid HTTP URL"));
    }
    let url = reqwest::Url::parse(url).map_err(|_| Failure::Request("invalid HTTP URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(Failure::Request(
            "HTTP(S) URL must have no userinfo or fragment",
        ));
    }
    let method = match args.shift_remove("method") {
        None => reqwest::Method::GET,
        Some(value) => {
            let Data::Text(method) = value.data() else {
                return Err(Failure::Request("method must be Text"));
            };
            reqwest::Method::from_bytes(method.as_bytes())
                .map_err(|_| Failure::Request("invalid HTTP method"))?
        }
    };
    if method == reqwest::Method::CONNECT {
        return Err(Failure::Request("HTTP tunneling is unsupported"));
    }
    let mut redactor = Redactor::empty();
    for (_, value) in url.query_pairs() {
        if !value.is_empty() {
            redactor.remember(&value);
        }
    }
    let mut request = reqwest::Request::new(method, url);
    if let Some(headers) = args.shift_remove("headers") {
        let Data::Record(fields) = headers.data() else {
            return Err(Failure::Request("headers must be a record of Text values"));
        };
        let mut bytes: usize = 0;
        for (name, value) in fields {
            let Data::Text(value) = value else {
                return Err(Failure::Request("header values must be Text"));
            };
            bytes = bytes.saturating_add(name.len()).saturating_add(value.len());
            if bytes > config.header_bytes || fields.len() > 64 {
                return Err(Failure::Request("headers exceed their budget"));
            }
            let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| Failure::Request("invalid header name"))?;
            if matches!(
                name.as_str(),
                "host" | "content-length" | "transfer-encoding" | "connection" | "upgrade"
            ) {
                return Err(Failure::Request("transport-owned header is unsupported"));
            }
            if !matches!(name.as_str(), "accept" | "content-type" | "cache-control")
                && !value.is_empty()
            {
                redactor.remember(value);
            }
            let value = reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| Failure::Request("invalid header value"))?;
            request.headers_mut().insert(name, value);
        }
    }
    if let Some(body) = args.shift_remove("body") {
        let bytes = match body.data() {
            Data::Text(text) => text.as_bytes().to_vec(),
            Data::Bytes(bytes) => bytes.to_vec(),
            _ => return Err(Failure::Request("body must be Text or Bytes")),
        };
        if bytes.len() > config.request.bytes {
            return Err(Failure::Request("request body exceeds its budget"));
        }
        *request.body_mut() = Some(bytes.into());
    }
    Ok((request, redactor))
}
