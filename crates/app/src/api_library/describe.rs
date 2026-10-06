//! Document generation owns I/O; the engine owns its ordinary node lifecycle.
use super::*;
use std::{io::Write, sync::Arc};
use wes_engine::{
    describe::{DescribeRequest, DescribeService},
    driver::CancellationToken,
    providers::{InvocationError, InvocationFuture},
};

impl ApiLibrary {
    pub fn describe_service(&self, base: PathBuf) -> Arc<dyn DescribeService> {
        Arc::new(Service {
            library: self.clone(),
            base,
        })
    }
}
struct Service {
    library: ApiLibrary,
    base: PathBuf,
}
fn failed(code: &str, message: impl Into<String>) -> InvocationError {
    InvocationError::Failed(
        wes_core::ErrorValue::new(
            wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
            code,
            message,
            vec![],
            None,
        )
        .expect("safe describe error"),
    )
}
async fn joined<T: Send + 'static>(f: impl FnOnce() -> Result<T> + Send + 'static) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| error("describe worker failed"))?
}
impl DescribeService for Service {
    fn describe(
        &self,
        request: DescribeRequest,
        cancellation: CancellationToken,
    ) -> InvocationFuture {
        let library = self.library.clone();
        let base = self.base.clone();
        Box::pin(async move {
            let token = cancellation.clone();
            let lib = library.clone();
            let settings = joined(move || {
                lib.settings()?
                    .ok_or_else(|| error("API library is unavailable in this data folder"))
            })
            .await
            .map_err(|e| failed("DSC001", e.to_string()))?;
            if token.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let location = if request.is_url {
                wes_adapters::imports::validate_spec_url(&request.location)
                    .map_err(|e| failed("DSC003", e.to_string()))?;
                request.location.clone()
            } else {
                absolute(&base, &request.location)
                    .map_err(|e| failed("DSC003", e.to_string()))?
                    .to_string_lossy()
                    .into_owned()
            };
            let body = tokio::select! {
                _ = token.cancelled() => return Err(InvocationError::Cancelled),
                result = read_document(&location) => result.map_err(|e| failed("DSC003", e.to_string()))?,
            };
            let discovered = location.clone();
            let key = PackageKey {
                service: request.provider,
                api_version: "describe".into(),
                scope: digest(location.as_bytes()),
            };
            let executable = settings
                .extractor
                .clone()
                .or_else(super::command::bundled_extractor)
                .ok_or_else(|| {
                    failed(
                        "DSC001",
                        "Bundled wes-extract is unavailable; configure an extractor executable.",
                    )
                })?;
            let options = Extraction {
                location: location.clone(),
                allow_partial: false,
            };
            let publication = Arc::new(DraftPublication {
                library: library.clone(),
                directory: settings.local_directory.clone(),
                key: key.clone(),
                body: Arc::new(body.clone()),
                location: location.clone(),
                discovered: discovered.clone(),
            });
            let converted = io_layer::extract_draft_controlled(
                &executable,
                &key,
                &body,
                &options,
                token.clone(),
            )
            .await;
            if token.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let bytes = match converted {
                Ok(bytes) => bytes,
                Err(e) => {
                    let code = io_layer::extraction_code(&e);
                    let summary = io_layer::extraction_message(&e);
                    if let Some(report) = io_layer::extraction_report(&e) {
                        let id = uuid::Uuid::new_v4().to_string();
                        let details = json!({"id":id,"report":report,"location":location,"discoveredLocation":discovered,"sourceDigest":digest(&body)});
                        let lib = library.clone();
                        let report_id = id.clone();
                        if joined(move || lib.save_failure(&report_id, &details))
                            .await
                            .is_ok()
                        {
                            return Err(super::failures::reported_failure(
                                code, summary, id, report,
                            ));
                        }
                        return Err(failed(
                            code,
                            format!(
                                "{summary}. Failure details could not be saved; check the data folder's permissions and free space."
                            ),
                        ));
                    }
                    return Err(failed(code, summary));
                }
            };
            let out = request
                .out
                .map(|s| absolute(&base, &s))
                .transpose()
                .map_err(|e| failed("DSC006", e.to_string()))?;
            // Cancellation prevents final publication/export.
            let result = joined(move || {
                if cancellation.is_cancelled() {
                    return Err(error("describe cancelled before final publication"));
                }
                publication.save(&bytes, out.as_deref())
            })
            .await
            .map_err(|e| {
                if token.is_cancelled() {
                    InvocationError::Cancelled
                } else {
                    failed("DSC006", e.to_string())
                }
            })?;
            let data = wes_adapters::codec::decode_json_preserving(
                &serde_json::to_vec(&result)
                    .map_err(|_| failed("DSC006", "Cannot encode spec result"))?,
                wes_adapters::codec::Limits {
                    bytes: 2 * max_descriptor(),
                    nodes: 100_000,
                },
            )
            .map_err(|_| failed("DSC006", "Spec result exceeds its budget"))?;
            wes_core::Value::new(
                wes_core::Shape::Unknown,
                data,
                wes_core::Provenance::default()
                    .with_policy(&wes_core::flow::FlowPolicy::default().private()),
            )
            .map_err(|_| failed("DSC006", "Cannot represent spec result"))
        })
    }
}
struct DraftPublication {
    library: ApiLibrary,
    directory: PathBuf,
    key: PackageKey,
    body: Arc<Vec<u8>>,
    location: String,
    discovered: String,
}
impl DraftPublication {
    fn save(&self, bytes: &[u8], out: Option<&Path>) -> Result<Value> {
        let (text, mut evidence) = io_layer::draft::extraction(bytes)?;
        evidence["location"] = json!(io_layer::portable_source_location(&self.discovered));
        let _guard = exclusive_lock(&self.library.home, ".api-library.lock")?;
        let current = self
            .library
            .settings()?
            .ok_or_else(|| error("API library configuration changed"))?;
        if current.local_directory != self.directory {
            return Err(error("API library configuration changed; retry"));
        }
        let mut store = Library::open(&current.local_directory)?;
        let saved = store.create_draft(
            self.key.clone(),
            text,
            evidence,
            &self.body,
            format!("described:{}; source:{}", self.location, self.discovered),
        )?;
        let mut result = json!({"draft":saved["draft"],"status":if saved["validation"]["valid"] == true {"valid"} else {"draft"},"operationCount":saved["validation"]["preview"]["operations"].as_array().map_or(0, Vec::len),"diagnostics":saved["validation"]["diagnostics"],"next":"Open /spec to review or complete this draft."});
        if let Some(revision) = saved["draft"]["descriptorRevision"].as_str() {
            result["descriptorPath"] = saved["descriptorPath"].clone();
            if let Some(package) = store.find(&self.key, Some(revision)) {
                result["package"] = json!(package);
            }
            if let Some(path) = out {
                match Export::prepare(path, &store.descriptor(revision)?).and_then(Export::publish)
                {
                    Ok(path) => result["exportedPath"] = json!(path),
                    Err(e) => {
                        return Err(error(match e.kind() {
                            std::io::ErrorKind::AlreadyExists => {
                                "Draft saved; export failed because the output already exists. Choose a new filename; the existing file was not changed."
                            }
                            std::io::ErrorKind::PermissionDenied => {
                                "Draft saved; export failed because the output location is not writable. Check its permissions or choose another location."
                            }
                            _ => {
                                "Draft saved; export failed. Check the output directory and available space; review the saved draft in /spec."
                            }
                        }));
                    }
                }
            }
        } else if out.is_some() {
            return Err(error(
                "Draft saved; export was not performed. Resolve its Problems in /spec before exporting an executable spec.",
            ));
        }
        Ok(result)
    }
}

fn absolute(base: &Path, text: &str) -> Result<PathBuf> {
    // The user's home as the host defines it; a variable of one platform is not it.
    absolute_from(base, text, std::env::home_dir())
}
fn absolute_from(base: &Path, text: &str, home: Option<PathBuf>) -> Result<PathBuf> {
    let path = if let Some(rest) = text.strip_prefix("~/") {
        home.ok_or_else(|| error("Home directory is unavailable"))?
            .join(rest)
    } else {
        base.join(text)
    };
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(error(
            "Use an absolute path or a path beneath the working directory, without '..'",
        ));
    }
    Ok(path)
}
pub(super) struct Export {
    file: tempfile::NamedTempFile,
    path: PathBuf,
}
impl Export {
    pub(super) fn prepare(path: &Path, bytes: &[u8]) -> Result<Self> {
        if !path.is_absolute() {
            return Err(error("Export needs an absolute file path"));
        }
        if path.symlink_metadata().is_ok() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "Output already exists; choose a new filename",
            ));
        }
        let parent = path
            .parent()
            .ok_or_else(|| error("Invalid export path"))?
            .canonicalize()?;
        let path = parent.join(
            path.file_name()
                .ok_or_else(|| error("Invalid export filename"))?,
        );
        let mut file = tempfile::NamedTempFile::new_in(parent)?;
        file.write_all(bytes)?;
        file.as_file().sync_all()?;
        Ok(Self { file, path })
    }
    pub(super) fn publish(self) -> Result<PathBuf> {
        self.file.persist_noclobber(&self.path).map_err(|e| std::io::Error::new(e.error.kind(), "Output could not be published; the saved library revision is available. Choose a new filename and export it."))?;
        Ok(self.path)
    }
}

#[cfg(test)]
mod location_tests {
    use super::*;

    #[test]
    fn a_tilde_location_is_beneath_the_hosts_user_home_and_needs_one() {
        let home = std::env::temp_dir().join("user");
        let base = std::env::temp_dir().join("work");
        assert_eq!(
            absolute_from(&base, "~/specs/api.json", Some(home.clone())).unwrap(),
            home.join("specs/api.json")
        );
        assert_eq!(
            absolute_from(&base, "specs/api.json", None).unwrap(),
            base.join("specs/api.json")
        );
        assert!(absolute_from(&base, "~/specs/api.json", None).is_err());
        assert!(absolute_from(&base, "~/../other", Some(home)).is_err());
        // The process finds its user's home here without a Unix variable being set.
        assert!(absolute(&base, "~/specs/api.json").unwrap().is_absolute());
    }
}
