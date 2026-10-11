//! Weak witnesses for immutable payload allocations. They retain no payload bytes.
use super::*;
use std::{mem::size_of, sync::Weak};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AllocationKey(u8, usize);

#[derive(Clone)]
enum Owner {
    Shared(Weak<dyn Send + Sync>),
    Text(Weak<str>),
    Bytes(Weak<[u8]>),
}
#[derive(Clone)]
pub struct Allocation {
    key: AllocationKey,
    bytes: u64,
    owner: Owner,
}
impl fmt::Debug for Allocation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Allocation")
            .field("bytes", &self.bytes)
            .field("alive", &self.alive())
            .finish()
    }
}
impl Allocation {
    pub fn key(&self) -> AllocationKey {
        self.key
    }
    pub fn bytes(&self) -> u64 {
        self.bytes
    }
    pub fn alive(&self) -> bool {
        match &self.owner {
            Owner::Shared(owner) => owner.strong_count() != 0,
            Owner::Text(owner) => owner.strong_count() != 0,
            Owner::Bytes(owner) => owner.strong_count() != 0,
        }
    }
    pub fn shared<T: Send + Sync + 'static>(owner: &Arc<T>, bytes: u64) -> Self {
        let weak: Weak<T> = Arc::downgrade(owner);
        let weak: Weak<dyn Send + Sync> = weak;
        Self {
            key: AllocationKey(0, Arc::as_ptr(owner) as usize),
            bytes: bytes.saturating_add(32),
            owner: Owner::Shared(weak),
        }
    }
}
impl Value {
    /// Conservative allocation charge, deduplicable by identity. Containers are charged to their
    /// owning root; independently shared Text/Bytes allocations remain live after field projection.
    /// This is neither an allocator counter nor an RSS guarantee. Failure refuses excessive work.
    pub fn allocations(&self, maximum: usize) -> Option<Vec<Allocation>> {
        let mut output = Vec::new();
        let mut containers = size_of::<Data>() as u64;
        let mut pending = vec![(self.data(), 0usize)];
        let mut nodes = 0usize;
        while let Some((data, depth)) = pending.pop() {
            nodes = nodes.checked_add(1)?;
            if nodes > maximum || depth > 256 {
                return None;
            }
            match data {
                Data::Text(text) => output.push(Allocation {
                    key: AllocationKey(1, Arc::as_ptr(text) as *const u8 as usize),
                    bytes: (text.len() as u64).saturating_add(32),
                    owner: Owner::Text(Arc::downgrade(text)),
                }),
                Data::Bytes(bytes) => output.push(Allocation {
                    key: AllocationKey(2, Arc::as_ptr(bytes) as *const u8 as usize),
                    bytes: (bytes.len() as u64).saturating_add(32),
                    owner: Owner::Bytes(Arc::downgrade(bytes)),
                }),
                Data::List(items) => {
                    containers = containers.checked_add(
                        (items.capacity() as u64).checked_mul(size_of::<Data>() as u64)?,
                    )?;
                    if nodes
                        .saturating_add(pending.len())
                        .saturating_add(items.len())
                        > maximum
                    {
                        return None;
                    }
                    pending.extend(items.iter().map(|d| (d, depth + 1)));
                }
                Data::Record(fields) => {
                    containers = containers.checked_add(
                        (fields.capacity() as u64)
                            .checked_mul((size_of::<(String, Data)>() + 48) as u64)?,
                    )?;
                    if nodes
                        .saturating_add(pending.len())
                        .saturating_add(fields.len())
                        > maximum
                    {
                        return None;
                    }
                    for (name, value) in fields {
                        containers = containers.checked_add(name.capacity() as u64)?;
                        pending.push((value, depth + 1));
                    }
                }
                Data::Option(Some(value)) => {
                    containers = containers.checked_add(size_of::<Data>() as u64)?;
                    pending.push((value, depth + 1));
                }
                Data::Decimal(value) => {
                    containers =
                        containers.checked_add(value.compact_text_size_bound().saturating_mul(2))?
                }
                // Recipes and descriptors own additional immutable objects. Their existing logical
                // work limits remain authoritative; account their full conservative payload here.
                Data::Iter(value) => {
                    output.push(Allocation::shared(value, 256));
                    output.extend(value.source().allocations(maximum.checked_sub(nodes)?)?);
                }
                Data::Dataset(value) => output.push(Allocation::shared(
                    value,
                    256 + [
                        value.store(),
                        value.dataset(),
                        value.manifest(),
                        value.manifest_digest(),
                        value.schema_digest(),
                    ]
                    .into_iter()
                    .map(|s| s.len() as u64)
                    .sum::<u64>(),
                )),
                _ => {}
            }
        }
        output.push(Allocation::shared(&self.data, containers));
        // Shapes and attribution have independently shared allocations. Bound their traversal
        // without formatting payloads; small wrapper overhead is deliberately conservative.
        let mut shapes = vec![(self.shape(), 0usize)];
        let mut shape_bytes = size_of::<Shape>() as u64;
        while let Some((shape, depth)) = shapes.pop() {
            nodes = nodes.checked_add(1)?;
            if nodes > maximum || depth > 256 {
                return None;
            }
            match shape {
                Shape::List(s) | Shape::Option(s) | Shape::Dataset(s) | Shape::Iter(s) => {
                    shape_bytes = shape_bytes.checked_add(size_of::<Shape>() as u64)?;
                    shapes.push((s, depth + 1));
                }
                Shape::Record(record) => {
                    if nodes
                        .saturating_add(shapes.len())
                        .saturating_add(record.fields().len())
                        > maximum
                    {
                        return None;
                    }
                    shape_bytes = shape_bytes.checked_add(
                        record.name().len() as u64
                            + (record.fields().len() as u64).saturating_mul(128),
                    )?;
                    for (name, shape) in record.fields() {
                        shape_bytes = shape_bytes.checked_add(name.len() as u64)?;
                        shapes.push((shape, depth + 1));
                    }
                }
                _ => {}
            }
        }
        output.push(Allocation::shared(&self.shape, shape_bytes));
        let mut provenance_bytes = 256u64;
        for (key, value) in self.provenance.facts() {
            provenance_bytes =
                provenance_bytes.checked_add(128 + key.len() as u64 + value.len() as u64)?;
        }
        for caution in self.provenance.cautions() {
            provenance_bytes = provenance_bytes.checked_add(64 + caution.len() as u64)?;
        }
        for origin in self.provenance.policy().origins() {
            provenance_bytes = provenance_bytes.checked_add(64 + origin.len() as u64)?;
        }
        for origin in self.provenance.policy().dataset_reads() {
            provenance_bytes = provenance_bytes
                .checked_add(128 + origin.store().len() as u64 + origin.dataset().len() as u64)?;
        }
        output.push(Allocation::shared(&self.provenance, provenance_bytes));
        if let Some(metadata) = &self.metadata {
            output.push(Allocation::shared(metadata, metadata.charge()));
        }
        if let Some(authority) = &self.authority {
            output.push(Allocation {
                key: AllocationKey(1, Arc::as_ptr(authority) as *const u8 as usize),
                bytes: authority.len() as u64 + 32,
                owner: Owner::Text(Arc::downgrade(authority)),
            });
        }
        (output.len() <= maximum).then_some(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn weak_witnesses_follow_shared_leaf_after_parent_is_released() {
        let bytes: Arc<[u8]> = vec![4; 4096].into();
        let parent = Value::new(
            Shape::Unknown,
            Data::Record(indexmap::IndexMap::from([(
                "body".into(),
                Data::Bytes(bytes.clone()),
            )])),
            Provenance::default(),
        )
        .unwrap();
        let witnesses = parent.allocations(1024).unwrap();
        let leaf = witnesses
            .iter()
            .find(|a| a.bytes() >= 4096)
            .unwrap()
            .clone();
        let child = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(bytes),
            Provenance::default(),
        )
        .unwrap();
        drop(parent);
        assert!(leaf.alive());
        assert!(
            child
                .allocations(1024)
                .unwrap()
                .iter()
                .any(|a| a.key() == leaf.key())
        );
        drop(child);
        assert!(!leaf.alive(), "tracking must not keep the bytes alive");
    }
}
