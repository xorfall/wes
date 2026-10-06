//! Versioned API artifacts and read-only repository I/O. No workspace or execution authority.
pub mod draft;
mod draft_store;
mod extractor;
mod files;
mod repository;
pub use extractor::{
    Extraction, ExtractionReport, extract, extract_draft_controlled, extraction_code,
    extraction_message, extraction_report,
};
pub use files::{
    Library, atomic_bytes, atomic_bytes_in, atomic_json, exclusive_lock, read_json_file,
    validate_directory,
};
pub use repository::{Repository, RepositoryCatalog, RepositoryPackage, read_document};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{io, sync::Arc};

pub type Result<T> = std::result::Result<T, io::Error>;
pub fn max_descriptor() -> usize {
    wes_budgets::get("api.descriptor.bytes") as usize
}
pub fn max_source() -> usize {
    wes_budgets::get("api.source.bytes") as usize
}
/// Request structured OpenAPI JSON/YAML; source retrieval never crawls HTML.
pub const DOCUMENT_ACCEPT: &str = "application/json, application/yaml, text/yaml";
/// Shareable source identity. Absolute read/reopen paths belong to local library
/// records, not immutable descriptors; the source digest remains the identity.
pub fn portable_source_location(location: &str) -> String {
    if let Ok(mut url) = reqwest::Url::parse(location)
        && matches!(url.scheme(), "http" | "https")
    {
        let _ = url.set_username("");
        let _ = url.set_password(None);
        url.set_query(None);
        url.set_fragment(None);
        return url.to_string();
    }
    location
        .rsplit(['/', '\\'])
        .find(|part| !part.is_empty())
        .unwrap_or("OpenAPI document")
        .chars()
        .take(256)
        .collect()
}
pub fn error(message: &str) -> io::Error {
    io::Error::other(message)
}
pub fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
pub fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && value != "."
        && value != ".."
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct PackageKey {
    pub service: String,
    pub api_version: String,
    pub scope: String,
}
impl PackageKey {
    pub fn validate(&self) -> Result<()> {
        if [&self.service, &self.api_version, &self.scope]
            .iter()
            .all(|v| identifier(v))
        {
            Ok(())
        } else {
            Err(error(
                "service, apiVersion and scope require 1..100 ASCII identifier characters",
            ))
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Package {
    pub key: PackageKey,
    pub revision: String,
    pub accepted: bool,
    pub origin: String,
    pub source_digest: Option<String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Index {
    pub version: u32,
    pub packages: Vec<Package>,
}
impl Default for Index {
    fn default() -> Self {
        Self {
            version: 1,
            packages: vec![],
        }
    }
}

/// Same bounded parser and transport/type validator as environment import, using a non-executable
/// placeholder endpoint and a credentials port that can never consult live user values.
pub fn validate_descriptor(bytes: &[u8]) -> Result<Vec<String>> {
    if bytes.len() > max_descriptor() {
        return Err(error("descriptor exceeds 1 MiB"));
    }
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| error("invalid descriptor JSON"))?;
    if value.get("version").and_then(|v| v.as_u64()) != Some(crate::descriptor::VERSION.into()) {
        return Err(error("API library requires a current Wes descriptor"));
    }
    let credentials =
        crate::credentials::MemoryCredentials::with_environment(Default::default(), |_| Ok(None));
    let reading = crate::descriptor::read_bound(
        bytes,
        None,
        Arc::new(credentials),
        Default::default(),
        wes_engine::imports::ImportMode::Replay,
        Some("http://127.0.0.1:1"),
    )
    .map_err(|e| error(e.0))?;
    Ok(reading
        .warnings
        .into_iter()
        .map(|warning| warning.message)
        .collect())
}

/// Shared contract compiler resolves the environment's credential slots without reading values.
pub fn authentication_requirements(
    bytes: &[u8],
    auth: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<Vec<String>> {
    let credentials =
        crate::credentials::MemoryCredentials::with_environment(Default::default(), |_| Ok(None));
    let reading = crate::descriptor::read_selected(
        bytes,
        None,
        Arc::new(credentials),
        Default::default(),
        wes_engine::imports::ImportMode::Replay,
        Some("http://127.0.0.1:1"),
        auth,
    )
    .map_err(|e| error(e.0))?;
    Ok(reading.description.secrets().to_vec())
}

/// The same validated authentication metadata shown by :info, without secret access.
pub fn authentication_metadata(
    bytes: &[u8],
    auth: &std::collections::BTreeMap<String, Vec<String>>,
) -> Result<serde_json::Value> {
    let reading = crate::descriptor::read_selected(
        bytes,
        None,
        Arc::new(crate::credentials::MemoryCredentials::with_environment(
            Default::default(),
            |_| Ok(None),
        )),
        Default::default(),
        wes_engine::imports::ImportMode::Replay,
        Some("http://127.0.0.1:1"),
        auth,
    )
    .map_err(|e| error(e.0))?;
    let Some(wes_core::Data::Record(info)) = reading.description.information() else {
        return Ok(serde_json::json!([]));
    };
    let Some(authentication) = info.get("authentication") else {
        return Ok(serde_json::json!([]));
    };
    let bytes = crate::codec::encode_request_data(authentication, Default::default())
        .map_err(|_| error("Authentication metadata unavailable"))?;
    serde_json::from_slice(&bytes).map_err(std::io::Error::other)
}
