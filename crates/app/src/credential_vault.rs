//! Application-owned encrypted credential vault for platforms without a system secure store.
//!
//! One file in the data home holds every remembered credential, encrypted with
//! XChaCha20-Poly1305 under a key derived from the user's password with Argon2id. Neither the
//! password nor the key is ever written; every process start begins locked. There is no
//! recovery: a forgotten password leaves only an explicit reset, which removes the vault alone.
use argon2::{Algorithm, Argon2, Params, Version};
use base64::{Engine, engine::general_purpose::STANDARD};
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU64, Ordering},
    },
};
use thiserror::Error;
use wes_engine::credentials::{
    CredentialError, ExposeSecret, Secret, SecretString, material::SecureStore,
};
use zeroize::Zeroizing;

/// Vault file name inside the data home.
pub const VAULT_FILE: &str = "credential-vault.json";
/// Prefix of interrupted atomic writes; recognized by the data home, removed by a reset.
pub const PENDING_PREFIX: &str = ".credential-vault-";

const FORMAT: &str = "wes.credential-vault";
const FORMAT_VERSION: u32 = 1;
const KDF: &str = "argon2id";
const CIPHER: &str = "xchacha20poly1305";
const KEY_BYTES: usize = 32;
const SALT_BYTES: usize = 16;
const NONCE_BYTES: usize = 24;
const MIN_PASSWORD_CHARS: usize = 8;
const MAX_PASSWORD_BYTES: usize = 1024;
/// Remembered material is bounded at 1 MiB; this leaves room for encoding and authentication.
const MAX_FILE_BYTES: u64 = 4 * 1024 * 1024;
/// Bounds accepted from a file so a damaged or hostile header cannot demand unbounded work.
const MAX_MEMORY_KIB: u32 = 1024 * 1024;
const MAX_ITERATIONS: u32 = 16;
const MAX_PARALLELISM: u32 = 4;

/// Argon2id cost used when a vault is created. Unlocking uses the cost recorded in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VaultCost {
    pub memory_kib: u32,
    pub iterations: u32,
    pub parallelism: u32,
}
impl Default for VaultCost {
    fn default() -> Self {
        Self {
            memory_kib: 64 * 1024,
            iterations: 3,
            parallelism: 1,
        }
    }
}

/// What the user can do next with the vault.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VaultStatus {
    /// No vault exists; creating one sets its password.
    Absent,
    /// A vault exists and needs its password before remembered credentials are usable.
    Locked,
    /// Remembered credentials can be read and changed until the vault is locked or reset.
    Unlocked,
}
impl VaultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Absent => "absent",
            Self::Locked => "locked",
            Self::Unlocked => "unlocked",
        }
    }
}

#[derive(Debug, Error)]
pub enum VaultError {
    #[error("a credential vault already exists; unlock it or reset it first")]
    Exists,
    #[error("no credential vault exists; create one first")]
    Missing,
    #[error("the credential vault is locked")]
    Locked,
    #[error("the password is incorrect or the vault file was changed")]
    WrongPassword,
    #[error("the password must have at least 8 characters and at most 1024 bytes")]
    WeakPassword,
    #[error(
        "the credential vault file is damaged or uses an unsupported format; reset it to continue"
    )]
    Damaged,
    #[error("the credential vault file could not be accessed")]
    Io(#[from] io::Error),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct KdfHeader {
    algorithm: String,
    #[serde(flatten)]
    cost: VaultCost,
    salt: String,
}
/// Authenticated as associated data: changing any parameter makes decryption fail.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Header {
    format: String,
    version: u32,
    kdf: KdfHeader,
    cipher: String,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Envelope {
    #[serde(flatten)]
    header: Header,
    nonce: String,
    ciphertext: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Contents {
    entries: BTreeMap<String, String>,
}

struct Open {
    key: Zeroizing<[u8; KEY_BYTES]>,
    header: Header,
    entries: BTreeMap<String, Secret>,
    /// Digest of the file bytes this state was read from or written as.
    fingerprint: [u8; 32],
}

/// A data home's vault. Operations are serialized; the data home lock excludes other processes.
pub struct CredentialVault {
    home: PathBuf,
    cost: VaultCost,
    open: Mutex<Option<Open>>,
    generation: AtomicU64,
}
impl CredentialVault {
    /// A locked vault handle for `home`. Nothing is read or created until it is used.
    ///
    /// # Arguments
    /// * `home` - The data home directory that holds the vault file.
    /// * `cost` - Argon2id cost for a vault created through this handle.
    pub fn new(home: PathBuf, cost: VaultCost) -> Arc<Self> {
        Arc::new(Self {
            home,
            cost,
            open: Mutex::new(None),
            generation: AtomicU64::new(0),
        })
    }
    fn path(&self) -> PathBuf {
        self.home.join(VAULT_FILE)
    }
    fn guard(&self) -> Result<std::sync::MutexGuard<'_, Option<Open>>, VaultError> {
        self.open
            .lock()
            .map_err(|_| VaultError::Io(io::Error::other("vault state is poisoned")))
    }
    fn changed(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }
    /// Lock and announce the change when the file no longer matches the unlocked state.
    fn drop_open(&self, open: &mut Option<Open>) {
        if open.take().is_some() {
            self.changed();
        }
    }

    /// The current state. A vault removed or replaced outside this handle reports as locked or
    /// absent, never with stale unlocked contents.
    ///
    /// # Returns
    /// The vault status.
    ///
    /// # Errors
    /// Returns an error when the vault file cannot be inspected.
    pub fn status(&self) -> Result<VaultStatus, VaultError> {
        let mut open = self.guard()?;
        match self.current(&mut open) {
            Ok(Some(_)) => Ok(VaultStatus::Unlocked),
            Ok(None) => Ok(VaultStatus::Absent),
            Err(CredentialError::Locked) => Ok(VaultStatus::Locked),
            Err(_) => Err(VaultError::Io(io::Error::other(
                "the credential vault file could not be inspected",
            ))),
        }
    }

    /// Create an empty vault protected by `password` and leave it unlocked.
    ///
    /// # Errors
    /// Returns [`VaultError::Exists`] when a vault already exists, [`VaultError::WeakPassword`]
    /// for an unacceptable password, or an I/O error when it cannot be written.
    pub fn create(&self, password: &SecretString) -> Result<(), VaultError> {
        let password = validated(password)?;
        let mut open = self.guard()?;
        if read_file(&self.path())?.is_some() {
            return Err(VaultError::Exists);
        }
        let mut salt = [0; SALT_BYTES];
        getrandom::fill(&mut salt).map_err(|e| io::Error::other(e.to_string()))?;
        let header = Header {
            format: FORMAT.into(),
            version: FORMAT_VERSION,
            kdf: KdfHeader {
                algorithm: KDF.into(),
                cost: self.cost,
                salt: STANDARD.encode(salt),
            },
            cipher: CIPHER.into(),
        };
        let key = derive(password, &salt, self.cost)?;
        let mut created = Open {
            key,
            header,
            entries: BTreeMap::new(),
            fingerprint: [0; 32],
        };
        created.fingerprint = self.write(&created)?;
        *open = Some(created);
        self.changed();
        Ok(())
    }

    /// Unlock with `password`. A wrong password leaves the vault locked and unchanged.
    ///
    /// # Errors
    /// Returns [`VaultError::Missing`], [`VaultError::WrongPassword`], [`VaultError::Damaged`]
    /// or an I/O error.
    pub fn unlock(&self, password: &SecretString) -> Result<(), VaultError> {
        let password = password.expose_secret().as_bytes();
        if password.len() > MAX_PASSWORD_BYTES {
            return Err(VaultError::WrongPassword);
        }
        let mut open = self.guard()?;
        let bytes = read_file(&self.path())?.ok_or(VaultError::Missing)?;
        let envelope = parse(&bytes)?;
        let salt = STANDARD
            .decode(&envelope.header.kdf.salt)
            .map_err(|_| VaultError::Damaged)?;
        let key = derive(password, &salt, envelope.header.kdf.cost)?;
        let entries = decrypt(&key, &envelope)?;
        self.drop_open(&mut open);
        *open = Some(Open {
            key,
            header: envelope.header,
            entries,
            fingerprint: fingerprint(&bytes),
        });
        self.changed();
        Ok(())
    }

    /// Forget the key and every decrypted value held in memory.
    ///
    /// # Errors
    /// Returns an error only when the vault state is unavailable.
    pub fn lock(&self) -> Result<(), VaultError> {
        let mut open = self.guard()?;
        self.drop_open(&mut open);
        Ok(())
    }

    /// Permanently remove the vault and any interrupted writes. Workspace data is untouched.
    ///
    /// # Errors
    /// Returns an I/O error when the vault cannot be removed.
    pub fn reset(&self) -> Result<(), VaultError> {
        let mut open = self.guard()?;
        open.take();
        // Even a failed removal must not leave earlier reads cached as current.
        self.changed();
        for entry in fs::read_dir(&self.home)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if name == VAULT_FILE || name.starts_with(PENDING_PREFIX) {
                match fs::remove_file(entry.path()) {
                    Ok(()) => {}
                    Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                    Err(error) => return Err(error.into()),
                }
            }
        }
        sync_directory(&self.home)?;
        Ok(())
    }

    /// The unlocked state, reloaded with the same key when the file changed underneath it.
    fn current<'a>(
        &self,
        open: &'a mut Option<Open>,
    ) -> Result<Option<&'a mut Open>, CredentialError> {
        let Some(bytes) = read_file(&self.path()).map_err(|_| CredentialError::Unavailable)? else {
            self.drop_open(open);
            return Ok(None);
        };
        let Some(state) = open.as_ref() else {
            return Err(CredentialError::Locked);
        };
        let digest = fingerprint(&bytes);
        if state.fingerprint != digest {
            let reloaded = parse(&bytes).ok().and_then(|envelope| {
                let same_key = envelope.header.kdf.salt == state.header.kdf.salt
                    && envelope.header.kdf.cost == state.header.kdf.cost;
                same_key
                    .then(|| decrypt(&state.key, &envelope).ok())
                    .flatten()
                    .map(|entries| (envelope.header, entries))
            });
            match reloaded {
                Some((header, entries)) => {
                    let state = open.as_mut().expect("unlocked state");
                    state.header = header;
                    state.entries = entries;
                    state.fingerprint = digest;
                    self.changed();
                }
                None => {
                    self.drop_open(open);
                    return Err(CredentialError::Locked);
                }
            }
        }
        Ok(open.as_mut())
    }

    fn write(&self, state: &Open) -> Result<[u8; 32], VaultError> {
        let contents = Zeroizing::new(
            serde_json::to_vec(&Contents {
                entries: state
                    .entries
                    .iter()
                    .map(|(name, value)| (name.clone(), value.expose_secret().to_owned()))
                    .collect(),
            })
            .map_err(|e| io::Error::other(e.to_string()))?,
        );
        let mut nonce = [0; NONCE_BYTES];
        getrandom::fill(&mut nonce).map_err(|e| io::Error::other(e.to_string()))?;
        let aad = associated_data(&state.header)?;
        let ciphertext = cipher(&state.key)
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &contents,
                    aad: &aad,
                },
            )
            .map_err(|_| io::Error::other("vault encryption failed"))?;
        let bytes = serde_json::to_vec(&Envelope {
            header: state.header.clone(),
            nonce: STANDARD.encode(nonce),
            ciphertext: STANDARD.encode(ciphertext),
        })
        .map_err(|e| io::Error::other(e.to_string()))?;
        replace_file(&self.home, &bytes)?;
        Ok(fingerprint(&bytes))
    }

    /// Apply one change to the unlocked entries and publish it atomically.
    fn change(
        &self,
        apply: impl FnOnce(&mut BTreeMap<String, Secret>) -> bool,
    ) -> Result<(), CredentialError> {
        let mut open = self.guard().map_err(|_| CredentialError::Unavailable)?;
        let Some(state) = self.current(&mut open)? else {
            return Err(CredentialError::Locked);
        };
        let mut entries = state.entries.clone();
        if !apply(&mut entries) {
            return Ok(());
        }
        let candidate = Open {
            key: state.key.clone(),
            header: state.header.clone(),
            entries,
            fingerprint: state.fingerprint,
        };
        match self.write(&candidate) {
            Ok(digest) => {
                state.entries = candidate.entries;
                state.fingerprint = digest;
                Ok(())
            }
            Err(_) => {
                // The rename may or may not have happened; reread instead of assuming either.
                self.drop_open(&mut open);
                Err(CredentialError::Unavailable)
            }
        }
    }
}

impl SecureStore for CredentialVault {
    fn supported(&self) -> bool {
        true
    }
    fn generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
    fn get(&self, reference: &str) -> Result<Option<Secret>, CredentialError> {
        let mut open = self.guard().map_err(|_| CredentialError::Unavailable)?;
        Ok(self
            .current(&mut open)?
            .and_then(|state| state.entries.get(reference).cloned()))
    }
    fn set(&self, reference: &str, value: &Secret) -> Result<(), CredentialError> {
        self.change(|entries| {
            entries.insert(reference.to_owned(), value.clone());
            true
        })
    }
    fn remove(&self, reference: &str) -> Result<(), CredentialError> {
        let mut open = self.guard().map_err(|_| CredentialError::Unavailable)?;
        // Nothing can be remembered without a vault, so forgetting needs no unlock.
        if read_file(&self.path())
            .map_err(|_| CredentialError::Unavailable)?
            .is_none()
        {
            self.drop_open(&mut open);
            return Ok(());
        }
        drop(open);
        self.change(|entries| entries.remove(reference).is_some())
    }
}

fn validated(password: &SecretString) -> Result<&[u8], VaultError> {
    let text = password.expose_secret();
    if text.chars().count() < MIN_PASSWORD_CHARS || text.len() > MAX_PASSWORD_BYTES {
        return Err(VaultError::WeakPassword);
    }
    Ok(text.as_bytes())
}
fn derive(
    password: &[u8],
    salt: &[u8],
    cost: VaultCost,
) -> Result<Zeroizing<[u8; KEY_BYTES]>, VaultError> {
    if salt.len() != SALT_BYTES
        || cost.memory_kib > MAX_MEMORY_KIB
        || cost.iterations > MAX_ITERATIONS
        || cost.parallelism > MAX_PARALLELISM
    {
        return Err(VaultError::Damaged);
    }
    let params = Params::new(
        cost.memory_kib,
        cost.iterations,
        cost.parallelism,
        Some(KEY_BYTES),
    )
    .map_err(|_| VaultError::Damaged)?;
    let mut key = Zeroizing::new([0; KEY_BYTES]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(password, salt, key.as_mut())
        .map_err(|_| VaultError::Damaged)?;
    Ok(key)
}
fn cipher(key: &[u8; KEY_BYTES]) -> XChaCha20Poly1305 {
    XChaCha20Poly1305::new(&(*key).into())
}
fn associated_data(header: &Header) -> Result<Vec<u8>, VaultError> {
    serde_json::to_vec(header).map_err(|e| io::Error::other(e.to_string()).into())
}
fn parse(bytes: &[u8]) -> Result<Envelope, VaultError> {
    let envelope: Envelope = serde_json::from_slice(bytes).map_err(|_| VaultError::Damaged)?;
    let header = &envelope.header;
    if header.format != FORMAT
        || header.version != FORMAT_VERSION
        || header.kdf.algorithm != KDF
        || header.cipher != CIPHER
    {
        return Err(VaultError::Damaged);
    }
    Ok(envelope)
}
fn decrypt(
    key: &[u8; KEY_BYTES],
    envelope: &Envelope,
) -> Result<BTreeMap<String, Secret>, VaultError> {
    let nonce: [u8; NONCE_BYTES] = STANDARD
        .decode(&envelope.nonce)
        .ok()
        .and_then(|nonce| nonce.try_into().ok())
        .ok_or(VaultError::Damaged)?;
    let ciphertext = STANDARD
        .decode(&envelope.ciphertext)
        .map_err(|_| VaultError::Damaged)?;
    let aad = associated_data(&envelope.header)?;
    let plaintext = Zeroizing::new(
        cipher(key)
            .decrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VaultError::WrongPassword)?,
    );
    // Authenticated contents that fail to parse were written by an incompatible version.
    let contents: Contents = serde_json::from_slice(&plaintext).map_err(|_| VaultError::Damaged)?;
    Ok(contents
        .entries
        .into_iter()
        .map(|(name, value)| (name, Arc::new(SecretString::from(value))))
        .collect())
}
fn fingerprint(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}
/// The vault file's bytes, or None when it does not exist. Links and oversized files are refused.
fn read_file(path: &Path) -> Result<Option<Vec<u8>>, VaultError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error.into()),
    };
    if !metadata.is_file() || metadata.len() > MAX_FILE_BYTES {
        return Err(VaultError::Damaged);
    }
    let mut bytes = Vec::new();
    fs::File::open(path)?
        .take(MAX_FILE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(VaultError::Damaged);
    }
    Ok(Some(bytes))
}
/// Publish complete, synced bytes under the vault name without exposing a partial file.
fn replace_file(home: &Path, bytes: &[u8]) -> io::Result<()> {
    let pending = home.join(format!("{PENDING_PREFIX}{}", uuid::Uuid::new_v4()));
    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let written = (|| {
        let mut file = options.open(&pending)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        drop(file);
        fs::rename(&pending, home.join(VAULT_FILE))
    })();
    if written.is_err() {
        let _ = fs::remove_file(&pending);
    }
    written?;
    sync_directory(home)
}
fn sync_directory(directory: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        fs::File::open(directory)?.sync_all()
    }
    #[cfg(not(unix))]
    {
        let _ = directory;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Barrier;

    const FAST: VaultCost = VaultCost {
        memory_kib: 64,
        iterations: 1,
        parallelism: 1,
    };

    fn password(text: &str) -> SecretString {
        SecretString::from(text)
    }
    fn secret(text: &str) -> Secret {
        Arc::new(SecretString::from(text))
    }
    fn vault(home: &Path) -> Arc<CredentialVault> {
        CredentialVault::new(home.to_path_buf(), FAST)
    }
    fn stored(vault: &CredentialVault, name: &str) -> Option<String> {
        vault
            .get(name)
            .unwrap()
            .map(|value| value.expose_secret().to_owned())
    }

    #[test]
    fn should_persist_entries_encrypted_and_require_unlock_after_restart() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let first = vault(home.path());
        first.create(&password("correct horse")).unwrap();
        first
            .set("provider/key", &secret("synthetic-value"))
            .unwrap();

        // Act
        let restarted = vault(home.path());
        let locked = restarted.status().unwrap();
        let locked_read = restarted.get("provider/key");
        restarted.unlock(&password("correct horse")).unwrap();

        // Assert
        assert_eq!(locked, VaultStatus::Locked);
        assert!(matches!(locked_read, Err(CredentialError::Locked)));
        assert_eq!(
            stored(&restarted, "provider/key").as_deref(),
            Some("synthetic-value")
        );
        let file = fs::read_to_string(home.path().join(VAULT_FILE)).unwrap();
        assert!(!file.contains("synthetic-value"));
        assert!(!file.contains("provider/key"));
        assert!(!file.contains("correct horse"));
    }

    #[test]
    fn should_reject_a_wrong_password_without_changing_the_vault() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        vault(home.path())
            .create(&password("correct horse"))
            .unwrap();
        let before = fs::read(home.path().join(VAULT_FILE)).unwrap();
        let restarted = vault(home.path());

        // Act
        let result = restarted.unlock(&password("wrong horse"));

        // Assert
        assert!(matches!(result, Err(VaultError::WrongPassword)));
        assert_eq!(restarted.status().unwrap(), VaultStatus::Locked);
        assert_eq!(fs::read(home.path().join(VAULT_FILE)).unwrap(), before);
    }

    #[test]
    fn should_refuse_short_passwords_and_a_second_vault() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let vault = vault(home.path());

        // Act
        let short = vault.create(&password("short"));
        vault.create(&password("long enough")).unwrap();
        let second = vault.create(&password("another password"));

        // Assert
        assert!(matches!(short, Err(VaultError::WeakPassword)));
        assert!(matches!(second, Err(VaultError::Exists)));
    }

    #[test]
    fn should_fail_safely_for_tampered_truncated_and_unknown_files() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let original = vault(home.path());
        original.create(&password("correct horse")).unwrap();
        original.set("a", &secret("value")).unwrap();
        let path = home.path().join(VAULT_FILE);
        let bytes = fs::read(&path).unwrap();
        let mut envelope: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        let mut changed_cost = envelope.clone();
        changed_cost["kdf"]["iterations"] = 2.into();
        let mut changed_text = envelope.clone();
        let ciphertext = envelope["ciphertext"].as_str().unwrap().to_owned();
        let mut raw = STANDARD.decode(ciphertext).unwrap();
        raw[0] ^= 1;
        changed_text["ciphertext"] = STANDARD.encode(raw).into();
        envelope["version"] = 2.into();
        let cases: [(&[u8], fn(&VaultError) -> bool); 4] = [
            (&serde_json::to_vec(&changed_cost).unwrap(), |e| {
                matches!(e, VaultError::WrongPassword)
            }),
            (&serde_json::to_vec(&changed_text).unwrap(), |e| {
                matches!(e, VaultError::WrongPassword)
            }),
            (&bytes[..bytes.len() / 2], |e| {
                matches!(e, VaultError::Damaged)
            }),
            (&serde_json::to_vec(&envelope).unwrap(), |e| {
                matches!(e, VaultError::Damaged)
            }),
        ];

        for (content, expected) in cases {
            // Act
            fs::write(&path, content).unwrap();
            let restarted = vault(home.path());
            let result = restarted.unlock(&password("correct horse"));

            // Assert
            let error = result.unwrap_err();
            assert!(expected(&error), "unexpected {error:?}");
            assert_eq!(fs::read(&path).unwrap(), content);
            assert!(matches!(restarted.get("a"), Err(CredentialError::Locked)));
        }
    }

    #[test]
    fn should_lock_and_announce_when_the_file_is_replaced_or_removed_outside() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let vault_handle = vault(home.path());
        vault_handle.create(&password("correct horse")).unwrap();
        let unlocked = vault_handle.generation();
        let other = tempfile::tempdir().unwrap();
        let foreign = vault(other.path());
        foreign.create(&password("other password")).unwrap();

        // Act
        fs::copy(other.path().join(VAULT_FILE), home.path().join(VAULT_FILE)).unwrap();
        let replaced = vault_handle.get("a");

        // Assert
        assert!(matches!(replaced, Err(CredentialError::Locked)));
        assert!(vault_handle.generation() > unlocked);
        fs::remove_file(home.path().join(VAULT_FILE)).unwrap();
        assert_eq!(vault_handle.status().unwrap(), VaultStatus::Absent);
        assert!(matches!(vault_handle.get("a"), Ok(None)));
    }

    #[test]
    fn should_keep_every_entry_when_written_concurrently() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let vault = vault(home.path());
        vault.create(&password("correct horse")).unwrap();
        let barrier = Arc::new(Barrier::new(8));

        // Act
        let writers = (0..8)
            .map(|index| {
                let vault = vault.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    barrier.wait();
                    vault
                        .set(&format!("key-{index}"), &secret(&format!("value-{index}")))
                        .unwrap();
                })
            })
            .collect::<Vec<_>>();
        writers
            .into_iter()
            .for_each(|writer| writer.join().unwrap());

        // Assert
        let restarted = CredentialVault::new(home.path().to_path_buf(), FAST);
        restarted.unlock(&password("correct horse")).unwrap();
        for index in 0..8 {
            assert_eq!(
                stored(&restarted, &format!("key-{index}")),
                Some(format!("value-{index}"))
            );
        }
        let leftovers = fs::read_dir(home.path())
            .unwrap()
            .filter(|entry| {
                entry
                    .as_ref()
                    .unwrap()
                    .file_name()
                    .to_string_lossy()
                    .starts_with(PENDING_PREFIX)
            })
            .count();
        assert_eq!(leftovers, 0);
    }

    #[test]
    fn should_forget_values_on_lock_and_remove_only_the_vault_on_reset() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        fs::create_dir(home.path().join("workspaces")).unwrap();
        fs::write(home.path().join("workspaces/kept"), "data").unwrap();
        fs::write(home.path().join(format!("{PENDING_PREFIX}interrupted")), "").unwrap();
        let vault = vault(home.path());
        vault.create(&password("correct horse")).unwrap();
        vault.set("a", &secret("value")).unwrap();
        let before_lock = vault.generation();

        // Act
        vault.lock().unwrap();
        let locked = vault.get("a");
        vault.reset().unwrap();

        // Assert
        assert!(matches!(locked, Err(CredentialError::Locked)));
        assert!(vault.generation() > before_lock);
        assert_eq!(vault.status().unwrap(), VaultStatus::Absent);
        assert!(matches!(vault.get("a"), Ok(None)));
        assert_eq!(vault.remove("a"), Ok(()));
        assert_eq!(
            vault.set("a", &secret("value")),
            Err(CredentialError::Locked)
        );
        assert_eq!(
            fs::read_to_string(home.path().join("workspaces/kept")).unwrap(),
            "data"
        );
        assert_eq!(fs::read_dir(home.path()).unwrap().count(), 1);
    }

    #[test]
    fn should_refuse_a_linked_vault_file() {
        // Arrange
        let home = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        vault(elsewhere.path())
            .create(&password("correct horse"))
            .unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(
            elsewhere.path().join(VAULT_FILE),
            home.path().join(VAULT_FILE),
        )
        .unwrap();
        #[cfg(not(unix))]
        return;

        // Act
        let result = vault(home.path()).unlock(&password("correct horse"));

        // Assert
        assert!(matches!(result, Err(VaultError::Damaged)));
    }
}
