//! Immutable typed snapshots with bounded, checksummed Text/Bytes leaf acquisition.
use super::*;
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::{Seek, SeekFrom},
};
use wes_core::{Data, Primitive, Shape};
use wes_engine::storage::{
    CHUNK_BYTES, LeafSelection, SnapshotDescriptor, SourceRange, StoredSnapshot,
};
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    offset: u64,
    bytes: u32,
    digest: String,
}
#[derive(Clone, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Leaf {
    fields: Vec<String>,
    text: bool,
    bytes: u64,
    chunks: Vec<Chunk>,
}
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    format: String,
    version: u32,
    shell: String,
    total: u64,
    leaves: Vec<Leaf>,
}
type CapturedManifest = (Manifest, Value, Vec<u8>, String);
#[derive(Default)]
pub(super) struct ManifestCache {
    entries: HashMap<ValueHandle, (u64, cap_std::time::SystemTime, u64, Arc<CapturedManifest>)>,
    charge: u64,
}
pub(super) struct Draft {
    owner: Weak<()>,
    file: cap_std::fs::File,
    maximum: u64,
    total: u64,
    written: u64,
    ports: BTreeMap<String, Part>,
}
#[derive(Default)]
struct Part {
    bytes: u64,
    buffer: Vec<u8>,
    chunks: Vec<Chunk>,
}
fn binary(handle: &ValueHandle) -> String {
    format!("{handle}.bin")
}
fn hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn metadata_limit() -> usize {
    wes_budgets::get("storage.acquisition.metadata.bytes") as usize
}
impl Draft {
    pub(super) fn disk_charge(&self) -> u64 {
        self.total.saturating_add(self.charge())
    }
    pub(super) fn charge(&self) -> u64 {
        4096 + self
            .ports
            .values()
            .map(|p| CHUNK_BYTES as u64 + (p.chunks.len() as u64).saturating_mul(512))
            .sum::<u64>()
    }
}
impl FileValues {
    pub(super) fn acquisition_usage(&self) -> AcquisitionUsage {
        AcquisitionUsage {
            drafts: self.drafts.len(),
            payload_bytes: self.drafts.values().map(|d| d.total).sum(),
            metadata_charge: self.drafts.values().map(Draft::charge).sum(),
            buffer_bytes: self
                .drafts
                .values()
                .flat_map(|d| d.ports.values())
                .map(|p| p.buffer.capacity())
                .sum(),
        }
    }
    fn binary_file(
        &self,
        handle: &ValueHandle,
        writable: bool,
    ) -> Result<cap_std::fs::File, StoreError> {
        let mut options = private_options();
        options.read(true).write(writable);
        let file = self
            .directory
            .open_with(binary(handle), &options)
            .map_err(|e| StoreError::backend("opening segmented payload", e))?;
        if !file
            .metadata()
            .map_err(|e| StoreError::backend("checking segmented payload", e))?
            .is_file()
        {
            return Err(StoreError::NotRegular);
        }
        Ok(file)
    }
    fn manifest(&self, handle: &ValueHandle) -> Result<Option<Arc<CapturedManifest>>, StoreError> {
        let Some(mut file) = self.file(handle)? else {
            return Ok(None);
        };
        let info = file
            .metadata()
            .map_err(|e| StoreError::backend("checking snapshot header", e))?;
        let modified = info
            .modified()
            .map_err(|e| StoreError::backend("checking snapshot header identity", e))?;
        if let Some((size, at, _, captured)) = self
            .manifests
            .lock()
            .expect("manifest cache")
            .entries
            .get(handle)
        {
            if *size == info.len() && *at == modified {
                return Ok(Some(captured.clone()));
            }
        }
        let mut prefix = [0u8; 27];
        let n = file
            .read(&mut prefix)
            .map_err(|e| StoreError::backend("reading snapshot header", e))?;
        if !prefix[..n].starts_with(b"{\"format\":\"wes.segmented\"") {
            return Ok(None);
        }
        file.seek(SeekFrom::Start(0))
            .map_err(|e| StoreError::backend("seeking snapshot header", e))?;
        if file
            .metadata()
            .map_err(|e| StoreError::backend("sizing snapshot header", e))?
            .len()
            > metadata_limit() as u64
        {
            return Err(StoreError::Limit("snapshot metadata"));
        }
        let mut bytes = Vec::new();
        file.take(metadata_limit() as u64 + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| StoreError::backend("reading snapshot header", e))?;
        if bytes.len() > metadata_limit() {
            return Err(StoreError::Limit("snapshot metadata"));
        }
        let m: Manifest = serde_json::from_slice(&bytes).map_err(|_| StoreError::Conflict)?;
        if m.format != "wes.segmented"
            || m.version != 1
            || m.leaves.is_empty()
            || m.leaves.len() > 16
        {
            return Err(StoreError::Conflict);
        }
        let shell = codec::decode_value(m.shell.as_bytes(), self.limits)
            .map_err(|e| codec_error("decoding snapshot contract", e))?
            .value;
        if !shell.data().is_inline()
            || shell.shape().contains_meta()
            || shell.provenance().policy().is_private()
            || shell.provenance().policy().is_unknown()
        {
            return Err(StoreError::Restricted);
        }
        let mut offsets = Vec::new();
        let mut paths = std::collections::BTreeSet::new();
        let mut nodes = 0usize;
        for leaf in &m.leaves {
            if leaf.fields.len() > 128 || !paths.insert(leaf.fields.clone()) {
                return Err(StoreError::Conflict);
            }
            let selected = wes_engine::plan::project_value(&shell, &leaf.fields)
                .map_err(|_| StoreError::Conflict)?;
            let expected = Shape::Primitive(if leaf.text {
                Primitive::Text
            } else {
                Primitive::Bytes
            });
            if selected.shape() != &expected
                || !matches!(selected.data(), Data::Text(s) if s.is_empty())
                    && !matches!(selected.data(),Data::Bytes(b) if b.is_empty())
            {
                return Err(StoreError::Conflict);
            }
            let mut total = 0u64;
            for (index, c) in leaf.chunks.iter().enumerate() {
                nodes += 1;
                if nodes > self.limits.nodes
                    || c.bytes == 0
                    || c.bytes as usize > CHUNK_BYTES
                    || index + 1 < leaf.chunks.len() && c.bytes as usize != CHUNK_BYTES
                    || c.digest.len() != 64
                    || !c
                        .digest
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(StoreError::Conflict);
                }
                total = total
                    .checked_add(c.bytes.into())
                    .ok_or(StoreError::Conflict)?;
                offsets.push((c.offset, u64::from(c.bytes)));
            }
            if total != leaf.bytes {
                return Err(StoreError::Conflict);
            }
        }
        offsets.sort_unstable();
        let mut end = 0u64;
        for (offset, length) in offsets {
            if offset != end {
                return Err(StoreError::Conflict);
            }
            end = end.checked_add(length).ok_or(StoreError::Conflict)?;
        }
        if end != m.total
            || self
                .binary_file(handle, false)?
                .metadata()
                .map_err(|e| StoreError::backend("sizing segmented payload", e))?
                .len()
                != m.total
        {
            return Err(StoreError::Conflict);
        }
        let charge = (bytes.len() as u64)
            .saturating_mul(8)
            .saturating_add(nodes as u64 * 256)
            .saturating_add(4096);
        let digest = format!("sha256:{}", hash(&bytes));
        let captured = Arc::new((m, shell, bytes, digest));
        if charge <= metadata_limit() as u64 {
            let mut cache = self.manifests.lock().expect("manifest cache");
            if let Some((_, _, old, _)) = cache.entries.remove(handle) {
                cache.charge -= old;
            }
            if cache.charge.saturating_add(charge) > metadata_limit() as u64 {
                cache.entries.clear();
                cache.charge = 0;
            }
            cache.charge += charge;
            cache.entries.insert(
                handle.clone(),
                (info.len(), modified, charge, captured.clone()),
            );
        }
        Ok(Some(captured))
    }
    pub(super) fn segmented_size(&self, handle: &ValueHandle) -> Result<Option<u64>, StoreError> {
        Ok(self
            .manifest(handle)?
            .map(|c| c.0.total.saturating_add(c.2.len() as u64)))
    }
    pub(super) fn describe_segmented(
        &self,
        handle: &ValueHandle,
        fields: &[String],
    ) -> Result<Option<StoredSnapshot>, StoreError> {
        let Some(captured) = self.manifest(handle)? else {
            return Ok(None);
        };
        let (m, shell, _, digest) = &*captured;
        let value =
            wes_engine::plan::project_value(&shell, fields).map_err(|_| StoreError::Conflict)?;
        let mut charge = value
            .allocations(self.limits.nodes)
            .ok_or(StoreError::Limit("snapshot metadata"))?
            .iter()
            .fold(4096u64, |n, a| {
                n.saturating_add(a.bytes().saturating_add(96))
            });
        for leaf in &m.leaves {
            if leaf.fields.starts_with(fields) {
                charge =
                    charge.saturating_add(leaf.bytes.saturating_mul(if leaf.text { 6 } else { 2 }));
            }
        }
        Ok(Some(StoredSnapshot::new(SnapshotDescriptor {
            handle: handle.clone(),
            digest: digest.clone(),
            selection: fields.to_vec(),
            shape: value.shape().clone(),
            provenance: value.provenance().clone(),
            metadata: value.metadata().cloned(),
            charge,
        })))
    }
    fn read_chunk(
        &self,
        file: &mut cap_std::fs::File,
        chunk: &Chunk,
    ) -> Result<Vec<u8>, StoreError> {
        file.seek(SeekFrom::Start(chunk.offset))
            .map_err(|e| StoreError::backend("seeking snapshot chunk", e))?;
        let mut bytes = vec![0; chunk.bytes as usize];
        file.read_exact(&mut bytes)
            .map_err(|e| StoreError::backend("reading snapshot chunk", e))?;
        if hash(&bytes) != chunk.digest {
            return Err(StoreError::Conflict);
        }
        Ok(bytes)
    }
    pub(super) fn segmented_range(
        &self,
        snapshot: &StoredSnapshot,
        from: u64,
        maximum: usize,
    ) -> Result<Option<SourceRange>, StoreError> {
        let Some(captured) = self.manifest(&snapshot.handle)? else {
            return Ok(None);
        };
        let (m, _, _, digest) = &*captured;
        if digest != &snapshot.digest {
            return Err(StoreError::Conflict);
        }
        let leaf = m
            .leaves
            .iter()
            .find(|l| l.fields == snapshot.selection)
            .ok_or(StoreError::NonMaterialized)?;
        if from > leaf.bytes || maximum == 0 || maximum > CHUNK_BYTES {
            return Err(StoreError::Conflict);
        }
        let mut file = self.binary_file(&snapshot.handle, false)?;
        let end = from.saturating_add(maximum as u64).min(leaf.bytes);
        // The sealed format requires full chunks except for each leaf's final chunk. A bounded
        // range therefore reads at most two chunks and does not walk the entire prefix index.
        let first = (from / CHUNK_BYTES as u64) as usize;
        let mut position = first as u64 * CHUNK_BYTES as u64;
        let mut output = Vec::with_capacity((end - from) as usize);
        for c in leaf.chunks.iter().skip(first) {
            let next = position + u64::from(c.bytes);
            if next > from && position < end {
                let bytes = self.read_chunk(&mut file, c)?;
                output.extend_from_slice(
                    &bytes[(from.saturating_sub(position)) as usize
                        ..(end.min(next) - position) as usize],
                );
            }
            position = next;
            if position >= end {
                break;
            }
        }
        Ok(Some(SourceRange {
            bytes: output,
            extent: leaf.bytes,
            text: leaf.text,
        }))
    }
    pub(super) fn read_segmented(
        &self,
        handle: &ValueHandle,
        fields: &[String],
        limit: u64,
    ) -> Result<Option<LoadedValue>, StoreError> {
        let Some(snapshot) = self.describe_segmented(handle, fields)? else {
            return Ok(None);
        };
        if snapshot.charge > limit || snapshot.charge > self.limits.bytes as u64 {
            return Err(StoreError::Limit("segmented materialization"));
        }
        let captured = self.manifest(handle)?.ok_or(StoreError::MissingValue)?;
        let (m, shell, _, _) = &*captured;
        let selected =
            wes_engine::plan::project_value(&shell, fields).map_err(|_| StoreError::Conflict)?;
        let mut data = selected.data().clone();
        let mut file = self.binary_file(handle, false)?;
        for leaf in m.leaves.iter().filter(|l| l.fields.starts_with(fields)) {
            let mut bytes = Vec::with_capacity(
                usize::try_from(leaf.bytes)
                    .map_err(|_| StoreError::Limit("materialized payload"))?,
            );
            for c in &leaf.chunks {
                bytes.extend_from_slice(&self.read_chunk(&mut file, c)?);
            }
            let value = if leaf.text {
                Data::Text(
                    String::from_utf8(bytes)
                        .map_err(|_| StoreError::Conflict)?
                        .into(),
                )
            } else {
                Data::Bytes(bytes.into())
            };
            replace(&mut data, &leaf.fields[fields.len()..], value)?;
        }
        let value = Value::new(
            selected.shape().clone(),
            data,
            selected.provenance().clone(),
        )
        .map_err(|_| StoreError::Conflict)?
        .with_metadata(selected.metadata().cloned());
        Ok(Some(LoadedValue { value }))
    }
    pub(super) fn reap_drafts(&mut self) -> Result<(), StoreError> {
        let gone = self
            .drafts
            .iter()
            .filter(|(_, d)| d.owner.strong_count() == 0)
            .map(|(h, _)| h.clone())
            .collect::<Vec<_>>();
        for h in gone {
            self.abort_segmented(&h)?;
        }
        Ok(())
    }
    pub(super) fn begin_segmented(
        &mut self,
        maximum: u64,
        owner: Weak<()>,
    ) -> Result<ValueHandle, StoreError> {
        self.reap_drafts()?;
        if maximum == 0
            || maximum > wes_budgets::get("storage.acquisition.bytes")
            || self.drafts.len() >= wes_budgets::get("storage.operations") as usize
            || owner.strong_count() == 0
            || self
                .drafts
                .values()
                .map(Draft::charge)
                .sum::<u64>()
                .saturating_add(4096)
                > metadata_limit() as u64
        {
            return Err(StoreError::Limit("acquisition admission"));
        }
        self.admit_spill_admission(4096, 2)?;
        let handle = ValueHandle::fresh();
        let mut options = private_options();
        options.read(true).write(true).create_new(true);
        let file = self
            .directory
            .open_with(binary(&handle), &options)
            .map_err(|e| StoreError::backend("creating finite acquisition", e))?;
        self.drafts.insert(
            handle.clone(),
            Draft {
                owner,
                file,
                maximum,
                total: 0,
                written: 0,
                ports: BTreeMap::new(),
            },
        );
        Ok(handle)
    }
    pub(super) fn append_segmented(
        &mut self,
        handle: &ValueHandle,
        port: &str,
        bytes: &[u8],
    ) -> Result<(), StoreError> {
        self.reap_drafts()?;
        let d = self.drafts.get(handle).ok_or(StoreError::MissingValue)?;
        if bytes.len() > CHUNK_BYTES
            || port.is_empty()
            || port.len() > 128
            || d.total.saturating_add(bytes.len() as u64) > d.maximum
            || !d.ports.contains_key(port) && d.ports.len() >= 16
        {
            return Err(StoreError::Limit("acquired output"));
        }
        let memory = self
            .drafts
            .values()
            .fold(0u64, |n, d| n.saturating_add(d.charge()));
        if memory.saturating_add(CHUNK_BYTES as u64 + 512) > metadata_limit() as u64 {
            return Err(StoreError::Limit("acquisition metadata"));
        }
        self.admit_spill_admission(bytes.len() as u64 + 512, 0)?;
        let d = self
            .drafts
            .get_mut(handle)
            .ok_or(StoreError::MissingValue)?;
        let part = d.ports.entry(port.into()).or_insert_with(|| Part {
            buffer: Vec::with_capacity(CHUNK_BYTES),
            ..Default::default()
        });
        part.bytes += bytes.len() as u64;
        d.total += bytes.len() as u64;
        let mut remaining = bytes;
        while !remaining.is_empty() {
            let count = remaining.len().min(CHUNK_BYTES - part.buffer.len());
            part.buffer.extend_from_slice(&remaining[..count]);
            remaining = &remaining[count..];
            if part.buffer.len() == CHUNK_BYTES {
                flush(&mut d.file, &mut d.written, part)?;
            }
        }
        Ok(())
    }
    pub(super) fn abort_segmented(&mut self, handle: &ValueHandle) -> Result<(), StoreError> {
        if let Some(draft) = self.drafts.remove(handle) {
            drop(draft);
            self.directory
                .remove_file(binary(handle))
                .map_err(|e| StoreError::backend("aborting finite acquisition", e))?;
        }
        Ok(())
    }
    pub(super) fn seal_segmented(
        &mut self,
        handle: &ValueHandle,
        shell: &Value,
        selections: &[LeafSelection],
    ) -> Result<StoredSnapshot, StoreError> {
        if selections.is_empty()
            || selections.len() > 16
            || !shell.data().is_inline()
            || shell.shape().contains_meta()
            || shell.provenance().policy().is_private()
            || shell.provenance().policy().is_unknown()
        {
            return Err(StoreError::Restricted);
        }
        let shell_bytes = codec::encode_value(shell, self.limits)
            .map_err(|e| codec_error("capturing acquired contract", e))?;
        let d = self
            .drafts
            .get_mut(handle)
            .ok_or(StoreError::MissingValue)?;
        if d.owner.strong_count() == 0 {
            return Err(StoreError::MissingValue);
        }
        for part in d.ports.values_mut() {
            flush(&mut d.file, &mut d.written, part)?;
        }
        let mut leaves = Vec::new();
        let mut ports = std::collections::BTreeSet::new();
        let mut paths = std::collections::BTreeSet::new();
        for s in selections {
            let selected = wes_engine::plan::project_value(shell, &s.fields)
                .map_err(|_| StoreError::Conflict)?;
            let expected = Shape::Primitive(if s.text {
                Primitive::Text
            } else {
                Primitive::Bytes
            });
            if selected.shape() != &expected
                || !paths.insert(s.fields.clone())
                || !(matches!(selected.data(),Data::Text(t) if t.is_empty())
                    || matches!(selected.data(),Data::Bytes(b) if b.is_empty()))
            {
                return Err(StoreError::Conflict);
            }
            if !ports.insert(s.port.clone()) {
                return Err(StoreError::Conflict);
            }
            let part = d.ports.get(&s.port);
            leaves.push(Leaf {
                fields: s.fields.clone(),
                text: s.text,
                bytes: part.map_or(0, |p| p.bytes),
                chunks: part.map_or_else(Vec::new, |p| p.chunks.clone()),
            });
        }
        // Only selected public leaves enter the immutable snapshot. Discarded streams cannot
        // become a hidden Keep dependency. Copy scratch is admitted before compaction begins.
        let selected_bytes = leaves.iter().map(|l| l.bytes).sum::<u64>();
        if selected_bytes != d.total {
            self.admit_spill(selected_bytes)?;
            let pending = format!(".wes-pending-{}", Uuid::new_v4());
            let mut options = private_options();
            options.read(true).write(true).create_new(true);
            let mut target = self
                .directory
                .open_with(&pending, &options)
                .map_err(|e| StoreError::backend("compacting acquired leaves", e))?;
            let result = (|| {
                let mut source = self.binary_file(handle, false)?;
                let mut offset = 0;
                for l in &mut leaves {
                    for c in &mut l.chunks {
                        let bytes = self.read_chunk(&mut source, c)?;
                        target
                            .write_all(&bytes)
                            .map_err(|e| StoreError::backend("compacting acquired payload", e))?;
                        c.offset = offset;
                        offset += bytes.len() as u64;
                    }
                }
                target
                    .sync_all()
                    .map_err(|e| StoreError::backend("syncing compacted payload", e))?;
                Ok::<(), StoreError>(())
            })();
            if let Err(e) = result {
                let _ = self.directory.remove_file(&pending);
                return Err(e);
            }
            self.directory
                .rename(&pending, &self.directory, binary(handle))
                .map_err(|e| StoreError::backend("installing compacted payload", e))?;
            let file = self.binary_file(handle, true)?;
            let d = self.drafts.get_mut(handle).unwrap();
            d.file = file;
            d.total = selected_bytes;
        }
        let d = self.drafts.get(handle).unwrap();
        // UTF-8 is validated incrementally, including a codepoint split across physical chunks.
        let mut file = self.binary_file(handle, false)?;
        for l in leaves.iter().filter(|l| l.text) {
            let mut carry = Vec::new();
            for c in &l.chunks {
                carry.extend_from_slice(&self.read_chunk(&mut file, c)?);
                match std::str::from_utf8(&carry) {
                    Ok(_) => carry.clear(),
                    Err(e) if e.error_len().is_none() => {
                        let rest = carry.split_off(e.valid_up_to());
                        if rest.len() > 3 {
                            return Err(StoreError::Conflict);
                        }
                        carry = rest;
                    }
                    Err(_) => return Err(StoreError::Conflict),
                }
            }
            if !carry.is_empty() {
                return Err(StoreError::Conflict);
            }
        }
        d.file
            .sync_all()
            .map_err(|e| StoreError::backend("syncing acquired payload", e))?;
        let m = Manifest {
            format: "wes.segmented".into(),
            version: 1,
            shell: String::from_utf8(shell_bytes).map_err(|_| StoreError::Conflict)?,
            total: selected_bytes,
            leaves,
        };
        let encoded = serde_json::to_vec(&m).map_err(|_| StoreError::Conflict)?;
        if encoded.len() > metadata_limit() {
            return Err(StoreError::Limit("snapshot metadata"));
        }
        self.admit_spill(encoded.len() as u64)?;
        let mut options = private_options();
        options.write(true).create_new(true);
        let pending = format!(".wes-pending-{}", Uuid::new_v4());
        let mut header = self
            .directory
            .open_with(&pending, &options)
            .map_err(|e| StoreError::backend("preparing acquired header", e))?;
        if let Err(e) = header.write_all(&encoded).and_then(|()| header.sync_all()) {
            drop(header);
            let _ = self.directory.remove_file(&pending);
            return Err(StoreError::backend("preparing acquired header", e));
        }
        drop(header);
        if let Err(e) = self
            .directory
            .hard_link(&pending, &self.directory, file_name(handle))
        {
            let _ = self.directory.remove_file(&pending);
            return Err(StoreError::backend("publishing acquired header", e));
        }
        // Publication transfers ownership before the fallible durability acknowledgement.
        // An uncertain receipt must never make later draft collection delete complete bytes.
        self.drafts.remove(handle);
        self.published.insert(handle.clone(), self.next_publication);
        self.next_publication += 1;
        self.directory
            .remove_file(&pending)
            .and_then(|()| self.sync_directory())
            .map_err(|source| StoreError::Published {
                handle: handle.clone(),
                source: Box::new(source),
            })?;
        self.describe_segmented(handle, &[])?
            .ok_or(StoreError::MissingValue)
    }
    pub(super) fn copy_segmented(
        &self,
        handle: &ValueHandle,
        target: &mut FileValues,
    ) -> Result<bool, StoreError> {
        let Some(captured) = self.manifest(handle)? else {
            return Ok(false);
        };
        let (m, _, header, _) = &*captured;
        if let Some(existing) = target.manifest(handle)? {
            if &existing.2 != header {
                return Err(StoreError::Conflict);
            }
            target.confirm_segmented(handle)?;
            return Ok(true);
        }
        let pending_blob = format!(".wes-pending-{}", Uuid::new_v4());
        let pending_header = format!(".wes-pending-{}", Uuid::new_v4());
        let mut options = private_options();
        options.write(true).create_new(true);
        let mut created_blob = false;
        let mut published = false;
        let result = (|| {
            let mut output = target
                .directory
                .open_with(&pending_blob, &options)
                .map_err(|e| StoreError::backend("preparing retained payload", e))?;
            let mut source = self.binary_file(handle, false)?;
            let mut chunks = m.leaves.iter().flat_map(|l| &l.chunks).collect::<Vec<_>>();
            chunks.sort_by_key(|c| c.offset);
            for c in &chunks {
                output
                    .write_all(&self.read_chunk(&mut source, c)?)
                    .map_err(|e| StoreError::backend("copying retained payload", e))?;
            }
            output
                .sync_all()
                .map_err(|e| StoreError::backend("syncing retained payload", e))?;
            drop(output);
            match target
                .directory
                .hard_link(&pending_blob, &target.directory, binary(handle))
            {
                Ok(()) => created_blob = true,
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                    // A previous unconfirmed publication may own the binary. Verify every byte
                    // against this captured manifest; neither overwrite nor accept it by name.
                    let mut prior = target.binary_file(handle, true)?;
                    if prior
                        .metadata()
                        .map_err(|e| StoreError::backend("checking retained payload", e))?
                        .len()
                        != m.total
                    {
                        return Err(StoreError::Conflict);
                    }
                    for c in &chunks {
                        target.read_chunk(&mut prior, c)?;
                    }
                    prior
                        .sync_all()
                        .map_err(|e| StoreError::backend("confirming retained payload", e))?;
                }
                Err(e) => return Err(StoreError::backend("publishing retained payload", e)),
            }
            let mut file = target
                .directory
                .open_with(&pending_header, &options)
                .map_err(|e| StoreError::backend("preparing retained snapshot", e))?;
            file.write_all(header)
                .and_then(|()| file.sync_all())
                .map_err(|e| StoreError::backend("preparing retained snapshot", e))?;
            drop(file);
            target
                .directory
                .hard_link(&pending_header, &target.directory, file_name(handle))
                .map_err(|e| StoreError::backend("publishing retained snapshot", e))?;
            published = true;
            target
                .sync_directory()
                .map_err(|source| StoreError::Published {
                    handle: handle.clone(),
                    source: Box::new(source),
                })?;
            target
                .verify_segmented(handle)
                .map_err(|source| StoreError::Published {
                    handle: handle.clone(),
                    source: Box::new(source),
                })?;
            Ok(())
        })();
        for name in [&pending_blob, &pending_header] {
            let _ = target.directory.remove_file(name);
        }
        if result.is_err() && created_blob && !published {
            let _ = target.directory.remove_file(binary(handle));
        }
        result.map(|()| true)
    }
    pub(super) fn confirm_segmented(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        self.verify_segmented(handle)?;
        self.binary_file(handle, true)?
            .sync_all()
            .map_err(|e| StoreError::backend("confirming retained payload", e))?;
        self.confirm_existing(handle, |f| f.sync_all())
    }
    fn verify_segmented(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        let captured = self.manifest(handle)?.ok_or(StoreError::MissingValue)?;
        let (m, _, _, _) = &*captured;
        let mut file = self.binary_file(handle, false)?;
        for c in m.leaves.iter().flat_map(|l| &l.chunks) {
            self.read_chunk(&mut file, c)?;
        }
        Ok(())
    }
    pub(super) fn remove_segmented(&self, handle: &ValueHandle) -> Result<(), StoreError> {
        if self.manifest(handle)?.is_some() {
            self.directory
                .remove_file(binary(handle))
                .map_err(|source| StoreError::Released {
                    handle: handle.clone(),
                    source: Box::new(source),
                })?;
        }
        Ok(())
    }
}
fn flush(
    file: &mut cap_std::fs::File,
    written: &mut u64,
    part: &mut Part,
) -> Result<(), StoreError> {
    if part.buffer.is_empty() {
        return Ok(());
    }
    file.write_all(&part.buffer)
        .map_err(|e| StoreError::backend("writing finite acquisition", e))?;
    part.chunks.push(Chunk {
        offset: *written,
        bytes: part.buffer.len() as u32,
        digest: hash(&part.buffer),
    });
    *written += part.buffer.len() as u64;
    part.buffer.clear();
    Ok(())
}
fn replace(data: &mut Data, fields: &[String], value: Data) -> Result<(), StoreError> {
    if fields.is_empty() {
        *data = value;
        return Ok(());
    }
    let Data::Record(record) = data else {
        return Err(StoreError::Conflict);
    };
    replace(
        record.get_mut(&fields[0]).ok_or(StoreError::Conflict)?,
        &fields[1..],
        value,
    )
}
