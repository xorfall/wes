//! Explicit HTTP operations. No parameter is implicitly sent as a body.
use super::{auth::CompiledAuth, request::PreparedRequest, *};
use crate::codec::{decode_json_for_contract, encode_request_data};
use reqwest::{
    Method, Request,
    header::{CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue},
};
use std::collections::{BTreeMap, BTreeSet};
use wes_core::{Data, Provenance, Shape, Value, contracts::Contract};

pub(super) mod diagnostics;
#[cfg(test)]
mod tests;

#[derive(Clone)]
pub(crate) struct Argument {
    pub name: String,
    pub wire: String,
    pub location: String,
    pub encoding: String,
    pub required: bool,
    pub contract: Arc<Contract>,
}
#[derive(Clone)]
pub(crate) struct Response {
    pub contract: Option<Arc<Contract>>,
}
impl Response {
    pub fn shape(&self) -> Shape {
        self.contract
            .as_ref()
            .map(|c| c.shape())
            .unwrap_or_else(|| Shape::Option(Box::new(Shape::Unknown)))
    }
    pub(super) fn decode(&self, body: &[u8], limits: Limits) -> Result<Value, Failure> {
        let data = if let Some(contract) = &self.contract {
            let data =
                decode_json_for_contract(body, limits, contract).map_err(|_| Failure::Response)?;
            if !contract.issues(&data).is_empty() {
                return Err(Failure::Response);
            }
            data
        } else {
            if !body.is_empty() {
                return Err(Failure::Response);
            }
            Data::Option(None)
        };
        Value::new(self.shape(), data, Provenance::default()).map_err(|_| Failure::Response)
    }
}
pub(crate) struct Operation {
    pub method: String,
    pub route: String,
    pub arguments: Vec<Argument>,
    pub responses: Arc<BTreeMap<u16, Response>>,
    pub(super) auth: CompiledAuth,
}
impl Operation {
    pub fn new(
        method: String,
        route: String,
        arguments: Vec<Argument>,
        responses: BTreeMap<u16, Response>,
        auth: Option<Auth>,
    ) -> Result<Self, HttpConfigError> {
        let bad = || HttpConfigError("invalid explicit HTTP operation");
        if !matches!(
            method.as_str(),
            "GET" | "HEAD" | "POST" | "PUT" | "PATCH" | "DELETE" | "OPTIONS"
        ) {
            return Err(bad());
        }
        let auth = CompiledAuth::new(auth)?;
        let mut names = BTreeSet::new();
        let mut destinations = BTreeSet::new();
        let mut remaining = route.clone();
        let mut bodies = 0;
        for arg in &arguments {
            let scalar_shape = |shape: &Shape| {
                matches!(
                    shape,
                    Shape::Primitive(
                        wes_core::Primitive::Text
                            | wes_core::Primitive::Int
                            | wes_core::Primitive::Decimal
                            | wes_core::Primitive::Bool
                    )
                )
            };
            let shape = arg.contract.shape();
            match arg.encoding.as_str() {
                "scalar" if !scalar_shape(&shape) => return Err(bad()),
                "repeat" if !matches!(&shape, Shape::List(item) if scalar_shape(item)) => {
                    return Err(bad());
                }
                "deepObject" if !matches!(&shape, Shape::Record(_)) => return Err(bad()),
                _ => {}
            }
            if arg.name.is_empty()
                || arg.wire.is_empty()
                || arg.wire.len() > 256
                || arg.wire.chars().any(char::is_control)
                || !names.insert(&arg.name)
            {
                return Err(bad());
            }
            let wire = if arg.location == "header" {
                arg.wire.to_ascii_lowercase()
            } else {
                arg.wire.clone()
            };
            if !destinations.insert((arg.location.clone(), wire)) {
                return Err(bad());
            }
            match arg.location.as_str() {
                "path" => {
                    let marker = format!("{{{}}}", arg.wire);
                    // Whole segments only. This makes encoding and dot-segment defense unambiguous.
                    if !arg.required
                        || arg.encoding != "scalar"
                        || !route.split('/').any(|s| s == marker)
                    {
                        return Err(bad());
                    }
                    remaining = remaining.replace(&marker, "parameter");
                }
                "query" => {
                    if !matches!(arg.encoding.as_str(), "scalar" | "repeat" | "deepObject")
                        || auth.uses_query(&arg.wire)
                    {
                        return Err(bad());
                    }
                }
                "header" => {
                    let header = HeaderName::from_bytes(arg.wire.as_bytes()).map_err(|_| bad())?;
                    if arg.encoding != "scalar"
                        || auth.uses_header(header.as_str())
                        || matches!(
                            header.as_str(),
                            "host"
                                | "authorization"
                                | "cookie"
                                | "content-type"
                                | "content-length"
                                | "transfer-encoding"
                                | "connection"
                                | "upgrade"
                                | "trailer"
                                | "te"
                                | "proxy-authorization"
                                | "proxy-connection"
                        )
                    {
                        return Err(bad());
                    }
                }
                "body" => {
                    bodies += 1;
                    if arg.encoding != "json" || matches!(method.as_str(), "GET" | "HEAD") {
                        return Err(bad());
                    }
                }
                _ => return Err(bad()),
            }
        }
        if bodies > 1 || remaining.contains(['{', '}']) {
            return Err(bad());
        }
        Ok(Self {
            method,
            route,
            arguments,
            responses: Arc::new(responses),
            auth,
        })
    }
    pub(super) fn prepare(
        &self,
        base: &Url,
        call: &Call,
        inner: &Inner,
    ) -> Result<PreparedRequest, Failure> {
        let bad = || {
            diagnostics::failure(
                "/arguments".into(),
                "HTTP_REQUEST_LIMIT",
                "Combined request exceeds its configured HTTP byte limit",
            )
        };
        if call
            .arguments
            .keys()
            .any(|name| !self.arguments.iter().any(|a| &a.name == name))
        {
            return Err(diagnostics::failure(
                "/arguments".into(),
                "HTTP_UNKNOWN_ARGUMENT",
                "Argument is not declared by this operation; review the operation's parameters",
            ));
        }
        let mut route = self.route.clone();
        let mut query = Vec::new();
        let mut headers = HeaderMap::new();
        let mut body = None;
        let mut argument_bytes = 0usize;
        let mut query_bytes = 0usize;
        for arg in &self.arguments {
            let Some(value) = call.arguments.get(&arg.name) else {
                if arg.required {
                    return Err(arg.issue("HTTP_REQUIRED_ARGUMENT", "required argument is missing"));
                } else {
                    continue;
                }
            };
            // Normalize through the foreign JSON boundary, not by double-wrapping native Some.
            let encoded = encode_request_data(value.data(), inner.config.request)
                .map_err(|e| arg.codec_issue(e))?;
            argument_bytes = argument_bytes
                .checked_add(encoded.len())
                .filter(|n| *n <= inner.config.request.bytes)
                .ok_or_else(|| {
                    arg.issue(
                        "HTTP_REQUEST_BYTES",
                        "combined arguments exceed the request byte limit",
                    )
                })?;
            let data = decode_json_for_contract(&encoded, inner.config.request, &arg.contract)
                .map_err(|e| arg.codec_issue(e))?;
            let issues = arg.contract.issues(&data);
            if !issues.is_empty() {
                return Err(arg.contract_issues(issues));
            }
            match arg.location.as_str() {
                "path" => {
                    let plain =
                        scalar(&data, inner.config.url_bytes).map_err(|e| arg.encoding_issue(e))?;
                    if plain.is_empty() || plain == "." || plain == ".." {
                        return Err(arg.issue(
                            "HTTP_PATH_SEGMENT",
                            "path segment must be nonempty and cannot be a dot segment",
                        ));
                    }
                    let marker = format!("{{{}}}", arg.wire);
                    let encoded = segment(&plain);
                    let count = route.matches(&marker).count();
                    let size = count
                        .checked_mul(encoded.len())
                        .and_then(|n| n.checked_add(route.len() - count * marker.len()))
                        .filter(|n| *n <= inner.config.url_bytes)
                        .ok_or_else(|| {
                            arg.issue("HTTP_URL_BYTES", "encoded path exceeds the URL byte limit")
                        })?;
                    route = route.replace(&marker, &encoded);
                    debug_assert_eq!(route.len(), size);
                }
                "header" => {
                    headers.insert(
                        HeaderName::from_bytes(arg.wire.as_bytes()).map_err(|_| {
                            arg.issue("HTTP_HEADER_NAME", "declared header name is invalid")
                        })?,
                        HeaderValue::from_str(
                            &scalar(&data, inner.config.header_bytes)
                                .map_err(|e| arg.encoding_issue(e))?,
                        )
                        .map_err(|_| {
                            arg.issue(
                                "HTTP_HEADER_VALUE",
                                "value cannot be encoded as an HTTP header",
                            )
                        })?,
                    );
                    if headers
                        .iter()
                        .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
                        .sum::<usize>()
                        > inner.config.header_bytes
                    {
                        return Err(arg.issue(
                            "HTTP_HEADER_BYTES",
                            "combined headers exceed the header byte limit",
                        ));
                    }
                }
                "query" => match arg.encoding.as_str() {
                    "repeat" => {
                        let Data::List(items) = &data else {
                            return Err(
                                arg.issue("HTTP_QUERY_REPEAT", "repeat encoding requires a list")
                            );
                        };
                        for item in items {
                            push_query(
                                &mut query,
                                &mut query_bytes,
                                inner.config.url_bytes,
                                arg.wire.clone(),
                                scalar(item, inner.config.url_bytes)
                                    .map_err(|e| arg.encoding_issue(e))?,
                            )
                            .map_err(|e| arg.encoding_issue(e))?;
                        }
                    }
                    "deepObject" => {
                        let Data::Record(items) = &data else {
                            return Err(arg.issue(
                                "HTTP_QUERY_OBJECT",
                                "deepObject encoding requires a record",
                            ));
                        };
                        for (key, item) in items {
                            if key.contains(['[', ']']) {
                                return Err(arg.issue(
                                    "HTTP_QUERY_KEY",
                                    "deepObject keys cannot contain square brackets",
                                ));
                            }
                            push_query(
                                &mut query,
                                &mut query_bytes,
                                inner.config.url_bytes,
                                format!("{}[{key}]", arg.wire),
                                scalar(item, inner.config.url_bytes)
                                    .map_err(|e| arg.encoding_issue(e))?,
                            )
                            .map_err(|e| arg.encoding_issue(e))?;
                        }
                    }
                    _ => push_query(
                        &mut query,
                        &mut query_bytes,
                        inner.config.url_bytes,
                        arg.wire.clone(),
                        scalar(&data, inner.config.url_bytes).map_err(|e| arg.encoding_issue(e))?,
                    )
                    .map_err(|e| arg.encoding_issue(e))?,
                },
                "body" => {
                    body = Some(
                        encode_request_data(&data, inner.config.request)
                            .map_err(|e| arg.codec_issue(e))?,
                    );
                }
                _ => {
                    return Err(arg.issue(
                        "HTTP_ARGUMENT_LOCATION",
                        "declared HTTP location is unsupported",
                    ));
                }
            }
        }
        if query.iter().map(|(k, v)| k.len() + v.len()).sum::<usize>() > inner.config.url_bytes
            || headers
                .iter()
                .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
                .sum::<usize>()
                > inner.config.header_bytes
        {
            return Err(bad());
        }
        let mut target = request::route_url(base, &route, inner.config.url_bytes)
        .map_err(|_| {
            diagnostics::failure(
                "/arguments".into(),
                "HTTP_REQUEST_TARGET",
                "Cannot construct the HTTP target from the configured endpoint and encoded path",
            )
        })?;
        if !query.is_empty() {
            target.query_pairs_mut().extend_pairs(&query);
        }
        if target.as_str().len() > inner.config.url_bytes {
            return Err(bad());
        }
        let redactor = self.auth.inject(
            inner.credentials.as_ref(),
            &mut headers,
            &mut query,
            inner.config.header_bytes,
            inner.config.url_bytes,
        )?;
        if headers
            .iter()
            .map(|(k, v)| k.as_str().len() + v.as_bytes().len())
            .sum::<usize>()
            > inner.config.header_bytes
        {
            return Err(bad());
        }
        // Credentials may add query parameters; rebuild the final query and recheck its size.
        if !query.is_empty() {
            target.set_query(None);
            target.query_pairs_mut().extend_pairs(&query);
        }
        if target.as_str().len() > inner.config.url_bytes {
            return Err(bad());
        }
        let mut request = Request::new(
            Method::from_bytes(self.method.as_bytes()).map_err(|_| {
                diagnostics::failure(
                    "/arguments".into(),
                    "HTTP_REQUEST_METHOD",
                    "Declared HTTP method is invalid",
                )
            })?,
            target,
        );
        if let Some(body) = body {
            headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
            *request.body_mut() = Some(body.into());
        }
        *request.headers_mut() = headers;
        Ok(PreparedRequest {
            request,
            redactor,
            responses: self.responses.clone(),
        })
    }
}
fn push_query(
    query: &mut Vec<(String, String)>,
    bytes: &mut usize,
    limit: usize,
    key: String,
    value: String,
) -> Result<(), Failure> {
    *bytes = bytes
        .checked_add(key.len())
        .and_then(|n| n.checked_add(value.len()))
        .filter(|n| *n <= limit)
        .ok_or(Failure::Request("HTTP query exceeds its byte budget"))?;
    query.push((key, value));
    Ok(())
}
fn scalar(data: &Data, limit: usize) -> Result<String, Failure> {
    let text = match data {
        Data::Text(v) => v.to_string(),
        Data::Int(v) => v.to_string(),
        Data::Bool(v) => v.to_string(),
        Data::Decimal(v) => v
            .plain_text(limit)
            .ok_or(Failure::Request("parameter exceeds its byte budget"))?,
        _ => {
            return Err(Failure::Request(
                "HTTP parameter requires a non-null scalar",
            ));
        }
    };
    if text.len() > limit {
        return Err(Failure::Request("parameter exceeds its byte budget"));
    }
    Ok(text)
}
fn segment(text: &str) -> String {
    use std::fmt::Write;
    let mut encoded = String::new();
    for b in text.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(b));
        } else {
            let _ = write!(encoded, "%{b:02X}");
        }
    }
    encoded
}
