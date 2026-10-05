//! Durable logical retirement lives outside the journal it retires. A retained
//! tombstone commits removal; an intent without that tombstone never authorizes cleanup.
use super::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceIdentity {
    pub id: String,
    pub generation: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDeletion {
    pub version: u8,
    pub name: String,
    pub identity: WorkspaceIdentity,
    pub payloads: Vec<String>,
    pub protected: Vec<String>,
}
fn valid_id(id: &str) -> bool {
    id.len() == 32
        && id
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
impl FileWorkspaces {
    fn read_identity(&self, file: &str) -> Result<Option<WorkspaceIdentity>, WorkspaceFileError> {
        let mut options = private_options();
        options.read(true);
        let file = match self.directory.open_with(file, &options) {
            Ok(f) => f,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(storage(e)),
        };
        if !file.metadata().map_err(storage)?.is_file() {
            return Err(WorkspaceFileError::Invalid);
        }
        let mut bytes = Vec::new();
        file.take(129).read_to_end(&mut bytes).map_err(storage)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| WorkspaceFileError::Invalid)?;
        let identity = if let Some(rest) = text
            .strip_prefix(MANIFEST_HEADER)
            .and_then(|s| s.strip_suffix('\n'))
        {
            let (id, generation) = rest.split_once('\n').ok_or(WorkspaceFileError::Invalid)?;
            WorkspaceIdentity {
                id: id.into(),
                generation: generation.into(),
            }
        } else {
            return Err(WorkspaceFileError::Invalid);
        };
        if !valid_id(&identity.id) || !valid_id(&identity.generation) {
            return Err(WorkspaceFileError::Invalid);
        }
        Ok(Some(identity))
    }
    pub fn identity(
        &self,
        name: &WorkspaceName,
    ) -> Result<Option<WorkspaceIdentity>, WorkspaceFileError> {
        self.read_identity(&file_name(name))
    }
    fn tombstones(&self) -> Result<Vec<String>, WorkspaceFileError> {
        let mut ids = vec![];
        for (i, entry) in self.directory.entries().map_err(storage)?.enumerate() {
            if i >= max_entries() {
                return Err(WorkspaceFileError::Capacity);
            }
            let entry = entry.map_err(storage)?;
            let file = entry.file_name();
            let Some(file) = file.to_str() else { continue };
            if let Some(id) = file.strip_prefix(".wes-deleted-") {
                if !valid_id(id) {
                    return Err(WorkspaceFileError::Invalid);
                }
                let identity = self
                    .read_identity(file)?
                    .ok_or(WorkspaceFileError::Invalid)?;
                if identity.id != id {
                    return Err(WorkspaceFileError::Invalid);
                }
                ids.push(id.into());
            }
        }
        Ok(ids)
    }
    pub fn has_deletions(&self) -> Result<bool, WorkspaceFileError> {
        Ok(!self.tombstones()?.is_empty())
    }
    pub fn pending_deletions(&self) -> Result<Vec<WorkspaceDeletion>, WorkspaceFileError> {
        let mut pending = vec![];
        let mut total = 0usize;
        for id in self.tombstones()? {
            let mut options = private_options();
            options.read(true);
            let file = match self
                .directory
                .open_with(format!(".wes-delete-{id}.json"), &options)
            {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                Err(e) => return Err(storage(e)),
            };
            if !file.metadata().map_err(storage)?.is_file() {
                return Err(WorkspaceFileError::Invalid);
            }
            let mut bytes = vec![];
            file.take(8 * 1024 * 1024 + 1)
                .read_to_end(&mut bytes)
                .map_err(storage)?;
            total = total.saturating_add(bytes.len());
            if total > 8 * 1024 * 1024 {
                return Err(WorkspaceFileError::Capacity);
            }
            let plan: WorkspaceDeletion =
                serde_json::from_slice(&bytes).map_err(|_| WorkspaceFileError::Invalid)?;
            if plan.version != 1
                || plan.identity.id != id
                || self.read_identity(&format!(".wes-deleted-{id}"))?.as_ref()
                    != Some(&plan.identity)
                || WorkspaceName::new(plan.name.clone()).is_err()
                || plan.payloads.len() > 100_000
                || plan
                    .payloads
                    .iter()
                    .any(|h| wes_engine::storage::ValueHandle::new(h).is_err())
                || !plan
                    .protected
                    .iter()
                    .collect::<std::collections::BTreeSet<_>>()
                    .is_subset(&plan.payloads.iter().collect())
            {
                return Err(WorkspaceFileError::Invalid);
            }
            pending.push(plan);
        }
        Ok(pending)
    }
    pub(super) fn ensure_recreatable(
        &self,
        name: &WorkspaceName,
    ) -> Result<(), WorkspaceFileError> {
        if self
            .pending_deletions()?
            .iter()
            .any(|d| d.name == name.as_str())
        {
            return Err(storage(io::Error::other(
                "Workspace deletion cleanup is pending; reopen to retry cleanup before reusing this name",
            )));
        }
        Ok(())
    }
    pub fn delete_workspace(&mut self, plan: &WorkspaceDeletion) -> Result<(), WorkspaceFileError> {
        let name =
            WorkspaceName::new(plan.name.clone()).map_err(|_| WorkspaceFileError::Invalid)?;
        if plan.version != 1
            || self.identity(&name)?.as_ref() != Some(&plan.identity)
            || plan.payloads.len() > 100_000
            || plan
                .payloads
                .iter()
                .any(|h| wes_engine::storage::ValueHandle::new(h).is_err())
            || !plan
                .protected
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .is_subset(&plan.payloads.iter().collect())
        {
            return Err(WorkspaceFileError::Invalid);
        }
        let id = &plan.identity.id;
        if self.read_identity(&format!(".wes-deleted-{id}"))?.is_some() {
            return Err(WorkspaceFileError::Invalid);
        }
        self.check_capacity(3)?;
        let bytes = serde_json::to_vec(plan).map_err(storage)?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err(WorkspaceFileError::Capacity);
        }
        let pending = format!(".wes-pending-{}", Uuid::new_v4().simple());
        let mut options = private_options();
        options.write(true).create_new(true);
        let mut file = self
            .directory
            .open_with(&pending, &options)
            .map_err(storage)?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(storage)?;
        drop(file);
        self.directory
            .rename(&pending, &self.directory, format!(".wes-delete-{id}.json"))
            .map_err(storage)?;
        self.directory.sync().map_err(storage)?;
        // The only logical commit point. The old ID is never reused for a new name binding.
        self.directory
            .rename(
                file_name(&name),
                &self.directory,
                format!(".wes-deleted-{id}"),
            )
            .map_err(storage)?;
        self.directory.sync().map_err(WorkspaceFileError::Published)
    }
    pub fn finish_deletion(&mut self, id: &str) -> Result<(), WorkspaceFileError> {
        if !valid_id(id) || self.read_identity(&format!(".wes-deleted-{id}"))?.is_none() {
            return Err(WorkspaceFileError::Invalid);
        }
        self.directory
            .remove_file(format!(".wes-delete-{id}.json"))
            .map_err(storage)?;
        self.directory.sync().map_err(WorkspaceFileError::Published)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn temp() -> tempfile::TempDir {
        let mut b = tempfile::Builder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            b.permissions(std::fs::Permissions::from_mode(0o700));
        }
        b.tempdir().unwrap()
    }
    fn empty() -> HistoryImage {
        use wes_engine::history::{AppendReceipt, HistoryCapture, HistoryCheckpoint, Persistence};
        let r = AppendReceipt {
            persistence: Persistence::Volatile,
            end_offset: 0,
        };
        HistoryCapture::new(HistoryCaptureLimits::default()).finish(HistoryCheckpoint {
            journal: r,
            recovery: r,
        })
    }
    #[test]
    fn durable_tombstone_survives_cleanup_and_same_name_gets_new_identity() {
        let root = temp();
        let mut files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("qa".into()).unwrap();
        files.save(&name, &empty()).unwrap();
        let first = files.identity(&name).unwrap().unwrap();
        files.save(&name, &empty()).unwrap();
        let replaced = files.identity(&name).unwrap().unwrap();
        assert_eq!(first.id, replaced.id);
        assert_ne!(first.generation, replaced.generation);
        let plan = WorkspaceDeletion {
            version: 1,
            name: "qa".into(),
            identity: replaced.clone(),
            payloads: vec![],
            protected: vec![],
        };
        files.delete_workspace(&plan).unwrap();
        assert!(files.names().unwrap().is_empty());
        assert!(files.save(&name, &empty()).is_err());
        drop(files);
        let mut files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        assert!(files.has_deletions().unwrap());
        assert_eq!(files.pending_deletions().unwrap().len(), 1);
        files.collect_unused().unwrap();
        files.finish_deletion(&replaced.id).unwrap();
        assert!(files.pending_deletions().unwrap().is_empty());
        files.save(&name, &empty()).unwrap();
        assert_ne!(files.identity(&name).unwrap().unwrap().id, first.id);
        assert!(files.delete_workspace(&plan).is_err());
        assert_eq!(files.names().unwrap(), vec![name]);
    }
    #[test]
    fn uncommitted_intent_does_not_authorize_cleanup() {
        let root = temp();
        let mut files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("current".into()).unwrap();
        files.save(&name, &empty()).unwrap();
        let old = files.identity(&name).unwrap().unwrap();
        std::fs::write(
            root.path().join(format!(".wes-delete-{}.json", old.id)),
            b"partial uncommitted intent",
        )
        .unwrap();
        assert!(files.pending_deletions().unwrap().is_empty());
        assert!(!files.has_deletions().unwrap());
        assert!(files.load(&name).is_ok());
    }
    #[test]
    fn workspace_pointer_without_generation_is_rejected_without_rewriting_it() {
        let root = temp();
        let files =
            FileWorkspaces::open(root.path(), ReadLimits::default(), Durability::File).unwrap();
        let name = WorkspaceName::new("incomplete".into()).unwrap();
        let path = root.path().join(file_name(&name));
        let bytes = "wes.workspace\n1\n11111111111111111111111111111111\n";
        std::fs::write(&path, bytes).unwrap();
        assert!(files.identity(&name).is_err());
        assert_eq!(std::fs::read_to_string(path).unwrap(), bytes);
    }
}
