//! Portable exact replay evidence, never live connections, values or grants.
use super::*;
use crate::codec::{
    Limits,
    history::{decode_journal, encode_journal},
};
use std::io::Write;
use wes_engine::{
    environments::{EnvironmentRecord, Registry},
    history::JournalEntry,
};

const FORMAT: &str = "wes/environment-lock/v1";
fn byte_budget() -> usize {
    wes_budgets::get("environment.lock.bytes") as usize
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Lock {
    format: String,
    records: Vec<Box<serde_json::value::RawValue>>,
}

pub(super) fn read(
    loader: &LocalEnvironments,
    path: &str,
) -> Result<Vec<EnvironmentRecord>, EnvironmentError> {
    let bytes = read_input(&loader.files, path, byte_budget(), "environment lock")?;
    let lock: Lock =
        serde_json::from_str(&bytes).map_err(|_| invalid("invalid environment lock format"))?;
    if lock.format != FORMAT || lock.records.len() > 1024 {
        return Err(invalid("unsupported or excessive environment lock"));
    }
    let mut registry = Registry::default();
    let mut result = vec![];
    let mut ids = std::collections::BTreeSet::new();
    for raw in lock.records {
        let JournalEntry::Environments(record) =
            decode_journal(raw.get().as_bytes(), Limits::default())
                .map_err(|_| invalid("invalid lock evidence"))?
                .entry
        else {
            return Err(invalid("lock may contain environment definitions only"));
        };
        if !ids.insert(record.id().to_owned()) {
            return Err(invalid("duplicate lock publication identity"));
        }
        let plan = record.prepare(&registry)?;
        registry.apply(plan)?;
        for name in registry.names() {
            let environment = registry.inspect(name).expect("registered");
            if !environment.is_abstract() {
                for alias in environment.imports().keys() {
                    loader.build(alias, &environment.bind(alias)?)?;
                }
            }
        }
        result.push(record);
    }
    Ok(result)
}
pub(super) fn export(
    loader: &LocalEnvironments,
    path: &str,
    records: &[EnvironmentRecord],
) -> Result<(), EnvironmentError> {
    if records.len() > 1024 {
        return Err(invalid("lock publication budget exceeded"));
    }
    let mut bytes = 128;
    let mut entries = vec![];
    for record in records {
        let encoded = encode_journal(
            &JournalEntry::Environments(record.clone()),
            Limits::default(),
        )
        .map_err(|_| invalid("lock encoding failed"))?;
        bytes += encoded.len() + 1;
        if bytes > byte_budget() {
            return Err(invalid("lock exceeds 64 MiB"));
        }
        entries.push(encoded);
    }
    let path = loader
        .files
        .resolve(path)
        .map_err(|_| invalid("invalid export destination"))?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| {
        invalid("export requires a new file; existing destinations are never overwritten")
    })?;
    let write = || -> std::io::Result<()> {
        write!(file, "{{\"format\":\"{FORMAT}\",\"records\":[")?;
        for (i, entry) in entries.iter().enumerate() {
            if i > 0 {
                file.write_all(b",")?;
            }
            file.write_all(entry)?;
        }
        file.write_all(b"]}\n")?;
        file.sync_all()
    };
    let mut write = write;
    write().map_err(|_| {
        invalid("export write failed; the newly created destination may be incomplete")
    })
}
