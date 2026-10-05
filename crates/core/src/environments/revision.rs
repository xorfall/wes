use super::*;
use sha2::{Digest, Sha256};
use std::str::FromStr;

/// Lowercase SHA-256 of the versioned canonical closure, not runtime identity or authorization.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Revision([u8; 32]);
impl Revision {
    /// Versioned, length-delimited evidence digest. The captured evidence remains owned by its
    /// original journal boundary; a namespace need not duplicate importer payloads to identify it.
    pub fn evidence<'a>(domain: &str, parts: impl IntoIterator<Item = &'a str>) -> Self {
        let mut fingerprint = Fingerprint::new(domain);
        for part in parts {
            fingerprint.text(part);
        }
        fingerprint.finish().0
    }
}
impl fmt::Display for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}
impl fmt::Debug for Revision {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl FromStr for Revision {
    type Err = EnvironmentError;
    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let invalid = || {
            error(
                "ENV001",
                "revision requires sha256: followed by 64 lowercase hexadecimal digits",
            )
        };
        let s = value.strip_prefix("sha256:").ok_or_else(invalid)?;
        if s.len() != 64
            || !s
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        let mut bytes = [0; 32];
        for (i, byte) in bytes.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).map_err(|_| invalid())?;
        }
        Ok(Self(bytes))
    }
}

/// Every string is length-prefixed (u64 BE), maps are sorted, and options/variants are tagged.
/// Versioned domain separation makes future canonicalization changes explicit.
pub(super) struct Fingerprint {
    hash: Sha256,
    bytes: usize,
}
impl Fingerprint {
    pub fn new(domain: &str) -> Self {
        let mut f = Self {
            hash: Sha256::new(),
            bytes: 0,
        };
        f.text(domain);
        f
    }
    pub fn text(&mut self, text: &str) {
        self.number(text.len());
        self.hash.update(text.as_bytes());
        self.bytes += text.len();
    }
    pub fn number(&mut self, value: usize) {
        self.hash.update((value as u64).to_be_bytes());
        self.bytes += 8;
    }
    pub fn flag(&mut self, value: bool) {
        self.hash.update([u8::from(value)]);
        self.bytes += 1;
    }
    pub fn revision(&mut self, value: Revision) {
        self.hash.update(value.0);
        self.bytes += 32;
    }
    pub fn value(&mut self, value: &ConfigValue) {
        match value {
            ConfigValue::Text(s) => {
                self.text("Text");
                self.text(s);
            }
            ConfigValue::Int(i) => {
                self.text("Int");
                self.hash.update(i.to_be_bytes());
                self.bytes += 8;
            }
            ConfigValue::Bool(b) => {
                self.text("Bool");
                self.flag(*b);
            }
        }
    }
    pub fn kind(&mut self, kind: ConfigType) {
        self.text(match kind {
            ConfigType::Text => "Text",
            ConfigType::Int => "Int",
            ConfigType::Bool => "Bool",
        });
    }
    pub fn setting(&mut self, setting: &Option<Setting>) {
        self.flag(setting.is_some());
        if let Some(s) = setting {
            match s {
                Setting::Literal(v) => {
                    self.text("literal");
                    self.value(v);
                }
                Setting::Config(k) => {
                    self.text("config");
                    self.text(k);
                }
            }
        }
    }
    pub fn auth(&mut self, auth: &std::collections::BTreeMap<String, Vec<String>>) {
        if !auth.is_empty() {
            self.text("http-auth-selection/v1");
            self.number(auth.len());
            for (operation, schemes) in auth {
                self.text(operation);
                self.number(schemes.len());
                for scheme in schemes {
                    self.text(scheme);
                }
            }
        }
    }
    pub fn import(&mut self, i: &ImportDefinition) {
        self.auth(&i.auth);
        if let Some(transport) = &i.transport {
            self.text("http-transport/v1");
            self.text(transport);
        }
        match i.binding_mode {
            BindingMode::Explicit => (),
            BindingMode::Automatic => self.text("automatic-binding/v1"),
            BindingMode::Unbound => self.text("unbound-provider/v1"),
        }
        if let Some(hash) = &i.source_sha256 {
            self.text("source-sha256/v1");
            self.text(hash);
        }
        if i.private_output {
            self.text("private-output/v1");
        }
        self.text(i.source.kind());
        self.text(i.source.location());
        self.text(&i.target);
        self.setting(&i.endpoint);
        self.setting(&i.timeout_ms);
        self.number(i.credentials.len());
        for (k, v) in &i.credentials {
            self.text(k);
            self.text(v);
        }
    }
    pub fn finish(self) -> (Revision, usize) {
        (Revision(self.hash.finalize().into()), self.bytes)
    }
}

pub(super) fn definition(name: &str, d: &Definition) -> Revision {
    let mut f = Fingerprint::new("wes/environment/declaration/v1");
    if d.owner.is_some() || d.drift {
        f.text("ownership/v1");
        f.flag(d.owner.is_some());
        if let Some(owner) = &d.owner {
            f.text(owner);
        }
        f.flag(d.drift);
    }
    if d.id.is_some() || d.retired || d.protected {
        f.text("identity-policy/v1");
        f.flag(d.id.is_some());
        if let Some(id) = &d.id {
            f.text(id);
        }
        f.flag(d.retired);
        f.flag(d.protected);
    }
    f.text(name);
    f.flag(d.abstract_environment);
    match &d.parent {
        None => f.text("none"),
        Some(Parent::Latest(n)) => {
            f.text("latest");
            f.text(n);
        }
        Some(Parent::Pinned { name, revision }) => {
            f.text("pinned");
            f.text(name);
            f.revision(*revision);
        }
    }
    f.number(d.parameters.len());
    for (key, p) in &d.parameters {
        f.text(key);
        f.kind(p.kind);
        f.flag(p.default.is_some());
        if let Some(v) = &p.default {
            f.value(v);
        }
    }
    f.number(d.config.len());
    for (k, v) in &d.config {
        f.text(k);
        f.value(v);
    }
    f.number(d.secret_slots.len());
    for (k, v) in &d.secret_slots {
        f.text(k);
        f.flag(*v);
    }
    f.number(d.secret_refs.len());
    for (k, v) in &d.secret_refs {
        f.text(k);
        f.text(v);
    }
    for m in [&d.imports, &d.overrides] {
        f.number(m.len());
        for (k, v) in m {
            f.text(k);
            f.import(v);
        }
    }
    f.number(d.hidden.len());
    for k in &d.hidden {
        f.text(k);
    }
    f.finish().0
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fmt::Write;
    #[test]
    fn sha256_dependency_matches_known_vector() {
        let hash = Sha256::digest(b"abc");
        let mut text = String::new();
        for byte in hash {
            write!(text, "{byte:02x}").unwrap();
        }
        assert_eq!(
            text,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }
}
