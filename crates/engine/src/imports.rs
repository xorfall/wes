//! Captured importer inputs, independent of live workspace and execution authority.
use crate::{
    driver::CancellationToken,
    providers::Invoker,
    value_size::{shape_charge, value_charge},
};
use indexmap::IndexMap;
use std::{fmt, sync::Arc};
use thiserror::Error;
use wes_core::{
    Value,
    capability::{Parameter, ProviderDescription, Rule, RuleBasis, Sort},
};

pub fn max_recipe_bytes() -> usize {
    wes_budgets::get("imports.recipe_bytes") as usize
}
fn max_sources() -> usize {
    wes_budgets::get("imports.sources") as usize
}
fn max_products() -> u64 {
    wes_budgets::get("imports.products") as u64
}
fn max_entries() -> usize {
    wes_budgets::get("imports.entries") as usize
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum ImportError {
    #[error("{0}")]
    Input(&'static str),
    #[error("invalid importer kind, alias, format or argument name")]
    Name,
    #[error("import input or metadata exceeds its supported budget")]
    Capacity,
    #[error("the selected importer is unavailable")]
    UnknownImporter,
    #[error("the import input could not be read")]
    Unavailable,
    #[error("the captured import format or contents are invalid")]
    InvalidRecipe,
    #[error("the replay record does not contain this import input")]
    MissingSnapshot,
    #[error("replay contains conflicting import snapshots")]
    ConflictingSnapshot,
    #[error("import preparation was cancelled")]
    Cancelled,
    #[error("the import worker terminated unexpectedly")]
    Worker,
    #[error("the import belongs to another input capture")]
    ForeignCapture,
}

/// Arguments are already analysed immutable values. `as` is represented only by the alias field.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportRequest {
    kind: String,
    alias: Option<String>,
    arguments: IndexMap<String, Value>,
    charge: usize,
}
impl ImportRequest {
    pub fn new(
        kind: String,
        alias: Option<String>,
        arguments: IndexMap<String, Value>,
    ) -> Result<Self, ImportError> {
        valid_name(&kind)?;
        if let Some(alias) = &alias {
            valid_name(alias)?;
        }
        if arguments.len() > 256 || arguments.contains_key("as") {
            return Err(ImportError::Capacity);
        }
        let mut charge = 256 + kind.len() + alias.as_ref().map_or(0, String::len);
        for (key, value) in &arguments {
            valid_name(key)?;
            charge += key.len();
            let remaining = (128usize * 1024)
                .checked_sub(charge)
                .ok_or(ImportError::Capacity)?;
            charge += value_charge(value, remaining as u64).ok_or(ImportError::Capacity)? as usize;
        }
        Ok(Self {
            kind,
            alias,
            arguments,
            charge,
        })
    }
    pub fn kind(&self) -> &str {
        &self.kind
    }
    pub fn alias(&self) -> Option<&str> {
        self.alias.as_deref()
    }
    pub fn arguments(&self) -> &IndexMap<String, Value> {
        &self.arguments
    }
}
impl fmt::Debug for ImportRequest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportRequest")
            .field("kind", &self.kind)
            .field("arguments", &self.arguments.len())
            .finish_non_exhaustive()
    }
}

/// Versioned, importer-owned UTF-8 recipe. It must contain every input needed to rebuild metadata;
/// credential names are allowed, credential values and callable handles are not.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportRecipe {
    format: String,
    source: Arc<str>,
}
impl ImportRecipe {
    pub fn new(format: String, source: String) -> Result<Self, ImportError> {
        valid_name(&format)?;
        if source.len() > max_recipe_bytes() {
            return Err(ImportError::Capacity);
        }
        Ok(Self {
            format,
            source: Arc::from(source),
        })
    }
    pub fn format(&self) -> &str {
        &self.format
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn shared_source(&self) -> Arc<str> {
        self.source.clone()
    }
    fn charge(&self) -> usize {
        128 + self.format.len() + self.source.len()
    }
}
impl fmt::Debug for ImportRecipe {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportRecipe")
            .field("format", &self.format)
            .field("bytes", &self.source.len())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ImportOrigin {
    #[default]
    Literal,
    Applied,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportSnapshot {
    origin: ImportOrigin,
    request: Arc<ImportRequest>,
    recipe: ImportRecipe,
}
impl ImportSnapshot {
    pub fn new(request: ImportRequest, recipe: ImportRecipe) -> Self {
        Self::from_origin(request, recipe, ImportOrigin::Literal)
    }
    pub fn from_origin(request: ImportRequest, recipe: ImportRecipe, origin: ImportOrigin) -> Self {
        Self {
            origin,
            request: Arc::new(request),
            recipe,
        }
    }
    pub fn origin(&self) -> ImportOrigin {
        self.origin
    }
    pub fn request(&self) -> &ImportRequest {
        &self.request
    }
    pub fn recipe(&self) -> &ImportRecipe {
        &self.recipe
    }
    fn charge(&self) -> usize {
        self.request.charge + self.recipe.charge()
    }
}

/// Metadata and invoker are validated and transported together, but not registered here.
pub struct ImportProduct {
    description: ProviderDescription,
    invoker: Arc<dyn Invoker>,
    streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
    conversations: Option<Arc<dyn crate::conversations::InteractiveInvoker>>,
    environment_factory: Option<Arc<dyn EnvironmentFactory>>,
    warnings: Vec<String>,
    charge: u64,
}
/// Inert construction of ports that need the final captured environment and its live authority.
/// The factory must preserve the product's catalogue and may not probe external resources.
pub trait EnvironmentFactory: Send + Sync + 'static {
    fn bind(
        &self,
        binding: &wes_core::environments::Binding,
        authority: crate::environments::Authority,
    ) -> Result<ImportProduct, ImportError>;
}
impl ImportProduct {
    pub fn with_environment_factory(mut self, factory: Arc<dyn EnvironmentFactory>) -> Self {
        self.environment_factory = Some(factory);
        self
    }
    pub(crate) fn bind_environment(
        &self,
        binding: &wes_core::environments::Binding,
        authority: Option<crate::environments::Authority>,
    ) -> Result<Option<ImportProduct>, ImportError> {
        let Some(factory) = &self.environment_factory else {
            return Ok(None);
        };
        let product = factory.bind(
            binding,
            authority.ok_or(ImportError::Input("environment authority is unavailable"))?,
        )?;
        if product.environment_factory.is_some()
            || product.description.name() != self.description.name()
            || product.description.secrets() != self.description.secrets()
            || !product
                .description
                .capabilities()
                .eq(self.description.capabilities())
            || product.streams.is_some() != self.streams.is_some()
            || product.conversations.is_some() != self.conversations.is_some()
            || product.charge > self.charge
        {
            return Err(ImportError::Input(
                "bound provider changed its captured metadata or ports",
            ));
        }
        Ok(Some(product))
    }
    pub(crate) fn charge(&self) -> u64 {
        self.charge
    }
    pub fn new(
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        warnings: Vec<String>,
    ) -> Result<Self, ImportError> {
        let charge = metadata_charge(&description, &warnings).ok_or(ImportError::Capacity)?;
        Ok(Self {
            description,
            invoker,
            streams: None,
            conversations: None,
            environment_factory: None,
            warnings,
            charge,
        })
    }
    pub fn with_streams(mut self, streams: Arc<dyn crate::streams::StreamingInvoker>) -> Self {
        self.streams = Some(streams);
        self
    }
    pub fn streams(&self) -> Option<&Arc<dyn crate::streams::StreamingInvoker>> {
        self.streams.as_ref()
    }
    pub fn with_conversations(
        mut self,
        conversations: Arc<dyn crate::conversations::InteractiveInvoker>,
    ) -> Self {
        self.conversations = Some(conversations);
        self
    }
    pub fn conversations(&self) -> Option<&Arc<dyn crate::conversations::InteractiveInvoker>> {
        self.conversations.as_ref()
    }
    pub fn description(&self) -> &ProviderDescription {
        &self.description
    }
    pub fn invoker(&self) -> &Arc<dyn Invoker> {
        &self.invoker
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }
}
impl fmt::Debug for ImportProduct {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportProduct")
            .field("capabilities", &self.description.capabilities().len())
            .field("warnings", &self.warnings.len())
            .finish_non_exhaustive()
    }
}

/// Trusted local adapter, not a Rust plugin sandbox. `capture` may perform bounded local input I/O;
/// it must enforce max_bytes while reading. Capture may retain its exact input in an application
/// source archive. Neither method may invoke a provider or mutate execution/value stores.
/// `build` uses only the supplied snapshot for provider metadata: no file/network reads or fallback.
/// Live mode may inspect named credentials through the read-only credential port to warn about
/// availability. Replay mode must not look them up; both may attach the handle for later invocation.
pub trait Importer: Send + Sync + 'static {
    /// Inert input hints, captured once at registration. No I/O or execution; validation
    /// remains the adapter's responsibility. `as` is supplied by the language itself.
    fn parameters(&self) -> Vec<Parameter> {
        Vec::new()
    }
    fn capture(
        &self,
        request: &ImportRequest,
        max_bytes: usize,
    ) -> Result<ImportRecipe, ImportError>;
    fn build(
        &self,
        snapshot: &ImportSnapshot,
        mode: ImportMode,
    ) -> Result<ImportProduct, ImportError>;
}
#[derive(Clone, Default)]
pub struct Importers {
    entries: IndexMap<String, Arc<dyn Importer>>,
    parameters: IndexMap<String, Vec<Parameter>>,
}
impl Importers {
    pub fn register(
        &mut self,
        kind: String,
        importer: Arc<dyn Importer>,
    ) -> Result<Option<Arc<dyn Importer>>, ImportError> {
        valid_name(&kind)?;
        if matches!(kind.as_str(), "plan" | "apply") {
            return Err(ImportError::Name);
        }
        if self.entries.len() >= max_entries() && !self.entries.contains_key(&kind) {
            return Err(ImportError::Capacity);
        }
        let parameters = importer.parameters();
        if parameters.len() > 256 {
            return Err(ImportError::Capacity);
        }
        let mut names = std::collections::BTreeSet::new();
        let mut left = 64 * 1024;
        for parameter in &parameters {
            valid_name(&parameter.name)?;
            if parameter.name == "as" || !names.insert(&parameter.name) {
                return Err(ImportError::Name);
            }
            text(&mut left, &parameter.name).ok_or(ImportError::Capacity)?;
            left -= shape_charge(&parameter.shape, left).ok_or(ImportError::Capacity)?;
            if let Some(content) = &parameter.content {
                text(&mut left, content).ok_or(ImportError::Capacity)?;
            }
            if let Sort::Selector(name) | Sort::Resource(name) | Sort::Fresh(name) = &parameter.sort
            {
                text(&mut left, name).ok_or(ImportError::Capacity)?;
            }
        }
        self.parameters.insert(kind.clone(), parameters);
        Ok(self.entries.insert(kind, importer))
    }
    pub(crate) fn entry(&self, kind: &str) -> Option<Arc<dyn Importer>> {
        self.entries.get(kind).cloned()
    }
    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.entries.keys().map(String::as_str)
    }
    pub fn parameters(&self) -> &IndexMap<String, Vec<Parameter>> {
        &self.parameters
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportMode {
    Live,
    Replay,
}
struct Entry {
    snapshot: ImportSnapshot,
    product: Option<Arc<ImportProduct>>,
}
#[derive(Clone)]
pub struct CapturedImport {
    owner: Arc<()>,
    index: usize,
    product: Arc<ImportProduct>,
    request: Arc<ImportRequest>,
    importer: Arc<dyn Importer>,
    recipe: ImportRecipe,
}
impl CapturedImport {
    pub fn request(&self) -> &ImportRequest {
        &self.request
    }
    pub fn recipe(&self) -> &ImportRecipe {
        &self.recipe
    }
    pub fn product(&self) -> &Arc<ImportProduct> {
        &self.product
    }
    pub(crate) fn matches(&self, request: &ImportRequest, registry: &Importers) -> bool {
        self.request.as_ref() == request
            && registry
                .entries
                .get(request.kind())
                .is_some_and(|entry| Arc::ptr_eq(entry, &self.importer))
    }
}
impl fmt::Debug for CapturedImport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CapturedImport")
            .field("request", &self.request)
            .field("product", &self.product)
            .finish_non_exhaustive()
    }
}

/// One immutable registry snapshot per source cell. Successful reads remain cached even when
/// metadata validation fails. Only imports explicitly accepted after staging enter finish().
pub struct ImportCapture {
    owner: Arc<()>,
    mode: ImportMode,
    importers: Importers,
    entries: Vec<Entry>,
    accepted: Vec<usize>,
    source_bytes: usize,
    product_bytes: u64,
}
impl ImportCapture {
    pub fn live(importers: Importers) -> Self {
        Self::new(importers, ImportMode::Live)
    }
    fn new(importers: Importers, mode: ImportMode) -> Self {
        Self {
            owner: Arc::new(()),
            mode,
            importers,
            entries: vec![],
            accepted: vec![],
            source_bytes: 0,
            product_bytes: 0,
        }
    }
    /// Validate the complete captured input set without invoking any importer method.
    pub fn replay(
        importers: Importers,
        snapshots: Vec<ImportSnapshot>,
    ) -> Result<Self, ImportError> {
        validate_snapshots(&snapshots)?;
        let mut capture = Self::new(importers, ImportMode::Replay);
        for snapshot in snapshots {
            if let Some(entry) = capture.entries.iter().find(|entry| {
                snapshot.origin == ImportOrigin::Literal
                    && entry.snapshot.origin == ImportOrigin::Literal
                    && entry.snapshot.request == snapshot.request
            }) {
                if entry.snapshot != snapshot {
                    return Err(ImportError::ConflictingSnapshot);
                }
            } else {
                capture.insert(snapshot)?;
            }
        }
        Ok(capture)
    }
    fn insert(&mut self, snapshot: ImportSnapshot) -> Result<usize, ImportError> {
        let bytes = self
            .source_bytes
            .checked_add(snapshot.charge())
            .ok_or(ImportError::Capacity)?;
        if self.entries.len() >= max_entries() || bytes > max_sources() {
            return Err(ImportError::Capacity);
        }
        let index = self.entries.len();
        self.entries.push(Entry {
            snapshot,
            product: None,
        });
        self.source_bytes = bytes;
        Ok(index)
    }
    /// Join entered reader/builder work on cooperative cancellation. The session must retain this
    /// future until physical completion; dropping it cannot preempt a blocking OS read.
    pub async fn read(
        &mut self,
        request: &ImportRequest,
        cancellation: CancellationToken,
    ) -> Result<CapturedImport, ImportError> {
        self.read_origin(request, ImportOrigin::Literal, cancellation)
            .await
    }
    pub(crate) fn replay_applied_request(&self) -> Result<ImportRequest, ImportError> {
        if self.mode != ImportMode::Replay {
            return Err(ImportError::MissingSnapshot);
        }
        self.entries
            .iter()
            .enumerate()
            .find(|(index, entry)| {
                entry.snapshot.origin == ImportOrigin::Applied && !self.accepted.contains(index)
            })
            .map(|(_, entry)| entry.snapshot.request.as_ref().clone())
            .ok_or(ImportError::MissingSnapshot)
    }
    pub(crate) async fn read_applied(
        &mut self,
        request: &ImportRequest,
        cancellation: CancellationToken,
    ) -> Result<CapturedImport, ImportError> {
        self.read_origin(request, ImportOrigin::Applied, cancellation)
            .await
    }
    async fn read_origin(
        &mut self,
        request: &ImportRequest,
        origin: ImportOrigin,
        cancellation: CancellationToken,
    ) -> Result<CapturedImport, ImportError> {
        cancelled(&cancellation)?;
        if request
            .arguments()
            .values()
            .any(|v| !v.provenance().policy().is_empty())
        {
            return Err(ImportError::Input(
                "Import arguments with resource origins, private or unknown policy require importer transfer admission; no contents were read",
            ));
        }
        let importer = self
            .importers
            .entries
            .get(request.kind())
            .cloned()
            .ok_or(ImportError::UnknownImporter)?;
        let index = if let Some(index) = self
            .entries
            .iter()
            .enumerate()
            .find(|(index, entry)| {
                entry.snapshot.origin == origin
                    && entry.snapshot.request.as_ref() == request
                    && (origin == ImportOrigin::Literal || !self.accepted.contains(index))
            })
            .map(|(index, _)| index)
        {
            index
        } else {
            if matches!(self.mode, ImportMode::Replay) {
                return Err(ImportError::MissingSnapshot);
            }
            if self.entries.len() >= max_entries() {
                return Err(ImportError::Capacity);
            }
            let max_bytes = max_sources()
                .checked_sub(self.source_bytes)
                .and_then(|left| left.checked_sub(request.charge + 128 + 256))
                .ok_or(ImportError::Capacity)?
                .min(max_recipe_bytes());
            let reading = importer.clone();
            let requested = request.clone();
            let token = cancellation.clone();
            let result = tokio::task::spawn_blocking(move || {
                cancelled(&token)?;
                reading.capture(&requested, max_bytes)
            })
            .await;
            cancelled(&cancellation)?;
            let recipe = result.map_err(|_| ImportError::Worker)??;
            if recipe.source.len() > max_bytes {
                return Err(ImportError::Capacity);
            }
            self.insert(ImportSnapshot::from_origin(request.clone(), recipe, origin))?
        };
        if self.entries[index].product.is_none() {
            let snapshot = self.entries[index].snapshot.clone();
            let token = cancellation.clone();
            let mode = self.mode;
            let importer = importer.clone();
            let result = tokio::task::spawn_blocking(move || {
                cancelled(&token)?;
                let mut product = importer.build(&snapshot, mode)?;
                if let Some(alias) = snapshot.request.alias() {
                    product.description = product
                        .description
                        .renamed(alias)
                        .map_err(|_| ImportError::Name)?;
                    product.charge = metadata_charge(&product.description, &product.warnings)
                        .ok_or(ImportError::Capacity)?;
                }
                Ok::<_, ImportError>(product)
            })
            .await;
            cancelled(&cancellation)?;
            let product = result.map_err(|_| ImportError::Worker)??;
            let bytes = self
                .product_bytes
                .checked_add(product.charge)
                .ok_or(ImportError::Capacity)?;
            if bytes > max_products() {
                return Err(ImportError::Capacity);
            }
            self.entries[index].product = Some(Arc::new(product));
            self.product_bytes = bytes;
        }
        Ok(CapturedImport {
            owner: self.owner.clone(),
            index,
            product: self.entries[index].product.as_ref().expect("built").clone(),
            request: self.entries[index].snapshot.request.clone(),
            importer,
            recipe: self.entries[index].snapshot.recipe.clone(),
        })
    }
    pub fn accept(&mut self, captured: &CapturedImport) -> Result<(), ImportError> {
        if !Arc::ptr_eq(&self.owner, &captured.owner) {
            return Err(ImportError::ForeignCapture);
        }
        if !self.accepted.contains(&captured.index) {
            self.accepted.push(captured.index);
        }
        Ok(())
    }
    pub fn finish(self) -> Vec<ImportSnapshot> {
        self.accepted
            .iter()
            .map(|&index| self.entries[index].snapshot.clone())
            .collect()
    }
    pub(crate) fn verify_complete(&self) -> Result<(), ImportError> {
        if self.mode == ImportMode::Replay && self.accepted.len() != self.entries.len() {
            return Err(ImportError::MissingSnapshot);
        }
        Ok(())
    }
}
fn cancelled(token: &CancellationToken) -> Result<(), ImportError> {
    if token.is_cancelled() {
        Err(ImportError::Cancelled)
    } else {
        Ok(())
    }
}
pub(crate) fn validate_snapshots(snapshots: &[ImportSnapshot]) -> Result<(), ImportError> {
    if snapshots.len() > max_entries() {
        return Err(ImportError::Capacity);
    }
    let mut bytes = 0usize;
    for (i, snapshot) in snapshots.iter().enumerate() {
        if snapshot.origin == ImportOrigin::Applied
            && snapshot.request.arguments().values().any(|v| {
                !matches!(v.shape(), wes_core::Shape::Primitive(_))
                    || !v.provenance().policy().is_empty()
            })
        {
            return Err(ImportError::InvalidRecipe);
        }
        bytes = bytes
            .checked_add(snapshot.charge())
            .ok_or(ImportError::Capacity)?;
        if bytes > max_sources() {
            return Err(ImportError::Capacity);
        }
        if snapshots[..i].iter().any(|previous| {
            previous.origin == ImportOrigin::Literal
                && snapshot.origin == ImportOrigin::Literal
                && previous.request == snapshot.request
                && previous.recipe != snapshot.recipe
        }) {
            return Err(ImportError::ConflictingSnapshot);
        }
    }
    Ok(())
}
fn valid_name(name: &str) -> Result<(), ImportError> {
    if name.trim().is_empty() || name.len() > 256 || name.chars().any(char::is_control) {
        return Err(ImportError::Name);
    }
    Ok(())
}
fn text(left: &mut u64, value: &str) -> Option<()> {
    *left = left.checked_sub(128 + value.len() as u64 * 6)?;
    Some(())
}
fn metadata_charge(description: &ProviderDescription, warnings: &[String]) -> Option<u64> {
    let mut left = 16 * 1024 * 1024u64;
    if warnings.len() > 1000 || description.capabilities().len() > 10_000 {
        return None;
    }
    text(&mut left, description.name())?;
    for value in description.secrets().iter().chain(warnings) {
        text(&mut left, value)?;
    }
    for cap in description.capabilities() {
        if let Some(resources) = &cap.resources {
            for field in resources.fields() {
                text(&mut left, field)?;
            }
        }
        text(&mut left, &cap.summary)?;
        for name in cap.path.iter().chain(&cap.provenance_arguments) {
            text(&mut left, name)?;
        }
        left -= shape_charge(&cap.result, left)?;
        for parameter in &cap.parameters {
            text(&mut left, &parameter.name)?;
            left -= shape_charge(&parameter.shape, left)?;
            if let Some(content) = &parameter.content {
                text(&mut left, content)?;
            }
            if let Sort::Selector(name) | Sort::Resource(name) | Sort::Fresh(name) = &parameter.sort
            {
                text(&mut left, name)?;
            }
        }
        for declared in &cap.rules {
            match &declared.rule {
                Rule::OneOf { key, values } => {
                    text(&mut left, key)?;
                    for value in values {
                        text(&mut left, value)?;
                    }
                }
                Rule::Requires { key, needs } => {
                    text(&mut left, key)?;
                    text(&mut left, needs)?;
                }
                Rule::MutuallyExclusive(keys) => {
                    text(&mut left, "")?;
                    for key in keys {
                        text(&mut left, key)?;
                    }
                }
                Rule::ProvenanceFact {
                    key,
                    fact,
                    expected,
                } => {
                    text(&mut left, key)?;
                    text(&mut left, fact)?;
                    text(&mut left, expected)?;
                }
            }
            match &declared.basis {
                RuleBasis::Documented { note } => {
                    if let Some(note) = note {
                        text(&mut left, note)?;
                    }
                }
                RuleBasis::Inferred { reason } => text(&mut left, reason)?,
            }
        }
    }
    Some(16 * 1024 * 1024 - left)
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use wes_core::{
        Primitive, Shape,
        capability::{Capability, Safety},
        environments::{EffectiveEnvironment, Package},
    };
    struct Never;
    impl Invoker for Never {
        fn invoke(
            &self,
            _: crate::providers::Call,
            _: CancellationToken,
        ) -> crate::providers::InvocationFuture {
            Box::pin(async { panic!("binding must never invoke a provider") })
        }
    }
    fn product(path: &str) -> ImportProduct {
        ImportProduct::new(
            ProviderDescription::new(
                "fixture",
                [Capability::new(
                    [path],
                    Shape::Primitive(Primitive::Text),
                    Safety::Safe,
                )],
                vec![],
            )
            .unwrap(),
            Arc::new(Never),
            vec![],
        )
        .unwrap()
    }
    struct Factory(&'static str);
    impl EnvironmentFactory for Factory {
        fn bind(
            &self,
            _: &wes_core::environments::Binding,
            _: crate::environments::Authority,
        ) -> Result<ImportProduct, ImportError> {
            Ok(product(self.0))
        }
    }
    #[test]
    fn environment_factory_requires_authority_and_preserves_captured_catalogue() {
        let (package, sources) = Package::configured_providers(
            "default",
            std::collections::BTreeMap::from([(
                "fixture".into(),
                wes_core::environments::ConfiguredProvider {
                    evidence: "fixture/v1".into(),
                    endpoint: None,
                },
            )]),
        )
        .unwrap();
        let environment =
            Arc::new(EffectiveEnvironment::resolve(&package, "default", None, &sources).unwrap());
        let binding = environment.bind("fixture").unwrap();
        let original = product("read").with_environment_factory(Arc::new(Factory("read")));
        assert!(original.bind_environment(&binding, None).is_err());
        assert!(
            original
                .bind_environment(&binding, Some(Default::default()))
                .unwrap()
                .is_some()
        );
        let changed = product("read").with_environment_factory(Arc::new(Factory("write")));
        assert!(
            changed
                .bind_environment(&binding, Some(Default::default()))
                .is_err()
        );
    }
}
