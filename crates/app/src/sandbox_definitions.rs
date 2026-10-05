//! Definition-only persistence. No runtime value, error, run or credential is serializable here.
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    io::{Read, Write},
    path::PathBuf,
};
use wes_engine::session::sandbox::{Definition, DefinitionStore};

pub(crate) struct FileDefinitions {
    directory: PathBuf,
    name: String,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Document {
    version: u8,
    definitions: Vec<Saved>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Saved {
    name: String,
    source: String,
    owner: String,
    environment: Option<Environment>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Environment {
    selected: Option<String>,
    revisions: BTreeMap<String, String>,
}
impl FileDefinitions {
    pub(crate) fn new(
        directory: PathBuf,
        workspace: &wes_engine::workspace::WorkspaceName,
    ) -> Self {
        use sha2::{Digest, Sha256};
        Self {
            directory: directory.join(".sandbox-definitions"),
            name: format!("{:x}.json", Sha256::digest(workspace.as_str().as_bytes())),
        }
    }
    pub(crate) fn remove(&self) -> Result<(), String> {
        // Validate owned document before removing the exact leaf, never traverse user entries.
        let _ = self.load()?;
        let Some(directory) = self.directory(false)? else {
            return Ok(());
        };
        match directory.remove_file(&self.name) {
            Ok(()) => directory
                .open(".")
                .and_then(|f| f.sync_all())
                .map_err(|_| "Sandbox cleanup could not be synchronized".into()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err("Sandbox cleanup failed".into()),
        }
    }
    fn directory(&self, create: bool) -> Result<Option<cap_std::fs::Dir>, String> {
        match std::fs::symlink_metadata(&self.directory) {
            Ok(m) if !m.is_dir() || m.file_type().is_symlink() => {
                return Err("Invalid sandbox definition directory".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !create => return Ok(None),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                std::fs::create_dir_all(&self.directory)
                    .map_err(|_| "Cannot create sandbox definition directory")?
            }
            Err(_) => return Err("Cannot access sandbox definition directory".into()),
        }
        cap_std::fs::Dir::open_ambient_dir(&self.directory, cap_std::ambient_authority())
            .map(Some)
            .map_err(|_| "Cannot open sandbox definition directory".into())
    }
}
impl DefinitionStore for FileDefinitions {
    fn load(&self) -> Result<Vec<Definition>, String> {
        let Some(directory) = self.directory(false)? else {
            return Ok(vec![]);
        };
        match directory.symlink_metadata(&self.name) {
            Ok(m) if !m.is_file() || m.file_type().is_symlink() => {
                return Err("Invalid sandbox definitions".into());
            }
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(vec![]),
            Err(_) => return Err("Cannot read sandbox definitions".into()),
        }
        let file = directory
            .open(&self.name)
            .map_err(|_| "Cannot open sandbox definitions")?;
        let mut bytes = vec![];
        file.take(8 * 1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| "Cannot read sandbox definitions")?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Sandbox definitions exceed their budget".into());
        }
        let document: Document =
            serde_json::from_slice(&bytes).map_err(|_| "Invalid sandbox definitions")?;
        if document.version != 1 {
            return Err("Unsupported sandbox definition version".into());
        }
        document
            .definitions
            .into_iter()
            .map(|s| {
                Ok(Definition {
                    name: s.name,
                    source: s.source,
                    owner: s.owner,
                    environment: s
                        .environment
                        .map(|e| {
                            Ok::<_, String>(wes_core::environments::EnvironmentContext {
                                selected: e.selected,
                                revisions: e
                                    .revisions
                                    .into_iter()
                                    .map(|(n, r)| {
                                        Ok((
                                            n,
                                            r.parse()
                                                .map_err(|_| "Invalid environment revision")?,
                                        ))
                                    })
                                    .collect::<Result<_, String>>()?,
                            })
                        })
                        .transpose()?,
                })
            })
            .collect()
    }
    fn save(&self, definitions: &[Definition]) -> Result<(), String> {
        let document = Document {
            version: 1,
            definitions: definitions
                .iter()
                .map(|d| Saved {
                    name: d.name.clone(),
                    source: d.source.clone(),
                    owner: d.owner.clone(),
                    environment: d.environment.as_ref().map(|e| Environment {
                        selected: e.selected.clone(),
                        revisions: e
                            .revisions
                            .iter()
                            .map(|(n, r)| (n.clone(), r.to_string()))
                            .collect(),
                    }),
                })
                .collect(),
        };
        let bytes =
            serde_json::to_vec(&document).map_err(|_| "Cannot encode sandbox definitions")?;
        if bytes.len() > 8 * 1024 * 1024 {
            return Err("Sandbox definitions exceed their budget".into());
        }
        let directory = self.directory(true)?.expect("created");
        if let Ok(m) = directory.symlink_metadata(&self.name)
            && (!m.is_file() || m.file_type().is_symlink())
        {
            return Err("Invalid sandbox definition destination".into());
        }
        let temporary = format!(".{}.tmp", uuid::Uuid::new_v4());
        let mut options = cap_std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use cap_std::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let result = (|| {
            let mut file = directory
                .open_with(&temporary, &options)
                .map_err(|_| "Cannot stage sandbox definitions")?;
            file.write_all(&bytes)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Cannot sync sandbox definitions")?;
            directory
                .rename(&temporary, &directory, &self.name)
                .map_err(|_| "Cannot publish sandbox definitions")?;
            wes_adapters::sync_directory(&directory)
                .map_err(|_| "Sandbox definitions published but directory sync failed")?;
            Ok(())
        })();
        let _ = directory.remove_file(&temporary);
        result
    }
}
