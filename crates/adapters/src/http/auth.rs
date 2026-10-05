use super::{Failure, HttpConfigError};
use base64::{Engine, engine::general_purpose::STANDARD};
use indexmap::{IndexMap, IndexSet};
use reqwest::header::{AUTHORIZATION, HeaderMap, HeaderName, HeaderValue};
use wes_engine::credentials::{Credentials, ExposeSecret, Secret, SecretString};

/// Configuration contains credential names, not credential values.
#[derive(Clone, Debug)]
pub enum Auth {
    Header {
        name: String,
        scheme: String,
        secret: String,
    },
    Basic {
        user: String,
        secret: String,
    },
    Query {
        parameter: String,
        secret: String,
    },
    BasicCredentials {
        user_secret: String,
        secret: String,
    },
    /// A valid contract whose environment has not selected an alternative.
    Unselected,
    Several(Vec<Auth>),
}
pub(super) struct CompiledAuth {
    parts: Vec<Part>,
    unselected: bool,
}
enum Part {
    BasicCredentials {
        user_secret: String,
        secret: String,
    },
    Header {
        name: HeaderName,
        scheme: String,
        secret: String,
    },
    Basic {
        user: String,
        secret: String,
    },
    Query {
        parameter: String,
        secret: String,
    },
}
impl Part {
    fn secret(&self) -> &str {
        match self {
            Self::BasicCredentials { secret, .. }
            | Self::Header { secret, .. }
            | Self::Basic { secret, .. }
            | Self::Query { secret, .. } => secret,
        }
    }
}
impl CompiledAuth {
    pub(super) fn uses_header(&self, name: &str) -> bool {
        self.parts.iter().any(|p| match p {
            Part::Header { name: n, .. } => n.as_str() == name,
            Part::Basic { .. } | Part::BasicCredentials { .. } => name == "authorization",
            _ => false,
        })
    }
    pub(super) fn uses_query(&self, name: &str) -> bool {
        self.parts
            .iter()
            .any(|p| matches!(p, Part::Query {parameter,..} if parameter == name))
    }
    pub fn new(auth: Option<Auth>) -> Result<Self, HttpConfigError> {
        if matches!(auth, Some(Auth::Unselected)) {
            return Ok(Self {
                parts: vec![],
                unselected: true,
            });
        }
        let mut pending = auth.into_iter().map(|auth| (auth, 0)).collect::<Vec<_>>();
        let mut parts = Vec::new();
        while let Some((auth, depth)) = pending.pop() {
            if depth > 16 || parts.len() + pending.len() >= 32 {
                return Err(HttpConfigError(
                    "authentication exceeds its structural budget",
                ));
            }
            let part = match auth {
                Auth::Unselected => {
                    return Err(HttpConfigError("unselected auth cannot be nested"));
                }
                Auth::BasicCredentials {
                    user_secret,
                    secret,
                } => {
                    if !valid_name(&user_secret) {
                        return Err(HttpConfigError("invalid basic username credential name"));
                    }
                    Part::BasicCredentials {
                        user_secret,
                        secret,
                    }
                }
                Auth::Several(children) => {
                    if children.len() < 2 || children.len() > 32 {
                        return Err(HttpConfigError(
                            "composite authentication needs 2 to 32 parts",
                        ));
                    }
                    pending.extend(children.into_iter().rev().map(|child| (child, depth + 1)));
                    continue;
                }
                Auth::Header {
                    name,
                    scheme,
                    secret,
                } => {
                    if name.len() > 256 {
                        return Err(HttpConfigError(
                            "authentication header name exceeds its byte budget",
                        ));
                    }
                    let name = HeaderName::from_bytes(name.as_bytes())
                        .map_err(|_| HttpConfigError("invalid authentication header name"))?;
                    if matches!(
                        name.as_str(),
                        "host"
                            | "content-type"
                            | "content-length"
                            | "transfer-encoding"
                            | "connection"
                            | "upgrade"
                            | "trailer"
                            | "proxy-authorization"
                            | "proxy-connection"
                    ) {
                        return Err(HttpConfigError(
                            "authentication cannot override transport framing or proxy headers",
                        ));
                    }
                    if scheme.len() > 256
                        || !scheme.is_ascii()
                        || scheme.chars().any(char::is_control)
                    {
                        return Err(HttpConfigError("invalid authentication scheme"));
                    }
                    Part::Header {
                        name,
                        scheme,
                        secret,
                    }
                }
                Auth::Basic { user, secret } => {
                    if user.len() > 1024 || user.chars().any(char::is_control) {
                        return Err(HttpConfigError("invalid basic-auth username"));
                    }
                    Part::Basic { user, secret }
                }
                Auth::Query { parameter, secret } => {
                    if !valid_name(&parameter) {
                        return Err(HttpConfigError("invalid authentication query name"));
                    }
                    Part::Query { parameter, secret }
                }
            };
            if !valid_name(part.secret()) {
                return Err(HttpConfigError("invalid credential name"));
            }
            parts.push(part);
        }
        let mut headers = IndexSet::new();
        let mut queries = IndexSet::new();
        for part in &parts {
            let unique = match part {
                Part::Header { name, .. } => headers.insert(name.as_str()),
                Part::Basic { .. } | Part::BasicCredentials { .. } => {
                    headers.insert("authorization")
                }
                Part::Query { parameter, .. } => queries.insert(parameter.as_str()),
            };
            if !unique {
                return Err(HttpConfigError("duplicate authentication destination"));
            }
        }
        Ok(Self {
            parts,
            unselected: false,
        })
    }
    pub fn names(&self) -> Vec<String> {
        self.parts
            .iter()
            .flat_map(|part| {
                let mut names = vec![part.secret().to_owned()];
                if let Part::BasicCredentials { user_secret, .. } = part {
                    names.push(user_secret.clone());
                }
                names
            })
            .collect::<IndexSet<_>>()
            .into_iter()
            .collect()
    }
    pub fn hazards(&self) -> Vec<String> {
        self.parts.iter().filter_map(|part| match part {
            Part::Query { parameter,.. } => Some(format!("Query credential '{parameter}' can be retained in server and proxy URL logs; prefer a header when supported")),
            _ => None,
        }).collect()
    }
    pub fn inject(
        &self,
        credentials: &dyn Credentials,
        headers: &mut HeaderMap,
        query: &mut Vec<(String, String)>,
        header_limit: usize,
        query_limit: usize,
    ) -> Result<Redactor, Failure> {
        if self.unselected {
            return Err(Failure::Request(
                "authentication choice is missing; select the endpoint's schemes in environment bind.auth",
            ));
        }
        let mut values = IndexMap::<String, Secret>::new();
        let snapshots = credentials
            .snapshot(&self.names())
            .map_err(|error| match error {
                wes_engine::credentials::CredentialError::AccessDenied(provider) => {
                    Failure::CredentialAccess(provider)
                }
                _ => Failure::CredentialLookup,
            })?;
        let mut redactor = Redactor { forms: Vec::new() };
        let mut header_bytes = 0usize;
        let mut query_bytes = query
            .iter()
            .try_fold(0usize, |total, (k, v)| {
                total.checked_add(k.len())?.checked_add(v.len())
            })
            .ok_or(Failure::Request("HTTP query exceeds its byte budget"))?;
        for part in &self.parts {
            // Read each named credential once per request, even when it feeds several destinations.
            if !values.contains_key(part.secret()) {
                let secret = snapshots
                    .get(part.secret())
                    .cloned()
                    .flatten()
                    .ok_or_else(|| Failure::Credential(part.secret().to_owned()))?;
                if secret.expose_secret().is_empty() || secret.expose_secret().len() > 64 * 1024 {
                    return Err(Failure::CredentialInvalid);
                }
                redactor.remember(secret.expose_secret());
                values.insert(part.secret().to_owned(), secret);
            }
            let value = values[part.secret()].expose_secret();
            let (name, text) = match part {
                Part::Header { name, scheme, .. } => (
                    name.clone(),
                    if scheme.trim().is_empty() {
                        value.to_owned()
                    } else {
                        format!("{scheme} {value}")
                    },
                ),
                Part::BasicCredentials { user_secret, .. } => {
                    let username = snapshots
                        .get(user_secret)
                        .and_then(|v| v.as_ref())
                        .ok_or_else(|| Failure::Credential(user_secret.clone()))?;
                    let username = username.expose_secret();
                    if username.is_empty()
                        || username.len() > 1024
                        || username.contains(':')
                        || username.chars().any(char::is_control)
                    {
                        return Err(Failure::CredentialInvalid);
                    }
                    redactor.remember(username);
                    let encoded = STANDARD.encode(format!("{username}:{value}"));
                    redactor.forms.push(SecretString::from(encoded.clone()));
                    (AUTHORIZATION, format!("Basic {encoded}"))
                }
                Part::Basic { user, .. } => {
                    let encoded = STANDARD.encode(format!("{user}:{value}"));
                    redactor.forms.push(SecretString::from(encoded.clone()));
                    (AUTHORIZATION, format!("Basic {encoded}"))
                }
                Part::Query { parameter, .. } => {
                    if query.iter().any(|(key, _)| key == parameter) {
                        return Err(Failure::Request(
                            "query argument conflicts with authentication",
                        ));
                    }
                    query_bytes = query_bytes
                        .checked_add(parameter.len())
                        .and_then(|n| n.checked_add(value.len()))
                        .filter(|n| *n <= query_limit)
                        .ok_or(Failure::Request("HTTP query exceeds its byte budget"))?;
                    query.push((parameter.clone(), value.to_owned()));
                    continue;
                }
            };
            header_bytes = header_bytes
                .checked_add(name.as_str().len())
                .and_then(|n| n.checked_add(text.len()))
                .filter(|n| *n <= header_limit)
                .ok_or(Failure::Request("HTTP headers exceed their byte budget"))?;
            let mut value = HeaderValue::from_bytes(text.as_bytes()).map_err(|_| {
                Failure::Request("credential cannot be represented as an HTTP header")
            })?;
            value.set_sensitive(true);
            headers.insert(name, value);
        }
        redactor
            .forms
            .sort_by_key(|form| std::cmp::Reverse(form.expose_secret().len()));
        redactor
            .forms
            .dedup_by(|a, b| a.expose_secret() == b.expose_secret());
        Ok(redactor)
    }
}
fn valid_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.len() <= 256
        && !name.chars().any(|ch| ch.is_control() || ch == '=')
}

/// Known injected forms only, not universal detection of secrets in arbitrary provider output.
pub(super) struct Redactor {
    forms: Vec<SecretString>,
}
impl Redactor {
    pub(super) fn empty() -> Self {
        Self { forms: vec![] }
    }
    pub(super) fn remember(&mut self, secret: &str) {
        self.forms.push(SecretString::from(secret.to_owned()));
        self.forms.push(SecretString::from(
            url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>(),
        ));
        let quoted = serde_json::to_string(secret).expect("strings serialize");
        self.forms
            .push(SecretString::from(quoted[1..quoted.len() - 1].to_owned()));
    }
    pub fn excerpt(&self, bytes: &[u8], limit: usize) -> String {
        let text = String::from_utf8_lossy(bytes);
        // Trimming first could reveal a credential whose leading/trailing whitespace is significant.
        let mut remaining = text.as_ref();
        let mut output = String::new();
        let mut units = 0;
        while !remaining.is_empty() && units < limit {
            if let Some(form) = self
                .forms
                .iter()
                .find(|form| remaining.starts_with(form.expose_secret()))
            {
                remaining = &remaining[form.expose_secret().len()..];
                output.push_str("[REDACTED]");
                units += 10;
            } else {
                let ch = remaining.chars().next().expect("nonempty");
                if units + ch.len_utf16() > limit {
                    break;
                }
                remaining = &remaining[ch.len_utf8()..];
                output.push(if ch.is_control() { ' ' } else { ch });
                units += ch.len_utf16();
            }
        }
        if !remaining.is_empty() {
            output.push('…');
        }
        output.trim().to_owned()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::{CredentialLimits, MemoryCredentials};

    #[test]
    fn injected_headers_are_sensitive_and_redaction_handles_whitespace_and_escaped_forms() {
        let store = MemoryCredentials::with_environment(CredentialLimits::default(), |_| {
            Ok(Some(SecretString::from(" leading-private ")))
        });
        let auth = CompiledAuth::new(Some(Auth::Header {
            name: "X-Key".into(),
            scheme: String::new(),
            secret: "token".into(),
        }))
        .unwrap();
        let mut headers = HeaderMap::new();
        let redactor = auth
            .inject(&store, &mut headers, &mut vec![], 1024, 1024)
            .ok()
            .unwrap();
        assert!(headers["x-key"].is_sensitive());
        assert!(!format!("{headers:?}").contains("leading-private"));
        assert_eq!(redactor.excerpt(b" leading-private ", 500), "[REDACTED]");
        let mut redactor = Redactor { forms: vec![] };
        redactor.remember("line\nprivate");
        for text in ["line\nprivate", "line\\nprivate", "line%0Aprivate"] {
            assert_eq!(redactor.excerpt(text.as_bytes(), 500), "[REDACTED]");
        }
    }

    #[test]
    fn basic_credentials_redact_both_values_and_encoding_and_reject_ambiguous_usernames() {
        for username in ["qa-user", "bad:user", "bad\nuser"] {
            let name = username.to_owned();
            let store =
                MemoryCredentials::with_environment(CredentialLimits::default(), move |slot| {
                    Ok(Some(SecretString::from(if slot == "WES_USERNAME" {
                        name.clone()
                    } else {
                        "qa-password".into()
                    })))
                });
            let auth = CompiledAuth::new(Some(Auth::BasicCredentials {
                user_secret: "username".into(),
                secret: "password".into(),
            }))
            .unwrap();
            let mut headers = HeaderMap::new();
            let result = auth.inject(&store, &mut headers, &mut vec![], 1024, 1024);
            if username != "qa-user" {
                assert!(result.is_err());
                continue;
            }
            let redactor = result.ok().unwrap();
            assert!(headers[AUTHORIZATION].is_sensitive());
            let encoded = STANDARD.encode("qa-user:qa-password");
            for text in ["qa-user", "qa-password", encoded.as_str()] {
                assert_eq!(redactor.excerpt(text.as_bytes(), 500), "[REDACTED]");
            }
        }
    }

    #[test]
    fn composite_depth_and_count_are_bounded_and_destination_names_are_case_insensitive() {
        let part = || Auth::Header {
            name: "X-Key".into(),
            scheme: String::new(),
            secret: "token".into(),
        };
        assert!(CompiledAuth::new(Some(Auth::Several((0..33).map(|_| part()).collect()))).is_err());
        let mut nested = part();
        for _ in 0..17 {
            nested = Auth::Several(vec![part(), nested]);
        }
        assert!(CompiledAuth::new(Some(nested)).is_err());
        assert!(
            CompiledAuth::new(Some(Auth::Several(vec![
                part(),
                Auth::Header {
                    name: "x-key".into(),
                    scheme: String::new(),
                    secret: "other".into()
                }
            ])))
            .is_err()
        );
    }

    #[test]
    fn excerpts_are_bounded_control_safe_and_never_split_a_unicode_scalar() {
        let redactor = Redactor { forms: vec![] };
        assert_eq!(redactor.excerpt("😀x".as_bytes(), 2), "😀…");
        assert_eq!(redactor.excerpt(b"a\x1bb\n", 500), "a b");
        assert!(redactor.excerpt("😀".repeat(10000).as_bytes(), 500).len() <= 1003);
    }
}
