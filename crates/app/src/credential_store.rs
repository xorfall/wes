//! Home-scoped provider credentials. macOS keeps them in the user's Keychain; other platforms
//! use the data home's encrypted vault. No key values or persistence index are written into
//! workspace/config files.
use crate::credential_vault::CredentialVault;
use std::{path::PathBuf, sync::Arc};
use wes_engine::credentials::material::SecureStore;
#[cfg(target_os = "macos")]
use wes_engine::credentials::{CredentialError, Secret};

/// The secure store for a data home, with the vault the user controls where the platform has no
/// system store.
///
/// # Arguments
/// * `home` - The opened data home directory.
///
/// # Returns
/// The store remembered credentials use, and the vault when the application owns it.
#[cfg(target_os = "macos")]
pub fn platform(home: PathBuf) -> (Arc<dyn SecureStore>, Option<Arc<CredentialVault>>) {
    (Arc::new(PlatformCredentials::new(home)), None)
}
/// The secure store for a data home, with the vault the user controls where the platform has no
/// system store.
///
/// # Arguments
/// * `home` - The opened data home directory.
///
/// # Returns
/// The store remembered credentials use, and the vault when the application owns it.
#[cfg(not(target_os = "macos"))]
pub fn platform(home: PathBuf) -> (Arc<dyn SecureStore>, Option<Arc<CredentialVault>>) {
    let vault = CredentialVault::new(home, crate::credential_vault::VaultCost::default());
    (vault.clone(), Some(vault))
}
#[cfg(target_os = "macos")]
pub struct PlatformCredentials {
    home: PathBuf,
}
#[cfg(target_os = "macos")]
impl PlatformCredentials {
    pub fn new(home: PathBuf) -> Self {
        Self { home }
    }
    fn account(&self, reference: &str) -> Result<String, CredentialError> {
        use sha2::{Digest, Sha256};
        let home = crate::data_home::keychain_account(&self.home)
            .map_err(|_| CredentialError::Unavailable)?;
        Ok(format!("{home}:{:x}", Sha256::digest(reference.as_bytes())))
    }
}
#[cfg(target_os = "macos")]
const SERVICE: &str = "app.wesdesk.provider.credential";
#[cfg(target_os = "macos")]
impl SecureStore for PlatformCredentials {
    fn supported(&self) -> bool {
        true
    }
    fn get(&self, reference: &str) -> Result<Option<Secret>, CredentialError> {
        match security_framework::passwords::get_generic_password(
            SERVICE,
            &self.account(reference)?,
        ) {
            Ok(bytes) => Ok(Some(std::sync::Arc::new(
                wes_engine::credentials::SecretString::from(
                    String::from_utf8(bytes).map_err(|_| CredentialError::Unavailable)?,
                ),
            ))),
            Err(e) if e.code() == -25300 => Ok(None),
            Err(_) => Err(CredentialError::Unavailable),
        }
    }
    fn set(&self, reference: &str, value: &Secret) -> Result<(), CredentialError> {
        use wes_engine::credentials::ExposeSecret;
        security_framework::passwords::set_generic_password(
            SERVICE,
            &self.account(reference)?,
            value.expose_secret().as_bytes(),
        )
        .map_err(|_| CredentialError::Unavailable)
    }
    fn remove(&self, reference: &str) -> Result<(), CredentialError> {
        match security_framework::passwords::delete_generic_password(
            SERVICE,
            &self.account(reference)?,
        ) {
            Ok(()) => Ok(()),
            Err(e) if e.code() == -25300 => Ok(()),
            Err(_) => Err(CredentialError::Unavailable),
        }
    }
}
