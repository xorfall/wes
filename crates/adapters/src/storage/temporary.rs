//! Process-owned temporary payloads. Restart reclaims owned artifacts instead of recovering them.
use super::*;
use cap_std::fs::{Dir, DirBuilder};

pub(super) struct TemporaryValues {
    pub values: Option<FileValues>,
    parent: Dir,
    name: String,
}
impl std::ops::Deref for TemporaryValues {
    type Target = FileValues;
    fn deref(&self) -> &FileValues {
        self.values.as_ref().expect("temporary values")
    }
}
impl std::ops::DerefMut for TemporaryValues {
    fn deref_mut(&mut self) -> &mut FileValues {
        self.values.as_mut().expect("temporary values")
    }
}
impl TemporaryValues {
    pub fn open(
        parent: &OwnedDirectory,
        limits: Limits,
        budget: Option<NonZeroU64>,
        protection: Option<Arc<crate::protected_storage::ObjectProtection>>,
    ) -> Result<Self, StoreError> {
        for entry in parent
            .entries()
            .map_err(|e| StoreError::backend("listing temporary owners", e))?
            .enumerate()
        {
            if entry.0 == 100_000 {
                return Err(StoreError::Limit("temporary owners"));
            }
            let entry = entry.1;
            let entry = entry.map_err(|e| StoreError::backend("reading a temporary owner", e))?;
            let name = entry.file_name();
            let Some(name) = name.to_str().filter(|n| {
                n.strip_prefix(".wes-transient-")
                    .and_then(|id| ValueHandle::new(id).ok())
                    .is_some()
            }) else {
                continue;
            };
            let metadata = parent
                .symlink_metadata(name)
                .map_err(|e| StoreError::backend("checking a temporary owner", e))?;
            if !metadata.is_dir() || metadata.is_symlink() {
                continue;
            }
            let directory = parent
                .open_dir(name)
                .map_err(|e| StoreError::backend("opening a temporary owner", e))?;
            match OwnedDirectory::existing(directory, DirectoryKind::Values, Durability::File) {
                Ok(directory) => cleanup(parent, name, directory)?,
                Err(
                    DirectoryError::Locked | DirectoryError::Unowned | DirectoryError::NotRegular,
                ) => continue,
                Err(error) => return Err(directory_error(error)),
            }
        }
        let name = format!(".wes-transient-{}", Uuid::new_v4());
        let mut builder = DirBuilder::new();
        #[cfg(unix)]
        {
            use cap_std::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        parent
            .create_dir_with(&name, &builder)
            .map_err(|e| StoreError::backend("creating a temporary owner", e))?;
        let directory = OwnedDirectory::from_dir(
            parent
                .open_dir(&name)
                .map_err(|e| StoreError::backend("opening a temporary owner", e))?,
            DirectoryKind::Values,
            Durability::File,
        )
        .map_err(directory_error)?;
        let values = FileValues {
            directory,
            limits,
            budget: Some(budget.unwrap_or_else(|| {
                NonZeroU64::new(wes_budgets::get("storage.live.bytes")).unwrap()
            })),
            evicted: Default::default(),
            published: Default::default(),
            next_publication: 0,
            holds: Default::default(),
            drafts: Default::default(),
            manifests: Default::default(),
            protection,
        };
        Ok(Self {
            values: Some(values),
            parent: parent
                .try_clone()
                .map_err(|e| StoreError::backend("retaining a temporary owner", e))?,
            name,
        })
    }
}
fn cleanup(parent: &Dir, name: &str, directory: OwnedDirectory) -> Result<(), StoreError> {
    let mut names = Vec::new();
    for (index, entry) in directory
        .entries()
        .map_err(|e| StoreError::backend("listing temporary artifacts", e))?
        .enumerate()
    {
        if index == 100_000 {
            return Err(StoreError::Limit("temporary artifacts"));
        }
        let entry = entry.map_err(|e| StoreError::backend("reading a temporary artifact", e))?;
        let name = entry.file_name();
        let Some(text) = name.to_str() else {
            return Err(StoreError::UnownedDirectory);
        };
        let recognized = matches!(text, ".wes-value-store" | ".wes-values.lock")
            || text
                .strip_suffix(".bin")
                .and_then(|id| ValueHandle::new(id).ok())
                .is_some()
            || text
                .strip_suffix(".json")
                .and_then(|id| ValueHandle::new(id).ok())
                .is_some()
            || text
                .strip_prefix(".wes-pending-")
                .and_then(|id| ValueHandle::new(id).ok())
                .is_some();
        if !recognized
            || !directory
                .symlink_metadata(text)
                .map_err(|e| StoreError::backend("checking a temporary artifact", e))?
                .is_file()
        {
            return Err(StoreError::UnownedDirectory);
        }
        names.push(name);
    }
    let directory = directory.into_retired();
    for name in names {
        directory
            .remove_file(name)
            .map_err(|e| StoreError::backend("removing a temporary artifact", e))?;
    }
    parent
        .remove_dir(name)
        .map_err(|e| StoreError::backend("removing a temporary owner", e))
}
impl Drop for TemporaryValues {
    fn drop(&mut self) {
        if let Some(values) = self.values.take() {
            if let Err(error) = cleanup(&self.parent, &self.name, values.directory) {
                tracing::warn!(%error, "temporary result cleanup could not be completed");
            }
        }
    }
}
