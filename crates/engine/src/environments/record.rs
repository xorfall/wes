//! Durable definition evidence, never an executable plan or credential grant.
use super::*;

#[derive(Clone, PartialEq, Eq)]
pub struct EnvironmentRecord {
    id: String,
    yaml: Arc<str>,
    sources: CapturedSources,
    before: BTreeMap<String, Revision>,
    after: BTreeMap<String, Revision>,
}
impl fmt::Debug for EnvironmentRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvironmentRecord")
            .field("id", &self.id)
            .field("bytes", &self.charge())
            .finish_non_exhaustive()
    }
}
impl EnvironmentRecord {
    pub fn new(
        id: String,
        yaml: String,
        sources: CapturedSources,
        before: BTreeMap<String, Revision>,
        after: BTreeMap<String, Revision>,
    ) -> Result<Self, EnvironmentError> {
        if uuid::Uuid::parse_str(&id).is_err() || id.len() != 36 {
            return Err(error(
                "ENV009",
                "environment record requires a UUID identity",
            ));
        }
        let package = Package::parse(&yaml)?;
        sources.validate(&package)?;
        if before.len() > wes_core::environments::MAX_ENVIRONMENTS
            || after.len() > wes_core::environments::MAX_ENVIRONMENTS
            || !after.keys().eq(package.definitions().keys())
        {
            return Err(error(
                "ENV009",
                "environment record has inconsistent revision identities",
            ));
        }
        if before
            .keys()
            .any(|s| s.is_empty() || s.len() > 128 || s.chars().any(char::is_control))
        {
            return Err(error("ENV009", "invalid prior environment identity"));
        }
        Ok(Self {
            id,
            yaml: yaml.into(),
            sources,
            before,
            after,
        })
    }
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn yaml(&self) -> &str {
        &self.yaml
    }
    pub fn sources(&self) -> &CapturedSources {
        &self.sources
    }
    pub fn before(&self) -> &BTreeMap<String, Revision> {
        &self.before
    }
    pub fn after(&self) -> &BTreeMap<String, Revision> {
        &self.after
    }
    pub fn charge(&self) -> u64 {
        ((self.yaml.len() + self.sources.charge()) as u64)
            .saturating_mul(6)
            .saturating_add((self.before.len() + self.after.len()) as u64 * 512)
            .saturating_add(self.sources.iter().count() as u64 * 256)
            .saturating_add(4096)
    }
    /// Reconstruct exact evidence in order. This newly prepared plan is not serialized authority.
    pub fn prepare(&self, registry: &Registry) -> Result<Plan, EnvironmentError> {
        if registry.recorded_revisions() != self.before {
            return Err(error(
                "ENV009",
                "environment history predecessor does not match",
            ));
        }
        let package = Package::parse(&self.yaml)?;
        let plan = registry.plan_document(&package, &self.sources)?;
        let after: BTreeMap<_, _> = plan
            .active
            .iter()
            .filter(|(n, _)| !plan.configured.contains_key(*n))
            .map(|(n, e)| (n.clone(), e.revision()))
            .collect();
        if after != self.after {
            return Err(error(
                "ENV009",
                "environment replay does not reconstruct its recorded revisions",
            ));
        }
        Ok(plan)
    }
    pub(crate) fn capture(
        yaml: String,
        sources: CapturedSources,
        registry: &Registry,
    ) -> Result<(Plan, Self), EnvironmentError> {
        Self::capture_mode(yaml, sources, registry, false)
    }
    pub(crate) fn capture_document(
        yaml: String,
        sources: CapturedSources,
        registry: &Registry,
    ) -> Result<(Plan, Self), EnvironmentError> {
        Self::capture_mode(yaml, sources, registry, true)
    }
    fn capture_mode(
        yaml: String,
        sources: CapturedSources,
        registry: &Registry,
        document: bool,
    ) -> Result<(Plan, Self), EnvironmentError> {
        let package = Package::parse(&yaml)?;
        let plan = if document {
            registry.plan_document(&package, &sources)?
        } else {
            registry.plan(&package, &sources)?
        };
        let after = plan
            .active
            .iter()
            .filter(|(n, _)| !plan.configured.contains_key(*n))
            .map(|(n, e)| (n.clone(), e.revision()))
            .collect();
        let record = Self {
            id: uuid::Uuid::new_v4().to_string(),
            yaml: yaml.into(),
            sources,
            before: registry.recorded_revisions(),
            after,
        };
        Ok((plan, record))
    }
}
