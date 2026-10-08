use super::*;
use revision::Fingerprint;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportOrigin {
    pub environment: String,
    pub declaration: Revision,
}

/// Both the reusable recipe and this consumer's resolved slots are captured immutably.
#[derive(Clone, PartialEq, Eq)]
pub struct EffectiveImport {
    declaration: Arc<ImportDefinition>,
    origin: ImportOrigin,
    source: Arc<CapturedSource>,
    target: Arc<Target>,
    endpoint: Option<String>,
    timeout_ms: Option<i64>,
    credentials: BTreeMap<String, String>,
}
impl EffectiveImport {
    /// Resolved execution contract, excluding unrelated environment revision bookkeeping.
    pub fn same_execution(&self, other: &Self) -> bool {
        self.declaration.transport == other.declaration.transport
            && self.declaration.auth == other.declaration.auth
            && self.declaration.output_policy == other.declaration.output_policy
            && self.source == other.source
            && self.target.same_execution(&other.target)
            && self.endpoint == other.endpoint
            && self.timeout_ms == other.timeout_ms
            && self.credentials == other.credentials
    }

    pub fn declaration(&self) -> &ImportDefinition {
        &self.declaration
    }
    pub fn origin(&self) -> &ImportOrigin {
        &self.origin
    }
    pub fn source(&self) -> &CapturedSource {
        &self.source
    }
    pub fn target(&self) -> &Target {
        &self.target
    }
    pub fn endpoint(&self) -> Option<&str> {
        self.endpoint.as_deref()
    }
    pub fn timeout_ms(&self) -> Option<i64> {
        self.timeout_ms
    }
    pub fn credential_refs(&self) -> &BTreeMap<String, String> {
        &self.credentials
    }
}
impl fmt::Debug for EffectiveImport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectiveImport")
            .field("origin", &self.origin)
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
pub struct EffectiveEnvironment {
    attached_targets: BTreeMap<String, Arc<Target>>,
    id: String,
    retired: bool,
    protected: bool,
    owner: Option<String>,
    drift: bool,
    name: String,
    revision: Revision,
    parent: Option<Arc<EffectiveEnvironment>>,
    abstract_environment: bool,
    parameters: BTreeMap<String, Parameter>,
    config: BTreeMap<String, ConfigValue>,
    secret_slots: BTreeMap<String, bool>,
    secret_refs: BTreeMap<String, String>,
    imports: BTreeMap<String, Arc<EffectiveImport>>,
    depth: usize,
    charge: usize,
}
impl fmt::Debug for EffectiveEnvironment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EffectiveEnvironment")
            .field("name", &self.name)
            .field("revision", &self.revision)
            .field("abstract", &self.abstract_environment)
            .field("imports", &self.imports.len())
            .finish_non_exhaustive()
    }
}
impl EffectiveEnvironment {
    /// Ownership/drift bookkeeping may be reconciled without reopening a retired execution bundle.
    pub fn same_execution(&self, other: &Self) -> bool {
        self.attached_targets == other.attached_targets
            && self.id == other.id
            && self.name == other.name
            && self.retired == other.retired
            && self.protected == other.protected
            && self.abstract_environment == other.abstract_environment
            && self.parameters == other.parameters
            && self.config == other.config
            && self.secret_slots == other.secret_slots
            && self.secret_refs == other.secret_refs
            && self.imports.len() == other.imports.len()
            && self.imports.iter().all(|(name, a)| {
                other.imports.get(name).is_some_and(|b| {
                    a.declaration == b.declaration
                        && a.source == b.source
                        && a.target == b.target
                        && a.endpoint == b.endpoint
                        && a.timeout_ms == b.timeout_ms
                        && a.credentials == b.credentials
                })
            })
    }
    pub fn identity(&self) -> &str {
        &self.id
    }
    pub fn owner(&self) -> Option<&str> {
        self.owner.as_deref()
    }
    pub fn has_drift(&self) -> bool {
        self.drift
    }
    pub fn is_retired(&self) -> bool {
        self.retired
    }
    pub fn is_protected(&self) -> bool {
        self.protected
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn revision(&self) -> Revision {
        self.revision
    }
    pub fn parent(&self) -> Option<&Arc<Self>> {
        self.parent.as_ref()
    }
    pub fn is_abstract(&self) -> bool {
        self.abstract_environment
    }
    /// Captured target vocabulary, including explicit attachments and provider destinations.
    /// A name with conflicting historical configurations cannot be selected implicitly.
    pub fn execution_targets(&self) -> BTreeMap<String, Result<Arc<Target>, EnvironmentError>> {
        let mut targets: BTreeMap<_, _> = self
            .attached_targets
            .iter()
            .map(|(k, v)| (k.clone(), Ok(v.clone())))
            .collect();
        for import in self.imports.values() {
            let name = import.target.name().to_owned();
            match targets.get(&name) {
                Some(Ok(existing)) if existing != &import.target => {
                    targets.insert(
                        name,
                        Err(error(
                            "ENV003",
                            "target name refers to different captured configurations",
                        )),
                    );
                }
                None => {
                    targets.insert(name, Ok(import.target.clone()));
                }
                _ => {}
            }
        }
        targets
    }
    pub fn imports(&self) -> &BTreeMap<String, Arc<EffectiveImport>> {
        &self.imports
    }
    pub fn config(&self) -> &BTreeMap<String, ConfigValue> {
        &self.config
    }
    pub fn secret_refs(&self) -> &BTreeMap<String, String> {
        &self.secret_refs
    }
    /// Conservative logical bytes including the pinned parent closure, not allocator/RSS size.
    pub fn charge(&self) -> usize {
        self.charge
    }

    /// Pure resolution. Caller supplies the parent selected by current-registry or pin lookup.
    /// Source capture/build and dispatch authorization are distinct, later boundaries.
    pub fn resolve(
        package: &Package,
        name: &str,
        parent: Option<Arc<Self>>,
        sources: &CapturedSources,
    ) -> Result<Self, EnvironmentError> {
        let d = package
            .definitions
            .get(name)
            .ok_or_else(|| error("ENV002", "environment definition does not exist"))?;
        match (&d.parent, &parent) {
            (None, None) => {}
            (Some(Parent::Latest(n)), Some(p)) if p.name() == n => {}
            (Some(Parent::Pinned { name, revision }), Some(p))
                if p.name() == name && p.revision() == *revision => {}
            _ => {
                return Err(error(
                    "ENV002",
                    "resolved parent does not match the declared reference",
                ));
            }
        }
        if parent
            .as_ref()
            .is_some_and(|p| p.name == name || p.depth >= MAX_DEPTH)
        {
            return Err(error(
                "ENV002",
                "environment inheritance is cyclic or exceeds its depth limit",
            ));
        }
        let mut ancestor = parent.as_ref();
        while let Some(p) = ancestor {
            if p.name() == name {
                return Err(error(
                    "ENV002",
                    "a pinned ancestor cannot refer back to its descendant",
                ));
            }
            ancestor = p.parent();
        }
        let mut attached_targets = parent
            .as_ref()
            .map(|p| p.attached_targets.clone())
            .unwrap_or_default();
        for key in &d.targets {
            let target = package
                .targets()
                .get(key)
                .ok_or_else(|| error("ENV003", format!("undefined execution target: {key}")))?;
            if attached_targets
                .get(key)
                .is_some_and(|old| old.as_ref() != target)
            {
                return Err(error(
                    "ENV003",
                    "an attached target cannot silently replace an inherited destination; use a distinct target name",
                ));
            }
            attached_targets.insert(key.clone(), Arc::new(target.clone()));
        }
        if attached_targets.len() > MAX_TARGETS {
            return Err(error("ENV007", "inherited targets exceed their budget"));
        }
        let mut parameters = parent
            .as_ref()
            .map(|p| p.parameters.clone())
            .unwrap_or_default();
        let mut config = parent
            .as_ref()
            .map(|p| p.config.clone())
            .unwrap_or_default();
        let mut secret_slots = parent
            .as_ref()
            .map(|p| p.secret_slots.clone())
            .unwrap_or_default();
        for (key, parameter) in &d.parameters {
            if parameters.get(key).is_some_and(|p| p != parameter) {
                return Err(error(
                    "ENV003",
                    format!("incompatible inherited parameter: {key}"),
                ));
            }
            parameters.insert(key.clone(), parameter.clone());
        }
        for (key, required) in &d.secret_slots {
            if secret_slots.get(key).is_some_and(|old| *old && !required) {
                return Err(error(
                    "ENV003",
                    "a child cannot weaken a required secret slot",
                ));
            }
            secret_slots.insert(key.clone(), *required);
        }
        if parameters.len() > MAX_IMPORTS || secret_slots.len() > MAX_IMPORTS {
            return Err(error(
                "ENV007",
                "inherited configuration schema exceeds its budget",
            ));
        }
        for (key, parameter) in &parameters {
            if !config.contains_key(key)
                && let Some(default) = &parameter.default
            {
                config.insert(key.clone(), default.clone());
            }
        }
        for (key, value) in &d.config {
            let parameter = parameters
                .get(key)
                .ok_or_else(|| error("ENV003", format!("undeclared configuration slot: {key}")))?;
            if !parameter.kind.accepts(value) {
                return Err(error(
                    "ENV003",
                    format!("configuration type mismatch: {key}"),
                ));
            }
            config.insert(key.clone(), value.clone());
        }
        if !d.abstract_environment && parameters.keys().any(|key| !config.contains_key(key)) {
            return Err(error(
                "ENV003",
                "runnable environment has missing configuration",
            ));
        }
        // Assignments intentionally do not inherit, even from a runnable parent.
        let secret_refs = d.secret_refs.clone();
        if secret_refs
            .keys()
            .any(|key| !secret_slots.contains_key(key))
        {
            return Err(error(
                "ENV003",
                "assignment names an undeclared secret slot",
            ));
        }
        if !d.abstract_environment
            && secret_slots
                .iter()
                .any(|(key, required)| *required && !secret_refs.contains_key(key))
        {
            return Err(error(
                "ENV003",
                "runnable environment has missing secret references",
            ));
        }

        let mut templates = parent
            .as_ref()
            .map(|p| p.imports.clone())
            .unwrap_or_default();
        for key in &d.hidden {
            if templates.remove(key).is_none() {
                return Err(error(
                    "ENV004",
                    format!("cannot hide an absent inherited import: {key}"),
                ));
            }
        }
        let origin = ImportOrigin {
            environment: name.into(),
            declaration: revision::definition(name, d),
        };
        for (key, definition) in d.imports.iter().chain(d.overrides.iter()) {
            if templates.contains_key(key) != d.overrides.contains_key(key) {
                return Err(error(
                    "ENV004",
                    format!(
                        "import requires explicit override of an existing inherited alias: {key}"
                    ),
                ));
            }
            let source = sources
                .get(&definition.source)
                .ok_or_else(|| error("ENV006", "import lacks captured source evidence"))?
                .clone();
            if let Some(expected) = &definition.source_sha256 {
                use sha2::{Digest, Sha256};
                if format!("{:x}", Sha256::digest(source.bytes().as_bytes())) != *expected {
                    return Err(error(
                        "ENV006",
                        "captured descriptor does not match pinned source sha256",
                    ));
                }
            }
            let target = if definition.binding_mode != BindingMode::Explicit {
                Target {
                    kind: TargetKind::Local,
                    name: "local".into(),
                    cwd: None,
                    variables: BTreeMap::new(),
                }
            } else {
                package
                    .targets
                    .get(&definition.target)
                    .ok_or_else(|| error("ENV003", "import references an undeclared target"))?
                    .clone()
            };
            templates.insert(
                key.clone(),
                Arc::new(EffectiveImport {
                    declaration: Arc::new(definition.clone()),
                    origin: origin.clone(),
                    source,
                    target: Arc::new(target),
                    endpoint: None,
                    timeout_ms: None,
                    credentials: BTreeMap::new(),
                }),
            );
        }
        if templates.len() > MAX_IMPORTS {
            return Err(error("ENV007", "effective import count exceeds its budget"));
        }
        let mut imports = BTreeMap::new();
        for (key, template) in templates {
            let mut import = template.as_ref().clone();
            let endpoint = resolve_setting(
                &import.declaration.endpoint,
                ConfigType::Text,
                &parameters,
                &config,
                d.abstract_environment,
            )?;
            import.endpoint = match endpoint {
                Some(ConfigValue::Text(s)) => Some(s),
                None => None,
                _ => unreachable!("checked setting"),
            };
            if import
                .endpoint
                .as_ref()
                .is_some_and(|s| s.is_empty() || s.chars().any(char::is_control))
            {
                return Err(error(
                    "ENV003",
                    "endpoint cannot be empty or contain control characters",
                ));
            }
            let timeout = resolve_setting(
                &import.declaration.timeout_ms,
                ConfigType::Int,
                &parameters,
                &config,
                d.abstract_environment,
            )?;
            import.timeout_ms = match timeout {
                Some(ConfigValue::Int(n)) if n > 0 => Some(n),
                None => None,
                _ => return Err(error("ENV003", "timeout_ms must be positive")),
            };
            import.credentials.clear();
            for (slot, reference) in &import.declaration.credentials {
                if !secret_slots.contains_key(reference) {
                    return Err(error(
                        "ENV003",
                        "import references an undeclared secret slot",
                    ));
                }
                match secret_refs.get(reference) {
                    Some(reference) => {
                        import.credentials.insert(slot.clone(), reference.clone());
                    }
                    None if d.abstract_environment => {}
                    None => return Err(error("ENV003", "import has an unassigned secret slot")),
                }
            }
            imports.insert(key, Arc::new(import));
        }
        let depth = parent.as_ref().map_or(1, |p| p.depth + 1);
        let mut f = Fingerprint::new("wes/environment/effective/v1");
        f.revision(origin.declaration);
        f.flag(parent.is_some());
        if let Some(parent) = &parent {
            f.revision(parent.revision);
        }
        f.number(imports.len());
        for (alias, import) in &imports {
            f.text(alias);
            f.text(&import.origin.environment);
            f.revision(import.origin.declaration);
            f.import(&import.declaration);
            f.text(import.source.format());
            f.text(import.source.bytes());
            if let TargetKind::Docker {
                socket,
                destination,
                image,
                shell,
            } = &import.target.kind
            {
                f.text("docker/exec/v1");
                f.text(socket);
                fingerprint_destination(&mut f, destination);
                f.flag(image.is_some());
                if let Some(image) = image {
                    f.text(image);
                }
                if let Some(shell) = shell {
                    f.text("docker/terminal-shell/v1");
                    f.text(shell);
                }
            }
            if let TargetKind::Ssh(config) = &import.target.kind {
                f.text("ssh/posix-exec/v1");
                for value in [
                    &config.client,
                    &config.host,
                    &config.user,
                    &config.identity_file,
                    &config.known_hosts,
                ] {
                    f.text(value);
                }
                f.number(usize::from(config.port));
            }
            let managed = import.target.cwd.is_some() || !import.target.variables.is_empty();
            f.text(if managed { "local/launch/v1" } else { "local" });
            f.text(import.target.name());
            if managed {
                f.flag(import.target.cwd.is_some());
                if let Some(cwd) = &import.target.cwd {
                    f.text(cwd);
                }
                f.number(import.target.variables.len());
                for (name, value) in &import.target.variables {
                    f.text(name);
                    f.text(value);
                }
            }
            f.auth(&import.declaration.auth);
            if let Some(transport) = &import.declaration.transport {
                f.text("http-transport/v1");
                f.text(transport);
            }
            f.flag(import.endpoint.is_some());
            if let Some(endpoint) = &import.endpoint {
                f.text(endpoint);
            }
            f.flag(import.timeout_ms.is_some());
            if let Some(timeout) = import.timeout_ms {
                f.value(&ConfigValue::Int(timeout));
            }
            f.number(import.credentials.len());
            for (k, v) in &import.credentials {
                f.text(k);
                f.text(v);
            }
        }
        f.number(parameters.len());
        for (k, p) in &parameters {
            f.text(k);
            f.kind(p.kind);
            f.flag(p.default.is_some());
            if let Some(v) = &p.default {
                f.value(v);
            }
        }
        f.number(config.len());
        for (k, v) in &config {
            f.text(k);
            f.value(v);
        }
        f.number(secret_slots.len());
        for (k, v) in &secret_slots {
            f.text(k);
            f.flag(*v);
        }
        f.number(secret_refs.len());
        for (k, v) in &secret_refs {
            f.text(k);
            f.text(v);
        }
        // Preserve existing revision evidence when no independent attachments were declared.
        if !attached_targets.is_empty() {
            f.text("execution-targets/v1");
            f.number(attached_targets.len());
            for target in attached_targets.values() {
                fingerprint_target(&mut f, target);
            }
        }
        let (revision, own_charge) = f.finish();
        let charge = own_charge
            .checked_add(parent.as_ref().map_or(0, |p| p.charge))
            .ok_or_else(|| error("ENV007", "effective closure byte budget exceeded"))?;
        if charge > 16 * 1024 * 1024 {
            return Err(error("ENV007", "effective closure byte budget exceeded"));
        }
        let protected = d.protected || parent.as_ref().is_some_and(|p| p.is_protected());
        Ok(Self {
            attached_targets,
            id: d.id.clone().unwrap_or_else(|| name.into()),
            retired: d.retired,
            protected,
            owner: d.owner.clone(),
            drift: d.drift,
            name: name.into(),
            revision,
            parent,
            abstract_environment: d.abstract_environment,
            parameters,
            config,
            secret_slots,
            secret_refs,
            imports,
            depth,
            charge,
        })
    }
    /// Capturing a binding is not run admission. Credentials and current target authority remain external.
    pub fn bind(self: &Arc<Self>, alias: &str) -> Result<Binding, EnvironmentError> {
        if self.abstract_environment {
            return Err(error(
                "ENV005",
                "abstract environments cannot execute providers",
            ));
        }
        let import = self
            .imports
            .get(alias)
            .ok_or_else(|| error("ENV005", "provider is absent from the selected environment"))?
            .clone();
        Ok(Binding {
            alias: alias.into(),
            environment: self.clone(),
            import,
        })
    }
}
#[derive(Clone, Debug)]
pub struct Binding {
    alias: String,
    environment: Arc<EffectiveEnvironment>,
    import: Arc<EffectiveImport>,
}
impl Binding {
    pub fn alias(&self) -> &str {
        &self.alias
    }
    pub fn environment(&self) -> &Arc<EffectiveEnvironment> {
        &self.environment
    }
    pub fn import(&self) -> &Arc<EffectiveImport> {
        &self.import
    }
}
fn resolve_setting(
    setting: &Option<Setting>,
    kind: ConfigType,
    parameters: &BTreeMap<String, Parameter>,
    config: &BTreeMap<String, ConfigValue>,
    abstract_environment: bool,
) -> Result<Option<ConfigValue>, EnvironmentError> {
    let value = match setting {
        None => return Ok(None),
        Some(Setting::Literal(v)) => Some(v),
        Some(Setting::Config(key)) => {
            let parameter = parameters
                .get(key)
                .ok_or_else(|| error("ENV003", "import names an undeclared configuration slot"))?;
            if parameter.kind != kind {
                return Err(error(
                    "ENV003",
                    "configuration slot is incompatible with adapter binding",
                ));
            }
            config.get(key)
        }
    };
    match value {
        Some(v) if kind.accepts(v) => Ok(Some(v.clone())),
        None if abstract_environment => Ok(None),
        _ => Err(error(
            "ENV003",
            "missing or incorrectly typed adapter binding",
        )),
    }
}

fn fingerprint_target(f: &mut Fingerprint, target: &Target) {
    f.text(target.name());
    match target.kind() {
        TargetKind::Local => f.text("local"),
        TargetKind::Docker {
            socket,
            destination,
            image,
            shell,
        } => {
            f.text("docker");
            f.text(socket);
            fingerprint_destination(f, destination);
            f.flag(image.is_some());
            if let Some(image) = image {
                f.text(image);
            }
            if let Some(shell) = shell {
                f.text("docker/terminal-shell/v1");
                f.text(shell);
            }
        }
        TargetKind::Ssh(config) => {
            f.text("ssh/posix-exec/v1");
            for value in [
                &config.client,
                &config.host,
                &config.user,
                &config.identity_file,
                &config.known_hosts,
            ] {
                f.text(value);
            }
            f.number(usize::from(config.port));
        }
    }
    f.flag(target.cwd().is_some());
    if let Some(cwd) = target.cwd() {
        f.text(cwd);
    }
    f.number(target.variables().len());
    for (key, value) in target.variables() {
        f.text(key);
        f.text(value);
    }
}

fn fingerprint_destination(f: &mut Fingerprint, destination: &DockerDestination) {
    match destination {
        DockerDestination::Container(container) => f.text(container),
        DockerDestination::Compose {
            project,
            service,
            replica,
        } => {
            f.text("docker/compose-selector/v1");
            f.text(project);
            f.text(service);
            f.flag(replica.is_some());
            if let Some(replica) = replica {
                f.number(*replica as usize);
            }
        }
    }
}
