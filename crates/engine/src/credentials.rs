//! Read-only credential port for invokers. Metadata names credentials but never owns their values.
pub use secrecy::{ExposeSecret, SecretString};
use std::sync::Arc;
use thiserror::Error;

pub type Secret = Arc<SecretString>;

#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum CredentialError {
    #[error(
        "credential access has not been granted for provider '{0}'; grant provider access in /env or use --grant-provider {0} in the CLI (supply credential values separately)"
    )]
    AccessDenied(String),
    #[error("invalid credential name")]
    InvalidName,
    #[error(
        "credential value must not be empty; supply a nonempty value or explicitly forget the credential"
    )]
    Empty,
    #[error("credential exceeds its configured size limit")]
    TooLarge,
    #[error("credential storage capacity exceeded")]
    Capacity,
    #[error("credential store is unavailable")]
    Unavailable,
    #[error(
        "the credential vault is locked or not set up; unlock or create it in Settings to use remembered credentials"
    )]
    Locked,
    #[error("credential environment value is not valid Unicode")]
    InvalidEnvironment,
}

/// An immutable snapshot of a single credential, safe to retain across later replacement/removal.
/// The port grants lookup only: invokers cannot supply credentials, enumerate the environment,
/// serialize a store or change it through this handle. Lookup must not perform network/disk I/O.
pub trait Credentials: Send + Sync + 'static {
    fn lookup(&self, name: &str) -> Result<Option<Secret>, CredentialError>;
    fn snapshot(
        &self,
        names: &[String],
    ) -> Result<std::collections::BTreeMap<String, Option<Secret>>, CredentialError> {
        if names.len() > 256 {
            return Err(CredentialError::Capacity);
        }
        names
            .iter()
            .map(|name| Ok((name.clone(), self.lookup(name)?)))
            .collect()
    }
}

pub mod material;
