//! Shared application service for library settings, repository resolution and reviewed ingestion.
//! Called on owned blocking workers; engine preparation/replay never starts repository/model work.
mod command;
pub use command::use_bundled_resources;
mod describe;
mod failures;
mod provenance;
pub(crate) mod settings;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
use wes_adapters::api_library::{self as io_layer, *};

#[derive(Clone)]
pub struct ApiLibrary {
    home: PathBuf,
    // Host-only grants, shared by web and terminal services; never persisted.
    draft_grants: std::sync::Arc<std::sync::Mutex<Vec<(PathBuf, PackageKey)>>>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
pub struct Settings {
    pub local_directory: PathBuf,
    #[serde(default)]
    pub repository: Option<Repository>,
    #[serde(default)]
    pub extractor: Option<PathBuf>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Saved {
    pub(crate) version: u32,
    pub(crate) settings: Settings,
}
#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum Request {
    DraftAccess {
        key: PackageKey,
        enabled: Option<bool>,
    },
    DescribeFailure {
        id: String,
    },
    ListDrafts,
    InspectDraft {
        key: PackageKey,
        revision: String,
    },
    ValidateDraft {
        text: String,
    },
    SaveDraft {
        key: PackageKey,
        revision: String,
        text: String,
    },
    ReviewDraft {
        key: PackageKey,
        revision: String,
    },
    ExportDraft {
        key: PackageKey,
        revision: String,
        file: String,
    },
    ValidateDescriptor {
        text: String,
    },
    SaveDescriptor {
        key: PackageKey,
        revision: String,
        text: String,
    },
    ExportDescriptor {
        key: PackageKey,
        revision: String,
        file: String,
    },
    Status,
    Configure {
        settings: Settings,
        #[serde(rename = "expectedRevision")]
        expected_revision: Option<String>,
    },
    List,
    Catalog,
    Resolve {
        key: PackageKey,
        #[serde(default)]
        revision: Option<String>,
        #[serde(default)]
        source: Option<Extraction>,
    },
    Ingest {
        key: PackageKey,
        source: Extraction,
    },
    Add {
        key: PackageKey,
        file: String,
    },
    Inspect {
        key: PackageKey,
        revision: String,
    },
    Accept {
        key: PackageKey,
        revision: String,
    },
    Compare {
        key: PackageKey,
        before: String,
        after: String,
    },
    Recipe {
        key: PackageKey,
        revision: String,
        environment: String,
        alias: String,
        endpoint: String,
        #[serde(default)]
        credentials: BTreeMap<String, String>,
        #[serde(default)]
        auth: BTreeMap<String, Vec<String>>,
    },
}
impl ApiLibrary {
    pub fn new(home: PathBuf) -> Self {
        Self {
            home,
            draft_grants: Default::default(),
        }
    }
    fn settings(&self) -> Result<Option<Settings>> {
        settings::load(&self.home)
    }
    fn configuration_revision(settings: &Settings) -> Result<String> {
        Ok(digest(
            &serde_json::to_vec(settings).map_err(std::io::Error::other)?,
        ))
    }
    fn status(&self, settings: Option<Settings>) -> Result<Value> {
        let revision = settings
            .as_ref()
            .map(Self::configuration_revision)
            .transpose()?;
        Ok(json!({"settings":settings,"revision":revision,"managed":true}))
    }
    fn configure(&self, settings: Settings, expected: Option<String>) -> Result<Value> {
        let current = self.settings()?;
        if current
            .as_ref()
            .map(Self::configuration_revision)
            .transpose()?
            != expected
        {
            return Err(error("library settings changed; reload before saving"));
        }
        settings::validate(&self.home, &settings)?;
        if let Some(executable) = &settings.extractor
            && (!executable.is_absolute() || !executable.is_file())
        {
            return Err(error(
                "extractor must be an existing absolute executable path",
            ));
        }
        if let Some(repository) = &settings.repository {
            repository.validate()?;
            if let Repository::Local { directory } = repository
                && (overlap(directory, &settings.local_directory) || overlap(directory, &self.home))
            {
                return Err(error(
                    "repository must not overlap the local library or application data home",
                ));
            }
        }
        let _library = Library::open(&settings.local_directory)?;
        settings::store(&self.home, &settings)?;
        self.status(Some(settings))
    }
    /// One transaction across settings/index/object publication. The OS lock covers other clients
    /// and processes, not merely this Rust value. Busy operations fail instead of blocking forever.
    pub fn perform(&self, request: Request) -> Result<Value> {
        self.perform_scoped(request, false)
    }
    /// Narrow agent boundary. Caller cannot acquire grants or reach other library actions.
    pub fn perform_agent(&self, request: Request) -> Result<Value> {
        if !matches!(
            request,
            Request::ListDrafts | Request::InspectDraft { .. } | Request::SaveDraft { .. }
        ) {
            return Err(error("This API library action requires user controls."));
        }
        self.perform_scoped(request, true)
    }
    fn perform_scoped(&self, request: Request, agent: bool) -> Result<Value> {
        // Text validation has no filesystem or publication responsibility.
        match &request {
            Request::ValidateDraft { text } => {
                return serde_json::to_value(io_layer::draft::validate(text))
                    .map_err(std::io::Error::other);
            }
            Request::ValidateDescriptor { text } => {
                return match validate_descriptor(text.as_bytes()) {
                    Ok(warnings) => Ok(json!({"valid":true,"diagnostics":warnings})),
                    Err(e) => Ok(json!({"valid":false,"diagnostics":[e.to_string()]})),
                };
            }
            _ => (),
        }
        validate_directory(&self.home)?;
        let _configuration = exclusive_lock(&self.home, ".api-library.lock")?;
        match &request {
            Request::DescribeFailure { id } => return self.read_failure(id),
            _ => (),
        }
        if matches!(request, Request::Status) {
            return self.status(self.settings()?);
        }
        if let Request::Configure {
            settings,
            expected_revision,
        } = request
        {
            return self.configure(settings, expected_revision);
        }
        let settings = self.settings()?.ok_or_else(|| error("choose a local API library directory in Settings or configure it through --api-request first"))?;
        let mut library = Library::open(&settings.local_directory)?;
        // Lock ordering is configuration -> library -> grants for every caller. Holding
        // the grant guard through the transaction linearizes revocation with admitted saves.
        let grants = agent.then(|| self.draft_grants.lock().expect("draft grants"));
        if let Some(grants) = &grants {
            let key = match &request {
                Request::InspectDraft { key, .. } | Request::SaveDraft { key, .. } => Some(key),
                _ => None,
            };
            if key.is_some_and(|key| {
                !grants.iter().any(|(directory, allowed)| {
                    directory == &settings.local_directory && allowed == key
                })
            }) {
                return Err(error(
                    "Draft access is not shared. Enable agent access for this draft in /spec.",
                ));
            }
        }
        let runtime = tokio::runtime::Handle::current();
        match request {
            Request::Status
            | Request::DescribeFailure { .. }
            | Request::Configure { .. }
            | Request::ValidateDraft { .. }
            | Request::ValidateDescriptor { .. } => unreachable!(),
            Request::DraftAccess { key, enabled } => {
                key.validate()?;
                if !library.drafts()?.iter().any(|draft| draft.key == key) {
                    return Err(error("Draft not found."));
                }
                let mut grants = self.draft_grants.lock().expect("draft grants");
                let entry = (settings.local_directory, key);
                if let Some(enabled) = enabled {
                    grants.retain(|grant| grant != &entry);
                    if enabled {
                        grants.push(entry.clone());
                    }
                }
                Ok(json!({"enabled":grants.contains(&entry)}))
            }
            Request::ListDrafts => {
                let mut drafts = library.drafts()?;
                if let Some(grants) = &grants {
                    drafts.retain(|draft| {
                        grants.iter().any(|(directory, key)| {
                            directory == &settings.local_directory && key == &draft.key
                        })
                    });
                }
                Ok(json!({"drafts":drafts}))
            }
            Request::InspectDraft { key, revision } => library.inspect_draft(&key, &revision),
            Request::SaveDraft {
                key,
                revision,
                text,
            } => library.save_draft(key, &revision, text),
            Request::ReviewDraft { key, revision } => library.review_draft(&key, &revision),
            Request::ExportDraft {
                key,
                revision,
                file,
            } => {
                let result = library.inspect_draft(&key, &revision)?;
                let descriptor_revision = result["draft"]["descriptorRevision"]
                    .as_str()
                    .filter(|_| result["validation"]["valid"] == true)
                    .ok_or_else(|| error("Resolve draft problems and save before exporting"))?;
                let path = describe::Export::prepare(
                    Path::new(&file),
                    &library.descriptor(descriptor_revision)?,
                )?
                .publish()?;
                Ok(json!({"exportedPath":path}))
            }
            Request::SaveDescriptor {
                key,
                revision,
                text,
            } => {
                let previous = required(&library, &key, &revision)?;
                if library
                    .find(&key, None)
                    .is_none_or(|p| p.revision != revision)
                {
                    return Err(error(
                        "This spec changed; reload its latest revision before saving",
                    ));
                }
                let edited = provenance::edited(text.as_bytes())?;
                let package = library.save(
                    key,
                    &edited,
                    format!("edited:{}", previous.revision),
                    None,
                    false,
                )?;
                package_result(&library, package, "edited")
            }
            Request::ExportDescriptor {
                key,
                revision,
                file,
            } => {
                required(&library, &key, &revision)?;
                let bytes = library.descriptor(&revision)?;
                let path = describe::Export::prepare(Path::new(&file), &bytes)?.publish()?;
                Ok(json!({"exportedPath":path,"revision":revision}))
            }
            Request::List => {
                Ok(json!({"packages":library.index.packages,"drafts":library.drafts()?}))
            }
            Request::Catalog => {
                let catalog = settings
                    .repository
                    .as_ref()
                    .map(|r| runtime.block_on(r.catalog()))
                    .transpose()?;
                Ok(json!({"repository":settings.repository,"catalog":catalog}))
            }
            Request::Resolve {
                key,
                revision,
                source,
            } => {
                key.validate()?;
                if revision.as_ref().is_some_and(|r| !valid_hash(r)) {
                    return Err(error("invalid requested revision"));
                }
                if let Some(package) = library.find(&key, revision.as_deref()) {
                    return package_result(&library, package, "local");
                }
                if let Some(repo) = &settings.repository {
                    let catalog = runtime.block_on(repo.catalog())?;
                    if let Some(entry) = catalog.packages.iter().rev().find(|p| {
                        p.key == key && revision.as_ref().is_none_or(|r| r == &p.revision)
                    }) {
                        let artifact = runtime.block_on(repo.download(entry))?;
                        let package = library.save(
                            key,
                            &artifact.bytes,
                            repo.origin(),
                            artifact.source.as_deref(),
                            artifact.reviewed,
                        )?;
                        return package_result(&library, package, "repository");
                    }
                }
                if revision.is_some() {
                    return Err(error(
                        "requested immutable revision was not found; no fallback or extraction performed",
                    ));
                }
                let Some(source) = source else {
                    return Ok(
                        json!({"found":false,"message":"No matching package; supply documentation to ingest."}),
                    );
                };
                self.ingest(&settings, &mut library, key, source)
            }
            Request::Ingest { key, source } => self.ingest(&settings, &mut library, key, source),
            Request::Add { key, file } => {
                key.validate()?;
                let bytes = runtime.block_on(read_document(&file))?;
                let package =
                    library.save(key, &bytes, format!("descriptor:{file}"), None, false)?;
                package_result(&library, package, "added")
            }
            Request::Inspect { key, revision } => {
                let p = required(&library, &key, &revision)?;
                package_result(&library, p, "local")
            }
            Request::Accept { key, revision } => {
                key.validate()?;
                let p = library.accept(&key, &revision)?;
                package_result(&library, p, "accepted")
            }
            Request::Compare { key, before, after } => {
                required(&library, &key, &before)?;
                required(&library, &key, &after)?;
                let a: Value = serde_json::from_slice(&library.descriptor(&before)?)
                    .map_err(std::io::Error::other)?;
                let b: Value = serde_json::from_slice(&library.descriptor(&after)?)
                    .map_err(std::io::Error::other)?;
                let mut changes = vec![];
                differences(&a, &b, "#", &mut changes);
                Ok(json!({"before":before,"after":after,"changes":changes}))
            }
            Request::Recipe {
                key,
                revision,
                environment,
                alias,
                endpoint,
                credentials,
                auth,
            } => {
                let p = required(&library, &key, &revision)?;
                if !p.accepted {
                    return Err(error(
                        "review and accept this exact draft revision before using it",
                    ));
                }
                if !identifier(&environment) || !identifier(&alias) {
                    return Err(error("environment and alias must be identifiers"));
                }
                let url = url::Url::parse(&endpoint)
                    .map_err(|_| error("execution endpoint must be an absolute HTTP(S) URL"))?;
                if !matches!(url.scheme(), "http" | "https")
                    || url.host_str().is_none()
                    || !url.username().is_empty()
                    || url.password().is_some()
                    || url.query().is_some()
                    || url.fragment().is_some()
                {
                    return Err(error(
                        "execution endpoint must be HTTP(S) without credentials, query or fragment",
                    ));
                }
                let slots =
                    io_layer::authentication_requirements(&library.descriptor(&revision)?, &auth)?
                        .into_iter()
                        .collect::<std::collections::BTreeSet<_>>();
                if slots != credentials.keys().cloned().collect() {
                    return Err(error(
                        "credential slot references must exactly match this descriptor; values are never accepted here",
                    ));
                }
                if credentials
                    .values()
                    .any(|s| s.is_empty() || s.len() > 4096 || s.chars().any(char::is_control))
                {
                    return Err(error("invalid credential reference"));
                }
                let mut secrets = serde_json::Map::new();
                let mut refs = serde_json::Map::new();
                let mut bindings = serde_json::Map::new();
                for (slot, reference) in &credentials {
                    secrets.insert(slot.clone(), json!({"required":true}));
                    refs.insert(slot.clone(), json!(reference));
                    bindings.insert(slot.clone(), json!({"secret":slot}));
                }
                let path = library.descriptor_path(&revision)?;
                let mut recipe = json!({"version":1,"package":format!("api-library-{environment}"),"targets":{"local":{"kind":"local"}},"environments":{environment.clone():{"secretSlots":secrets,"secretRefs":refs,"imports":{alias.clone():{"source":{"kind":"spec","file":path,"sha256":revision},"bind":{"target":"local","endpoint":endpoint,"credentials":bindings}}}}}});
                if !auth.is_empty() {
                    recipe["environments"][&environment]["imports"][&alias]["bind"]["auth"] =
                        json!(auth);
                }
                let text = serde_json::to_string_pretty(&recipe).map_err(std::io::Error::other)?;
                // Authoritative YAML grammar validates user choices without opening a workspace.
                wes_core::environments::Package::parse(&text).map_err(|e| error(&e.to_string()))?;
                Ok(json!({"package":p,"recipe":text}))
            }
        }
    }
    fn ingest(
        &self,
        settings: &Settings,
        library: &mut Library,
        key: PackageKey,
        source: Extraction,
    ) -> Result<Value> {
        key.validate()?;
        let executable = settings
            .extractor
            .as_ref()
            .ok_or_else(|| error("configure the wes-extract executable before ingestion"))?;
        let runtime = tokio::runtime::Handle::current();
        let body = runtime.block_on(read_document(&source.location))?;
        let bytes = runtime.block_on(io_layer::extract(executable, &key, &body, &source))?;
        let bytes = provenance::located(&bytes, &source.location)?;
        let package = library.save(
            key,
            &bytes,
            format!("ingested:{}", source.location),
            Some(&body),
            false,
        )?;
        package_result(library, package, "ingested")
    }
}
fn overlap(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}
fn required(library: &Library, key: &PackageKey, revision: &str) -> Result<Package> {
    key.validate()?;
    library
        .find(key, Some(revision))
        .ok_or_else(|| error("local package revision not found"))
}
fn package_result(library: &Library, package: Package, from: &str) -> Result<Value> {
    let bytes = library.descriptor(&package.revision)?;
    let descriptor: Value = serde_json::from_slice(&bytes).map_err(std::io::Error::other)?;
    Ok(
        json!({"found":true,"from":from,"descriptorPath":library.descriptor_path(&package.revision)?,"package":package,"descriptor":descriptor,"source":String::from_utf8(bytes).map_err(|_|error("invalid descriptor UTF-8"))?}),
    )
}
fn differences(a: &Value, b: &Value, at: &str, out: &mut Vec<Value>) {
    if a == b {
        return;
    }
    if let (Some(a), Some(b)) = (a.as_object(), b.as_object()) {
        let keys: std::collections::BTreeSet<_> = a.keys().chain(b.keys()).collect();
        for key in keys {
            let path = format!("{at}/{}", key.replace('~', "~0").replace('/', "~1"));
            match (a.get(key), b.get(key)) {
                (Some(a), Some(b)) => differences(a, b, &path, out),
                (a, b) => out.push(json!({"path":path,"before":a,"after":b})),
            }
        }
    } else {
        out.push(json!({"path":at,"before":a,"after":b}));
    }
}

pub fn parse_request(bytes: &[u8]) -> Result<Request> {
    if bytes.len() > 2 * max_descriptor() {
        return Err(error("library request exceeds 2 MiB"));
    }
    wes_adapters::codec::decode_json_preserving(
        bytes,
        wes_adapters::codec::Limits {
            bytes: 2 * max_descriptor(),
            nodes: 100_000,
        },
    )
    .map_err(|_| error("invalid library request JSON"))?;
    serde_json::from_slice(bytes).map_err(|_| error("invalid library action or fields"))
}
