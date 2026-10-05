//! A validated immutable-definition catalogue, shared by drafts and runtime instances.
use crate::{Artifact, Package, validate_catalogue};
use std::{collections::BTreeMap, ops::Deref, sync::Arc};

#[derive(Clone, Debug)]
pub struct Catalogue {
    packages: BTreeMap<String, Arc<Package>>,
    artifacts: BTreeMap<String, Arc<Artifact>>,
}
impl Default for Catalogue {
    fn default() -> Self {
        let mut catalogue = Self::new([]).expect("empty catalogue");
        for artifact in crate::artifacts() {
            catalogue = catalogue
                .installed(Arc::new(artifact.clone()))
                .expect("shipped artifact catalogue");
        }
        catalogue
    }
}
impl Deref for Catalogue {
    type Target = BTreeMap<String, Arc<Package>>;
    fn deref(&self) -> &Self::Target {
        &self.packages
    }
}
impl Catalogue {
    pub fn new(packages: impl IntoIterator<Item = Arc<Package>>) -> Result<Self, String> {
        let mut definitions = BTreeMap::new();
        for p in packages {
            if definitions.insert(p.manifest.name.clone(), p).is_some() {
                return Err("Duplicate view name".into());
            }
        }
        validate_catalogue(definitions.values().map(Arc::as_ref))?;
        Ok(Self {
            packages: definitions,
            artifacts: BTreeMap::new(),
        })
    }
    pub fn installed(&self, artifact: Arc<Artifact>) -> Result<Self, String> {
        if self.artifacts.contains_key(&artifact.digest) {
            return Ok(self.clone());
        }
        let p = &artifact.package;
        if self
            .packages
            .values()
            .any(|old| old.manifest.name == p.manifest.name || old.manifest.id == p.manifest.id)
        {
            return Err(
                "View name/id is already installed; use a new identity for different code".into(),
            );
        }
        if self
            .artifacts
            .values()
            .map(|a| {
                a.source.javascript.len()
                    + a.source.css.len()
                    + a.source.types.len()
                    + a.source.manifest.len()
            })
            .sum::<usize>()
            + artifact.source.javascript.len()
            + artifact.source.css.len()
            + artifact.source.types.len()
            + artifact.source.manifest.len()
            > 32 * 1024 * 1024
        {
            return Err("Installed view catalogue exceeds its 32 MiB budget".into());
        }
        let mut result = self.clone();
        result
            .packages
            .insert(p.manifest.name.clone(), Arc::new(p.clone()));
        result.artifacts.insert(artifact.digest.clone(), artifact);
        validate_catalogue(result.packages.values().map(Arc::as_ref))?;
        Ok(result)
    }
    pub fn artifact(&self, digest: &str) -> Option<&Arc<Artifact>> {
        self.artifacts.get(digest)
    }
}
