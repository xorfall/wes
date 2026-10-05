use super::{Failure, HttpConfigError, Inner, auth::Redactor};
use crate::codec::check_arguments_detailed;
use reqwest::{Request, Url};
use wes_engine::providers::Call;

pub(super) fn base_url(base: &str, limit: usize) -> Result<Url, HttpConfigError> {
    if base.len() > limit
        || base.chars().any(char::is_control)
        || base.contains('\\')
        || base.trim() != base
        || has_dot_segments(base)
    {
        return Err(HttpConfigError("invalid base URL"));
    }
    let url = Url::parse(base).map_err(|_| HttpConfigError("invalid base URL"))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(HttpConfigError(
            "base must be an HTTP(S) URL without userinfo, query or fragment",
        ));
    }
    Ok(url)
}
/// Append a bounded route without accepting another origin, query or fragment.
pub(super) fn route_url(base: &Url, path: &str, limit: usize) -> Result<Url, HttpConfigError> {
    if (!path.is_empty() && !path.starts_with('/'))
        || path.contains(['?', '#', '\\'])
        || path.chars().any(char::is_control)
        || path.len() > limit
        || has_dot_segments(path)
    {
        return Err(HttpConfigError(
            "binding must be a path without query, fragment or backslash",
        ));
    }
    let prefix = base.as_str().strip_suffix('/').unwrap_or(base.as_str());
    if prefix
        .len()
        .checked_add(path.len())
        .is_none_or(|n| n > limit)
    {
        return Err(HttpConfigError("binding URL exceeds its byte budget"));
    }
    let target = Url::parse(&format!("{prefix}{}", path))
        .map_err(|_| HttpConfigError("invalid binding URL"))?;
    if target.origin() != base.origin() {
        return Err(HttpConfigError("binding leaves the configured origin"));
    }
    Ok(target)
}
fn has_dot_segments(path: &str) -> bool {
    path.split('/').any(|segment| {
        matches!(
            segment.to_ascii_lowercase().replace("%2e", ".").as_str(),
            "." | ".."
        )
    })
}
pub(super) struct PreparedRequest {
    pub request: Request,
    pub redactor: Redactor,
    pub responses: std::sync::Arc<std::collections::BTreeMap<u16, super::explicit::Response>>,
}
impl Inner {
    pub(super) fn prepare(&self, call: &Call) -> Result<PreparedRequest, Failure> {
        let operation = self
            .operations
            .get(&call.capability.path)
            .ok_or(Failure::Request("HTTP capability has no operation"))?;
        check_arguments_detailed(&call.arguments, self.config.request).map_err(|error| {
            if let Some(argument) = operation
                .arguments
                .iter()
                .find(|arg| Some(arg.name.as_str()) == error.name)
            {
                return argument.codec_issue(error.error);
            }
            super::explicit::diagnostics::failure(
                "/arguments".into(),
                "HTTP_REQUEST_BUDGET",
                "Arguments exceed the HTTP encoding/work budget",
            )
        })?;
        operation.prepare(&self.base, call, self)
    }
}
