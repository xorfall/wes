//! The explicit Wes JSON descriptor, not OpenAPI or a user type package.
//! Parsing and validation have no registry, filesystem or provider-execution authority.
mod contract;
pub const VERSION: u32 = 1;
pub(crate) mod notes;
use crate::{
    codec::{Limits, raw},
    http::{Auth, HttpConfig, HttpInvoker, HttpProvider},
};
use indexmap::IndexMap;
use std::{collections::BTreeSet, sync::Arc};
use thiserror::Error;
use wes_core::{
    Data, Shape,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::credentials::Credentials;

/// Diagnostics deliberately do not retain JSON parser errors, URLs or arbitrary input values.
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("invalid provider descriptor: {0}")]
pub struct DescriptorError(pub &'static str);
type Result<T> = std::result::Result<T, DescriptorError>;
type Fields = IndexMap<String, Data>;

/// A fully validated, unregistered HTTP provider. Warnings must be surfaced by the importer.
pub struct Reading {
    pub description: ProviderDescription,
    pub invoker: HttpInvoker,
    pub warnings: Vec<String>,
}

/// Reads the current explicit contract, bounded to 1 MiB / 20,000 JSON nodes.
/// The caller supplies the execution endpoint; documentation has no destination authority.
/// Credential availability is advisory at import; each invocation performs a fresh lookup.
pub fn read(
    bytes: &[u8],
    name: Option<&str>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    endpoint: &str,
) -> Result<Reading> {
    read_bound(
        bytes,
        name,
        credentials,
        config,
        wes_engine::imports::ImportMode::Live,
        Some(endpoint),
    )
}
pub(crate) fn read_bound(
    bytes: &[u8],
    name: Option<&str>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    mode: wes_engine::imports::ImportMode,
    endpoint: Option<&str>,
) -> Result<Reading> {
    read_selected(
        bytes,
        name,
        credentials,
        config,
        mode,
        endpoint,
        &Default::default(),
    )
}
pub(crate) fn read_selected(
    bytes: &[u8],
    name: Option<&str>,
    credentials: Arc<dyn Credentials>,
    config: HttpConfig,
    mode: wes_engine::imports::ImportMode,
    endpoint: Option<&str>,
    auth: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<Reading> {
    let mut context = raw::Context::new(Limits {
        bytes: 1024 * 1024,
        nodes: 20_000,
    });
    let root = context
        .root(bytes)
        .map_err(|_| DescriptorError("invalid or excessive JSON"))?;
    let data = raw::present(root, &mut context, 0)
        .map_err(|_| DescriptorError("invalid or excessive JSON"))?;
    let fields = object(&data)?;
    contract::read(
        root.get().as_bytes(),
        fields,
        name,
        credentials,
        config,
        mode,
        endpoint,
        auth,
    )
}

fn read_auth(data: &Data) -> Result<Auth> {
    let Data::List(parts) = data else {
        return Err(DescriptorError("auth must be an array"));
    };
    if parts.is_empty() || parts.len() > 32 {
        return Err(DescriptorError("expected 1 to 32 authentication parts"));
    }
    let mut auth = Vec::new();
    for part in parts {
        let fields = object(part)?;
        known(
            fields,
            &["secret", "header", "query", "user", "userSecret", "scheme"],
        )?;
        let secret = text(fields, "secret")?.to_owned();
        let user = optional_text(fields, "user")?;
        let user_secret = optional_text(fields, "userSecret")?;
        let query = optional_text(fields, "query")?;
        let header = optional_text(fields, "header")?;
        let scheme = optional_text(fields, "scheme")?;
        if usize::from(user_secret.is_some())
            + usize::from(user.is_some())
            + usize::from(query.is_some())
            + usize::from(header.is_some())
            > 1
            || (scheme.is_some() && (user.is_some() || user_secret.is_some() || query.is_some()))
        {
            return Err(DescriptorError(
                "authentication destinations are mutually exclusive",
            ));
        }
        auth.push(if let Some(user_secret) = user_secret {
            Auth::BasicCredentials {
                user_secret: user_secret.into(),
                secret,
            }
        } else if let Some(user) = user {
            Auth::Basic {
                user: user.into(),
                secret,
            }
        } else if let Some(parameter) = query {
            Auth::Query {
                parameter: parameter.into(),
                secret,
            }
        } else {
            Auth::Header {
                name: header.unwrap_or("Authorization").into(),
                scheme: scheme.unwrap_or("Bearer").into(),
                secret,
            }
        });
    }
    Ok(if auth.len() == 1 {
        auth.pop().expect("one part")
    } else {
        Auth::Several(auth)
    })
}
fn object(data: &Data) -> Result<&Fields> {
    match data {
        Data::Record(fields) => Ok(fields),
        _ => Err(DescriptorError("expected an object")),
    }
}
fn required<'a>(fields: &'a Fields, key: &'static str) -> Result<&'a Data> {
    fields.get(key).ok_or(DescriptorError(key))
}
fn as_text(data: &Data) -> Result<&str> {
    match data {
        Data::Text(text) => Ok(text),
        _ => Err(DescriptorError("expected text")),
    }
}
fn text<'a>(fields: &'a Fields, key: &'static str) -> Result<&'a str> {
    as_text(required(fields, key)?)
}
fn optional_text<'a>(fields: &'a Fields, key: &str) -> Result<Option<&'a str>> {
    fields.get(key).map(as_text).transpose()
}
fn known(fields: &Fields, keys: &[&str]) -> Result<()> {
    if fields.keys().any(|key| !keys.contains(&key.as_str())) {
        return Err(DescriptorError("unknown descriptor field"));
    }
    Ok(())
}
