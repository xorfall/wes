//! Immutable retained record membership. Projection precedes payload materialization.
use super::*;
use sha2::{Digest, Sha256};
use wes_core::{Data, Provenance, RecordShape, Shape};
use wes_engine::storage::{SnapshotDescriptor, SnapshotMember, SourceRange, StoredSnapshot};
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Composite {
    format: String,
    version: u32,
    pub(super) members: Vec<SnapshotMember>,
}
thread_local! { static TRAVERSAL: std::cell::Cell<(usize,usize)> = const { std::cell::Cell::new((0,0)) }; }
struct Guard;
impl Guard {
    fn enter(limits: Limits) -> Result<Self, StoreError> {
        TRAVERSAL.with(|state| {
            let (depth, work) = state.get();
            let work = if depth == 0 { 1 } else { work + 1 };
            if depth >= 128 || work > limits.nodes {
                return Err(StoreError::Limit("snapshot composition traversal"));
            }
            state.set((depth + 1, work));
            Ok(Self)
        })
    }
}
impl Drop for Guard {
    fn drop(&mut self) {
        TRAVERSAL.with(|s| {
            let (d, w) = s.get();
            s.set((d - 1, w));
        });
    }
}
fn hash(bytes: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(bytes))
}
impl FileValues {
    pub(super) fn composite_holds(
        &mut self,
        handle: &ValueHandle,
    ) -> Result<Vec<ValueLease>, StoreError> {
        let _guard = Guard::enter(self.limits)?;
        let Some((root, _)) = self.composite_header(handle)? else {
            return Ok(vec![]);
        };
        let mut holds = Vec::new();
        for member in &root.members {
            let snapshot = self.member_snapshot(member, &[])?;
            holds.push(
                self.hold(&snapshot.handle)?
                    .ok_or(StoreError::MissingValue)?,
            );
        }
        Ok(holds)
    }
    pub(super) fn composite_header(
        &self,
        handle: &ValueHandle,
    ) -> Result<Option<(Composite, Vec<u8>)>, StoreError> {
        let Some(bytes) = self.snapshot_header(
            handle,
            b"{\"format\":\"wes.composite\"",
            wes_budgets::get("storage.acquisition.metadata.bytes") as usize,
        )?
        else {
            return Ok(None);
        };
        let root: Composite = serde_json::from_slice(&bytes).map_err(|_| StoreError::Corrupt)?;
        if root.format != "wes.composite"
            || root.version != 1
            || root.members.len() > wes_budgets::get("values.outputs") as usize
        {
            return Err(StoreError::Corrupt);
        }
        let mut names = std::collections::BTreeSet::new();
        for m in &root.members {
            let h = ValueHandle::new(&m.handle).map_err(|_| StoreError::Corrupt)?;
            if &h == handle
                || m.name.is_empty()
                || m.name.len() > 1024
                || !names.insert(&m.name)
                || m.selection.len() > 128
                || m.selection.iter().any(|s| s.is_empty() || s.len() > 1024)
                || !m.digest.strip_prefix("sha256:").is_some_and(|s| {
                    s.len() == 64
                        && s.bytes()
                            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                })
            {
                return Err(StoreError::Corrupt);
            }
        }
        Ok(Some((root, bytes)))
    }
    fn member_snapshot(
        &self,
        m: &SnapshotMember,
        fields: &[String],
    ) -> Result<StoredSnapshot, StoreError> {
        let mut selection = m.selection.clone();
        selection.extend_from_slice(fields);
        let snapshot = self
            .describe(&ValueHandle::new(&m.handle)?, &selection)?
            .ok_or(StoreError::MissingValue)?;
        if snapshot.digest != m.digest
            || snapshot.provenance.policy().is_private()
            || snapshot.provenance.policy().is_unknown()
            || snapshot.shape.contains_meta()
        {
            return Err(StoreError::Restricted);
        }
        Ok(snapshot)
    }
    pub(super) fn describe_composite(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let _guard = Guard::enter(self.limits)?;
        let Some((root, bytes)) = self.composite_header(handle)? else {
            return Ok(None);
        };
        let mut children = Vec::new();
        let mut charge = 4096u64;
        for m in &root.members {
            let s = self.member_snapshot(m, &[])?;
            charge = charge.saturating_add(s.charge);
            children.push((m.name.clone(), s));
        }
        let provenance = Provenance::agreed_by(children.iter().map(|(_, s)| &s.provenance));
        let (shape, metadata, charge) = if fields.is_empty() {
            (
                Shape::Record(
                    RecordShape::new(
                        "Record",
                        children.iter().map(|(n, s)| (n.clone(), s.shape.clone())),
                    )
                    .map_err(|_| StoreError::Conflict)?,
                ),
                None,
                charge,
            )
        } else {
            let m = root
                .members
                .iter()
                .find(|m| m.name == fields[0])
                .ok_or(StoreError::Conflict)?;
            let s = self.member_snapshot(m, &fields[1..])?;
            (s.shape.clone(), s.metadata.clone(), s.charge)
        };
        let mut descriptor = SnapshotDescriptor {
            handle: handle.clone(),
            digest: hash(&bytes),
            selection: fields.to_vec(),
            shape,
            provenance,
            metadata,
            charge,
        };
        // A projected child retains the complete record's permission/provenance. Its original
        // decoded charge alone may omit that joined metadata, even for a tiny scalar field.
        descriptor.charge = descriptor
            .charge
            .saturating_add(descriptor.metadata_charge());
        let snapshot = StoredSnapshot::new(descriptor);
        if snapshot.metadata_charge() > wes_budgets::get("storage.acquisition.metadata.bytes") {
            return Err(StoreError::Limit("record snapshot contract"));
        }
        Ok(Some(snapshot))
    }
    pub(super) fn read_composite(
        &self,
        handle: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        let _guard = Guard::enter(self.limits)?;
        let Some(snapshot) = self.describe_composite(handle, fields)? else {
            return Ok(None);
        };
        if snapshot.charge > limit || snapshot.charge > self.limits.bytes as u64 {
            return Err(StoreError::Limit("record snapshot materialization"));
        }
        let (root, _) = self
            .composite_header(handle)?
            .ok_or(StoreError::MissingValue)?;
        let data = if fields.is_empty() {
            let mut record = indexmap::IndexMap::new();
            for m in &root.members {
                let s = self.member_snapshot(m, &[])?;
                let v = self
                    .read_selected(&s.handle, &s.selection, s.charge.min(limit))?
                    .ok_or(StoreError::MissingValue)?
                    .value;
                if !s.matches(&v) {
                    return Err(StoreError::Conflict);
                }
                record.insert(m.name.clone(), v.into_data());
            }
            Data::Record(record)
        } else {
            let m = root
                .members
                .iter()
                .find(|m| m.name == fields[0])
                .ok_or(StoreError::Conflict)?;
            let s = self.member_snapshot(m, &fields[1..])?;
            let v = self
                .read_selected(&s.handle, &s.selection, limit)?
                .ok_or(StoreError::MissingValue)?
                .value;
            if !s.matches(&v) {
                return Err(StoreError::Conflict);
            }
            v.into_data()
        };
        let value = Value::new(snapshot.shape.clone(), data, snapshot.provenance.clone())
            .map_err(|_| StoreError::Conflict)?
            .with_metadata(snapshot.metadata.clone());
        Ok(Some(LoadedValue { value }))
    }
    pub(super) fn composite_range(
        &self,
        snapshot: &StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<Option<SourceRange>, StoreError> {
        let _guard = Guard::enter(self.limits)?;
        let Some((root, bytes)) = self.composite_header(&snapshot.handle)? else {
            return Ok(None);
        };
        if hash(&bytes) != snapshot.digest || snapshot.selection.is_empty() {
            return Err(StoreError::Conflict);
        }
        let m = root
            .members
            .iter()
            .find(|m| m.name == snapshot.selection[0])
            .ok_or(StoreError::Conflict)?;
        let child = self.member_snapshot(m, &snapshot.selection[1..])?;
        self.source_range(&child, from, maximum).map(Some)
    }
    pub(super) fn compose_retained(
        &mut self,
        handle: &ValueHandle,
        members: &[SnapshotMember],
    ) -> Result<StoredSnapshot, StoreError> {
        let root = Composite {
            format: "wes.composite".into(),
            version: 1,
            members: members.to_vec(),
        };
        for m in &root.members {
            self.member_snapshot(m, &[])?;
        }
        let bytes = serde_json::to_vec(&root).map_err(|_| StoreError::Conflict)?;
        if bytes.len() as u64 > wes_budgets::get("storage.acquisition.metadata.bytes") {
            return Err(StoreError::Limit("record snapshot metadata"));
        }
        if let Some((_, existing)) = self.composite_header(handle)? {
            if existing != bytes {
                return Err(StoreError::Conflict);
            }
            self.confirm_composite(handle)?;
        } else {
            if self.file(handle)?.is_some() {
                return Err(StoreError::Conflict);
            }
            let pending = format!(".wes-pending-{}", Uuid::new_v4());
            let mut options = private_options();
            options.write(true).create_new(true);
            let mut file = self
                .directory
                .open_with(&pending, &options)
                .map_err(|e| StoreError::backend("preparing record snapshot", e))?;
            let result = (|| {
                let protected = self.protect_object(handle, &bytes)?;
                file.write_all(&protected)
                    .and_then(|()| file.sync_all())
                    .map_err(|e| StoreError::backend("syncing record snapshot", e))?;
                self.directory
                    .hard_link(&pending, &self.directory, file_name(handle))
                    .map_err(|e| StoreError::backend("publishing record snapshot", e))?;
                self.sync_directory()
                    .map_err(|source| StoreError::Published {
                        handle: handle.clone(),
                        source: Box::new(source),
                    })
            })();
            drop(file);
            let _ = self.directory.remove_file(&pending);
            result?;
        }
        self.describe_composite(handle, &[])?
            .ok_or(StoreError::MissingValue)
    }
    pub(super) fn confirm_composite(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        self.describe_composite(handle, &[])?
            .ok_or(StoreError::MissingValue)?;
        self.confirm_existing(handle, |file| file.sync_all())
    }
}
