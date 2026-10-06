//! Inert environment definitions and immutable resolved closures. No filesystem, execution or secrets.
mod package;
mod resolve;
mod revision;

pub use resolve::{Binding, EffectiveEnvironment, EffectiveImport, ImportOrigin};
pub use revision::Revision;
use std::{collections::BTreeMap, fmt, sync::Arc};
use thiserror::Error;

pub const MAX_ENVIRONMENTS: usize = 128;
pub const MAX_TARGETS: usize = 64;
pub const MAX_IMPORTS: usize = 256;
pub const MAX_DEPTH: usize = 32;
pub const MAX_SOURCE_BYTES: usize = 256 * 1024;
pub const MAX_CAPTURE_BYTES: usize = 8 * 1024 * 1024;

/// Client acknowledgement of a definition image, not execution or credential authority.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct EnvironmentContext {
    pub selected: Option<String>,
    pub revisions: BTreeMap<String, Revision>,
}
impl EnvironmentContext {
    pub fn validate(&self) -> Result<(), EnvironmentError> {
        if self.revisions.len() > MAX_ENVIRONMENTS
            || self.revisions.keys().any(|n| !name(n))
            || self
                .selected
                .as_ref()
                .is_some_and(|n| !self.revisions.contains_key(n))
        {
            return Err(error("ENV008", "invalid environment acknowledgement"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Error, PartialEq, Eq)]
#[error("{code}: {message}")]
pub struct EnvironmentError {
    pub code: &'static str,
    pub message: String,
}
pub(crate) fn error(code: &'static str, message: impl Into<String>) -> EnvironmentError {
    EnvironmentError {
        code,
        message: message.into(),
    }
}

/// Initial schema deliberately supports only scalar configuration; arbitrary objects are not merged.
#[derive(Clone, PartialEq, Eq)]
pub enum ConfigValue {
    Text(String),
    Int(i64),
    Bool(bool),
}
impl fmt::Debug for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Text(_) => "Text(..)",
            Self::Int(_) => "Int(..)",
            Self::Bool(_) => "Bool(..)",
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfigType {
    Text,
    Int,
    Bool,
}
impl ConfigType {
    pub fn accepts(self, value: &ConfigValue) -> bool {
        matches!(
            (self, value),
            (Self::Text, ConfigValue::Text(_))
                | (Self::Int, ConfigValue::Int(_))
                | (Self::Bool, ConfigValue::Bool(_))
        )
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    pub kind: ConfigType,
    pub default: Option<ConfigValue>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Parent {
    Latest(String),
    Pinned { name: String, revision: Revision },
}
impl Parent {
    pub fn name(&self) -> &str {
        match self {
            Self::Latest(n) | Self::Pinned { name: n, .. } => n,
        }
    }
}

/// Inert configuration for compiled-in targets. Concrete launch behavior is adapter-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Target {
    pub(crate) kind: TargetKind,
    pub(crate) name: String,
    pub(crate) cwd: Option<String>,
    pub(crate) variables: BTreeMap<String, String>,
}
impl Target {
    /// A display-name change does not change the captured execution destination.
    pub fn same_execution(&self, other: &Self) -> bool {
        self.kind == other.kind && self.cwd == other.cwd && self.variables == other.variables
    }

    pub fn kind(&self) -> &TargetKind {
        &self.kind
    }
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }
    pub fn variables(&self) -> &BTreeMap<String, String> {
        &self.variables
    }
    pub fn name(&self) -> &str {
        &self.name
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TargetKind {
    Local,
    /// Existing container only. Container environment inheritance is explicitly acknowledged.
    Docker {
        socket: String,
        destination: DockerDestination,
        image: Option<String>,
        shell: Option<String>,
    },
    Ssh(SshTarget),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DockerDestination {
    Container(String),
    Compose {
        project: String,
        service: String,
        replica: Option<u32>,
    },
}

/// Explicit host-side client/key paths and a remote POSIX-shell execution contract.
/// Key contents and live host verification are never captured in this configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SshTarget {
    pub client: String,
    pub host: String,
    pub user: String,
    pub port: u16,
    pub identity_file: String,
    pub known_hosts: String,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SourceKey {
    kind: String,
    location: String,
}
impl SourceKey {
    pub fn new(kind: &str, location: &str) -> Result<Self, EnvironmentError> {
        if !matches!(kind, "spec" | "openapi" | "process" | "docker" | "builtin")
            || (kind == "builtin" && !matches!(location, "sh" | "http" | "docker"))
            || (kind == "docker" && !location.starts_with('/'))
            || location.is_empty()
            || location.len() > 4096
            || location.chars().any(char::is_control)
        {
            return Err(error("ENV001", "invalid import kind or source location"));
        }
        Ok(Self {
            kind: kind.into(),
            location: location.into(),
        })
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn location(&self) -> &str {
        &self.location
    }
    /// HTTP source locations keep their URL identity through capture and lock replay.
    pub fn source_field(&self) -> &'static str {
        if self.kind == "builtin" {
            "name"
        } else if self.kind == "docker" {
            "socket"
        } else if self.kind == "process" {
            "bin"
        } else if self.location.starts_with("http://") || self.location.starts_with("https://") {
            "url"
        } else {
            "file"
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Setting {
    Literal(ConfigValue),
    Config(String),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum BindingMode {
    Explicit,
    Automatic,
    Unbound,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportDefinition {
    /// HTTP implementation, independent of execution destination.
    pub transport: Option<String>,
    /// Operation path -> selected authentication scheme names (AND). Values are not credentials.
    pub auth: BTreeMap<String, Vec<String>>,
    pub binding_mode: BindingMode,
    pub source_sha256: Option<String>,
    pub private_output: bool,
    pub source: SourceKey,
    pub target: String,
    pub endpoint: Option<Setting>,
    pub timeout_ms: Option<Setting>,
    /// Adapter credential slot -> declared secret slot. These are references, not values or grants.
    pub credentials: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Definition {
    /// Execution destinations usable independently of provider imports.
    pub targets: std::collections::BTreeSet<String>,
    pub owner: Option<String>,
    pub drift: bool,
    pub id: Option<String>,
    pub retired: bool,
    pub protected: bool,
    pub abstract_environment: bool,
    pub parent: Option<Parent>,
    pub parameters: BTreeMap<String, Parameter>,
    pub config: BTreeMap<String, ConfigValue>,
    pub secret_slots: BTreeMap<String, bool>,
    pub secret_refs: BTreeMap<String, String>,
    pub imports: BTreeMap<String, ImportDefinition>,
    pub overrides: BTreeMap<String, ImportDefinition>,
    pub hidden: std::collections::BTreeSet<String>,
}

/// Immutable, validated input package. Descriptors are supplied separately as captured evidence.
#[derive(Clone)]
pub struct Package {
    package: Option<String>,
    definitions: BTreeMap<String, Definition>,
    targets: BTreeMap<String, Target>,
}
/// Captured configuration for an application-provided namespace entry.
#[derive(Clone, Debug)]
pub struct ConfiguredProvider {
    pub evidence: String,
    pub endpoint: Option<String>,
}
impl Package {
    /// A named namespace for configured providers, including captured importer products.
    /// Evidence participates in ordinary binding revisions; YAML cannot fabricate these handles.
    pub fn configured_providers(
        environment: &str,
        evidence: BTreeMap<String, ConfiguredProvider>,
    ) -> Result<(Self, CapturedSources), EnvironmentError> {
        if !name(environment) || evidence.is_empty() || evidence.len() > MAX_IMPORTS {
            return Err(error("ENV001", "invalid configured environment"));
        }
        let mut sources = CapturedSources::default();
        let mut imports = BTreeMap::new();
        for (alias, content) in evidence {
            if !name(&alias) {
                return Err(error("ENV001", "invalid built-in provider name"));
            }
            let source = SourceKey {
                kind: "configured".into(),
                location: alias.clone(),
            };
            sources.insert(
                source.clone(),
                CapturedSource::new("configured/v1", &content.evidence)?,
            )?;
            imports.insert(
                alias,
                ImportDefinition {
                    transport: None,
                    auth: BTreeMap::new(),
                    binding_mode: BindingMode::Explicit,
                    source_sha256: None,
                    private_output: false,
                    source,
                    target: "local".into(),
                    endpoint: content
                        .endpoint
                        .map(|value| Setting::Literal(ConfigValue::Text(value))),
                    timeout_ms: None,
                    credentials: BTreeMap::new(),
                },
            );
        }
        Ok((
            Self {
                package: None,
                definitions: BTreeMap::from([(
                    environment.into(),
                    Definition {
                        id: Some(format!("builtin.{environment}")),
                        protected: true,
                        imports,
                        ..Definition::default()
                    },
                )]),
                targets: BTreeMap::from([(
                    "local".into(),
                    Target {
                        kind: TargetKind::Local,
                        name: "local".into(),
                        cwd: None,
                        variables: BTreeMap::new(),
                    },
                )]),
            },
            sources,
        ))
    }
    pub fn package_name(&self) -> Option<&str> {
        self.package.as_deref()
    }
    pub fn parse(yaml: &str) -> Result<Self, EnvironmentError> {
        package::parse(yaml)
    }
    pub fn definitions(&self) -> &BTreeMap<String, Definition> {
        &self.definitions
    }
    pub fn targets(&self) -> &BTreeMap<String, Target> {
        &self.targets
    }
    pub fn required_sources(&self) -> std::collections::BTreeSet<SourceKey> {
        self.definitions
            .values()
            .flat_map(|d| {
                d.imports
                    .values()
                    .chain(d.overrides.values())
                    .map(|i| i.source.clone())
            })
            .collect()
    }
}
impl fmt::Debug for Package {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentPackage")
            .field("environments", &self.definitions.len())
            .field("targets", &self.targets.len())
            .finish_non_exhaustive()
    }
}

/// Captured importer input, not evidence that an adapter supports its bindings or execution is allowed.
#[derive(Clone, PartialEq, Eq)]
pub struct CapturedSource {
    format: String,
    bytes: Arc<str>,
}
impl CapturedSource {
    pub fn new(format: &str, bytes: &str) -> Result<Self, EnvironmentError> {
        if format.is_empty()
            || format.len() > 128
            || format.chars().any(char::is_control)
            || bytes.len() > MAX_SOURCE_BYTES
        {
            return Err(error(
                "ENV006",
                "invalid or oversized captured importer evidence",
            ));
        }
        Ok(Self {
            format: format.into(),
            bytes: bytes.into(),
        })
    }
    pub fn format(&self) -> &str {
        &self.format
    }
    pub fn bytes(&self) -> &str {
        &self.bytes
    }
    pub fn shared_bytes(&self) -> Arc<str> {
        self.bytes.clone()
    }
    pub fn charge(&self) -> usize {
        self.format.len() + self.bytes.len()
    }
}
impl fmt::Debug for CapturedSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedSource")
            .field("bytes", &self.bytes.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Default, PartialEq, Eq)]
pub struct CapturedSources {
    entries: BTreeMap<SourceKey, Arc<CapturedSource>>,
    bytes: usize,
}
impl CapturedSources {
    pub fn iter(&self) -> impl Iterator<Item = (&SourceKey, &Arc<CapturedSource>)> {
        self.entries.iter()
    }
    pub fn insert(
        &mut self,
        key: SourceKey,
        source: CapturedSource,
    ) -> Result<(), EnvironmentError> {
        if let Some(previous) = self.entries.get(&key) {
            return if previous.as_ref() == &source {
                Ok(())
            } else {
                Err(error(
                    "ENV006",
                    "conflicting captured evidence for one source",
                ))
            };
        }
        let charge = key.kind.len() + key.location.len() + source.charge();
        if self.entries.len() >= 1024 || charge > MAX_CAPTURE_BYTES.saturating_sub(self.bytes) {
            return Err(error("ENV007", "captured evidence budget exceeded"));
        }
        self.entries.insert(key, Arc::new(source));
        self.bytes += charge;
        Ok(())
    }
    pub fn validate(&self, package: &Package) -> Result<(), EnvironmentError> {
        let required = package.required_sources();
        if !required.iter().eq(self.entries.keys()) {
            return Err(error(
                "ENV006",
                "captured sources must exactly match declared import sources",
            ));
        }
        Ok(())
    }
    pub fn get(&self, key: &SourceKey) -> Option<&Arc<CapturedSource>> {
        self.entries.get(key)
    }
    pub fn charge(&self) -> usize {
        self.bytes
    }
}
impl fmt::Debug for CapturedSources {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedSources")
            .field("entries", &self.entries.len())
            .field("bytes", &self.bytes)
            .finish()
    }
}

pub(crate) fn name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphanumeric() || (i > 0 && matches!(b, b'_' | b'-' | b'.')))
}
