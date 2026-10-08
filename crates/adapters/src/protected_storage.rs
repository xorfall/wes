//! Authenticated, home-bound object bytes. Keys are supplied by the application owner.
//! Encryption is neither read authority nor permission to export the decoded contents.
use chacha20poly1305::{
    KeyInit, XChaCha20Poly1305, XNonce,
    aead::{Aead, Payload},
};
use std::{io, sync::Arc};
use zeroize::Zeroizing;

const MAGIC: &[u8; 8] = b"WESENC01";
const NONCE: usize = 24;
pub const OVERHEAD: usize = MAGIC.len() + NONCE + 16;

/// A startup-scoped key lease. Locking requires joining all owners of this lease.
/// Debug and serialization deliberately have no access to its material.
pub struct ObjectProtection {
    home: String,
    key: Zeroizing<[u8; 32]>,
}
impl ObjectProtection {
    pub fn new(home: &str, key: [u8; 32]) -> io::Result<Arc<Self>> {
        if !uuid::Uuid::parse_str(home).is_ok_and(|id| id.to_string() == home) {
            return Err(invalid());
        }
        Ok(Arc::new(Self {
            home: home.into(),
            key: Zeroizing::new(key),
        }))
    }
    fn associated(&self, domain: &str, object: &str) -> io::Result<Vec<u8>> {
        if domain.is_empty() || object.is_empty() || domain.len() > 256 || object.len() > 512 {
            return Err(invalid());
        }
        let mut aad = Vec::new();
        for part in ["wes/encrypted-object/v1", &self.home, domain, object] {
            aad.extend_from_slice(&(part.len() as u64).to_be_bytes());
            aad.extend_from_slice(part.as_bytes());
        }
        Ok(aad)
    }
    pub(crate) fn seal(&self, domain: &str, object: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
        let aad = self.associated(domain, object)?;
        let mut nonce = [0; NONCE];
        getrandom::fill(&mut nonce)
            .map_err(|_| io::Error::other("storage nonce is unavailable"))?;
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        let sealed = cipher
            .encrypt(
                &XNonce::from(nonce),
                Payload {
                    msg: bytes,
                    aad: &aad,
                },
            )
            .map_err(|_| invalid())?;
        let mut out = Vec::with_capacity(bytes.len() + OVERHEAD);
        out.extend_from_slice(MAGIC);
        out.extend_from_slice(&nonce);
        out.extend_from_slice(&sealed);
        Ok(out)
    }
    pub(crate) fn open(
        &self,
        domain: &str,
        object: &str,
        bytes: &[u8],
        limit: usize,
    ) -> io::Result<Zeroizing<Vec<u8>>> {
        if bytes.len() < OVERHEAD
            || bytes.len().saturating_sub(OVERHEAD) > limit
            || !bytes.starts_with(MAGIC)
        {
            return Err(invalid());
        }
        let aad = self.associated(domain, object)?;
        let cipher = XChaCha20Poly1305::new((&*self.key).into());
        cipher
            .decrypt(
                &XNonce::try_from(&bytes[MAGIC.len()..MAGIC.len() + NONCE])
                    .map_err(|_| invalid())?,
                Payload {
                    msg: &bytes[MAGIC.len() + NONCE..],
                    aad: &aad,
                },
            )
            .map(Zeroizing::new)
            .map_err(|_| invalid())
    }
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "encrypted storage is unavailable, corrupt or belongs to another owner",
    )
}

/// An immutable proof binds a directory's selected mode to its original home key.
/// Missing keys and attempts to change an initialized directory's mode fail closed.
pub(crate) fn check_mode(
    dir: &cap_std::fs::Dir,
    domain: &str,
    protection: Option<&ObjectProtection>,
) -> io::Result<()> {
    use std::io::{Read, Write};
    const NAME: &str = ".wes-storage-protection";
    const PROOF: &[u8] = b"wes.storage-protection/v1";
    let mut read = crate::filesystem::private_options();
    read.read(true);
    match dir.open_with(NAME, &read) {
        Ok(file) => {
            if !file.metadata()?.is_file() || file.metadata()?.len() > 256 {
                return Err(invalid());
            }
            let mut bytes = Vec::new();
            file.take(257).read_to_end(&mut bytes)?;
            match protection {
                Some(p) if p.open(domain, "mode", &bytes, 64)?.as_slice() == PROOF => Ok(()),
                None if bytes == b"ordinary\n" => Ok(()),
                _ => Err(invalid()),
            }
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            // Old ordinary directories may acquire an ordinary marker. Encrypted mode
            // never reinterprets or silently converts an existing plaintext object.
            if protection.is_some() {
                for entry in dir.entries()? {
                    let name = entry?.file_name();
                    let name = name.to_str().ok_or_else(invalid)?;
                    if !name.starts_with(".wes-") {
                        return Err(invalid());
                    }
                }
            }
            let bytes = match protection {
                Some(p) => p.seal(domain, "mode", PROOF)?,
                None => b"ordinary\n".to_vec(),
            };
            let mut write = crate::filesystem::private_options();
            write.write(true).create_new(true);
            let mut file = dir.open_with(NAME, &write)?;
            file.write_all(&bytes)?;
            file.sync_all()
        }
        Err(e) => Err(e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn append_frames_bind_order_and_keep_exact_torn_tail_boundary() {
        let p = ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [3; 32]).unwrap();
        let first = encode_log_frame(Some(&p), "catalog", 0, b"first").unwrap();
        let second = encode_log_frame(Some(&p), "catalog", first.len(), b"second").unwrap();
        let mut all = first.clone();
        all.extend_from_slice(&second);
        let log = decode_log(Some(&p), "catalog", all.clone(), 20, 4, 20).unwrap();
        assert_eq!(log.bytes, b"firstsecond");
        assert_eq!(log.boundary(5).unwrap(), first.len());
        assert_eq!(log.boundary(11).unwrap(), all.len());
        assert!(!log.torn);
        all.pop();
        let torn = decode_log(Some(&p), "catalog", all, 20, 4, 20).unwrap();
        assert!(torn.torn);
        assert_eq!(torn.bytes, b"first");
        assert_eq!(torn.boundary(5).unwrap(), first.len());
        let mut tampered = first.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert!(decode_log(Some(&p), "catalog", tampered, 20, 4, 20).is_err());
        let mut swapped = second;
        swapped.extend_from_slice(&first);
        assert!(decode_log(Some(&p), "catalog", swapped, 20, 4, 20).is_err());
        assert!(decode_log(Some(&p), "catalog", first, 4, 4, 20).is_err());
    }
    #[test]
    fn identity_key_tamper_and_bounds_are_authenticated() {
        let home = uuid::Uuid::new_v4().to_string();
        let p = ObjectProtection::new(&home, [7; 32]).unwrap();
        let bytes = p
            .seal("values", "object", b"confidential sentinel")
            .unwrap();
        assert_eq!(
            p.open("values", "object", &bytes, 100).unwrap().as_slice(),
            b"confidential sentinel"
        );
        assert!(p.open("dataset", "object", &bytes, 100).is_err());
        assert!(p.open("values", "other", &bytes, 100).is_err());
        assert!(
            ObjectProtection::new(&home, [8; 32])
                .unwrap()
                .open("values", "object", &bytes, 100)
                .is_err()
        );
        assert!(
            ObjectProtection::new(&uuid::Uuid::new_v4().to_string(), [7; 32])
                .unwrap()
                .open("values", "object", &bytes, 100)
                .is_err()
        );
        assert!(p.open("values", "object", &bytes, 3).is_err());
        for at in [0, 8, 31, bytes.len() - 1] {
            let mut tampered = bytes.clone();
            tampered[at] ^= 1;
            assert!(p.open("values", "object", &tampered, 100).is_err());
        }
        for length in 0..bytes.len() {
            assert!(p.open("values", "object", &bytes[..length], 100).is_err());
        }
        assert_ne!(
            bytes,
            p.seal("values", "object", b"confidential sentinel")
                .unwrap()
        );
        assert!(!bytes.windows(12).any(|w| w == b"confidential"));
    }
}

/// Logical bytes and exact physical boundaries of an authenticated append log.
/// Recovery may discard an incomplete outer frame, never an authentication failure.
pub(crate) struct DecodedLog {
    pub bytes: Vec<u8>,
    pub physical: usize,
    pub torn: bool,
    boundaries: Option<std::collections::BTreeMap<usize, usize>>,
}
impl DecodedLog {
    pub fn boundary(&self, logical: usize) -> io::Result<usize> {
        match &self.boundaries {
            None => Ok(logical),
            Some(map) => map.get(&logical).copied().ok_or_else(invalid),
        }
    }
}
pub(crate) fn encode_log_frame(
    protection: Option<&ObjectProtection>,
    domain: &str,
    offset: usize,
    bytes: &[u8],
) -> io::Result<Vec<u8>> {
    let Some(p) = protection else {
        return Ok(bytes.to_vec());
    };
    let sealed = p.seal(domain, &offset.to_string(), bytes)?;
    let length = u32::try_from(sealed.len()).map_err(|_| invalid())?;
    let mut frame = length.to_le_bytes().to_vec();
    frame.extend_from_slice(&sealed);
    Ok(frame)
}
pub(crate) fn decode_log(
    protection: Option<&ObjectProtection>,
    domain: &str,
    physical: Vec<u8>,
    byte_limit: usize,
    frame_limit: usize,
    frame_bytes: usize,
) -> io::Result<DecodedLog> {
    let Some(p) = protection else {
        return Ok(DecodedLog {
            physical: physical.len(),
            bytes: physical,
            torn: false,
            boundaries: None,
        });
    };
    let mut bytes = Vec::new();
    let mut at = 0usize;
    let mut torn = false;
    let mut boundaries = std::collections::BTreeMap::from([(0, 0)]);
    for _ in 0..frame_limit {
        if at == physical.len() {
            break;
        }
        if physical.len() - at < 4 {
            torn = true;
            break;
        }
        let length =
            u32::from_le_bytes(physical[at..at + 4].try_into().map_err(|_| invalid())?) as usize;
        if length < OVERHEAD || length.saturating_sub(OVERHEAD) > frame_bytes {
            return Err(invalid());
        }
        let end = at
            .checked_add(4)
            .and_then(|n| n.checked_add(length))
            .ok_or_else(invalid)?;
        if end > physical.len() {
            torn = true;
            break;
        }
        let decoded = p.open(domain, &at.to_string(), &physical[at + 4..end], frame_bytes)?;
        if bytes
            .len()
            .checked_add(decoded.len())
            .is_none_or(|n| n > byte_limit)
        {
            return Err(invalid());
        }
        bytes.extend_from_slice(&decoded);
        at = end;
        boundaries.insert(bytes.len(), at);
    }
    if !torn && at != physical.len() {
        return Err(invalid());
    }
    Ok(DecodedLog {
        bytes,
        physical: physical.len(),
        torn,
        boundaries: Some(boundaries),
    })
}
