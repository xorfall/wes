//! Draft history lives beside, never inside, the executable object catalog.
use super::{
    draft::{self, Artifact, DraftSummary, Evidence},
    files::{decode_metadata, read_confined},
    *,
};
use serde_json::{Value, json};

impl Library {
    pub fn drafts(&self) -> Result<Vec<DraftSummary>> {
        if !self.dir.try_exists("draft-index.json")? {
            return Ok(vec![]);
        }
        let drafts: Vec<DraftSummary> =
            decode_metadata(&read_confined(&self.dir, "draft-index.json", max_source())?)?;
        if drafts.len() > 4000 {
            return Err(error("draft revision capacity exceeded"));
        }
        let mut seen = std::collections::BTreeSet::new();
        for d in &drafts {
            d.key.validate()?;
            if !valid_hash(&d.revision)
                || d.origin.len() > 4096
                || d.source_digest.as_ref().is_some_and(|s| !valid_hash(s))
                || d.descriptor_revision
                    .as_ref()
                    .is_some_and(|s| !valid_hash(s))
                || d.valid != d.descriptor_revision.is_some()
                || !seen.insert((serde_json::to_string(&d.key).unwrap(), d.revision.clone()))
            {
                return Err(error("invalid draft index"));
            }
        }
        Ok(drafts)
    }
    fn draft_artifact(&self, revision: &str) -> Result<Artifact> {
        if !valid_hash(revision) {
            return Err(error("invalid draft revision"));
        }
        let bytes = read_confined(&self.dir, &format!("drafts/{revision}.json"), max_source())?;
        if digest(&bytes) != revision {
            return Err(error("draft content hash mismatch"));
        }
        decode_metadata(&bytes)
    }
    pub fn inspect_draft(&self, key: &PackageKey, revision: &str) -> Result<Value> {
        key.validate()?;
        let summary = self
            .drafts()?
            .into_iter()
            .find(|d| &d.key == key && d.revision == revision)
            .ok_or_else(|| error("draft revision not found"))?;
        let artifact = self.draft_artifact(revision)?;
        let validation = draft::validate(&artifact.text);
        let mut result = json!({"draft":summary,"text":artifact.text,"validation":validation,"evidence":artifact.evidence});
        // Always revalidate retained text before exposing an executable path.
        if validation.valid
            && let Some(revision) = &summary.descriptor_revision
        {
            let bytes = self.descriptor(revision)?;
            let expected = materialize(&validation, &artifact.evidence)?
                .ok_or_else(|| error("draft cannot be materialized"))?;
            if bytes != expected {
                return Err(error(
                    "draft descriptor does not match its retained text and evidence",
                ));
            }
            result["descriptor"] = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
            result["descriptorPath"] = json!(self.descriptor_path(revision)?);
        }
        Ok(result)
    }
    pub fn create_draft(
        &mut self,
        key: PackageKey,
        text: String,
        source: Value,
        original: &[u8],
        origin: String,
    ) -> Result<Value> {
        let evidence = Evidence {
            source,
            status: "current".into(),
            manual_targets: vec![],
        };
        self.write_draft(
            key,
            Artifact {
                text,
                evidence,
                parent: None,
            },
            Some(original),
            origin,
        )
    }
    pub fn save_draft(&mut self, key: PackageKey, revision: &str, text: String) -> Result<Value> {
        let drafts = self.drafts()?;
        let previous = drafts
            .iter()
            .rev()
            .find(|d| d.key == key)
            .ok_or_else(|| error("draft not found"))?;
        if previous.revision != revision {
            return Err(error(
                "This draft changed; reload its latest revision before saving. Your text has not been overwritten.",
            ));
        }
        let mut artifact = self.draft_artifact(revision)?;
        if text == artifact.text {
            // A retained draft may become materializable after validator improvements.
            // Only an explicit save publishes a new revision; source evidence did not change.
            if previous.descriptor_revision.is_none() && draft::validate(&text).valid {
                artifact.parent = Some(revision.into());
                return self.write_draft(key, artifact, None, previous.origin.clone());
            }
            return self.inspect_draft(&key, revision);
        }
        artifact.evidence.status = "stale".into();
        if let (Ok(before), Ok(after)) = (
            serde_json::from_str(&artifact.text),
            serde_json::from_str(&text),
        ) {
            draft::changed(&before, &after, "#", &mut artifact.evidence.manual_targets);
        } else {
            artifact.evidence.manual_targets.push("#".into());
        }
        artifact.evidence.manual_targets.sort();
        artifact.evidence.manual_targets.dedup();
        artifact.parent = Some(revision.into());
        artifact.text = text;
        self.write_draft(key, artifact, None, previous.origin.clone())
    }
    fn write_draft(
        &mut self,
        key: PackageKey,
        artifact: Artifact,
        original: Option<&[u8]>,
        origin: String,
    ) -> Result<Value> {
        key.validate()?;
        if artifact.text.len() > max_descriptor()
            || origin.len() > 4096
            || original.is_some_and(|s| s.len() > max_source() || std::str::from_utf8(s).is_err())
        {
            return Err(error("draft or source exceeds its budget"));
        }
        let mut index = self.drafts()?;
        if index.len() >= 4000 {
            return Err(error("draft revision capacity reached"));
        }
        let bytes = serde_json::to_vec_pretty(&artifact).map_err(io::Error::other)?;
        if bytes.len() > max_source() {
            return Err(error("draft evidence exceeds storage budget"));
        }
        let revision = digest(&bytes);
        if index.iter().any(|d| d.key == key && d.revision == revision) {
            return self.inspect_draft(&key, &revision);
        }
        let validation = draft::validate(&artifact.text);
        let descriptor_revision = if let Some(descriptor) =
            materialize(&validation, &artifact.evidence)?
        {
            let package = self.save(key.clone(), &descriptor, origin.clone(), original, false)?;
            Some(package.revision)
        } else {
            None
        };
        let source_digest = original.map(digest).or_else(|| {
            index
                .iter()
                .rev()
                .find(|d| d.key == key)
                .and_then(|d| d.source_digest.clone())
        });
        if let (Some(body), Some(hash)) = (original, &source_digest) {
            self.object(&format!("sources/{hash}.txt"), body)?;
        }
        if self
            .dir
            .symlink_metadata("drafts")
            .is_ok_and(|m| !m.is_dir() || m.file_type().is_symlink())
        {
            return Err(error("invalid draft directory"));
        }
        self.dir.create_dir_all("drafts")?;
        self.object(&format!("drafts/{revision}.json"), &bytes)?;
        index.push(DraftSummary {
            key: key.clone(),
            revision: revision.clone(),
            accepted: false,
            origin,
            source_digest,
            valid: validation.valid,
            descriptor_revision,
        });
        atomic_bytes_in(
            &self.dir,
            "draft-index.json".as_ref(),
            &serde_json::to_vec_pretty(&index).map_err(io::Error::other)?,
            max_source(),
            "draft index exceeds budget",
        )?;
        self.inspect_draft(&key, &revision)
    }
    pub fn review_draft(&mut self, key: &PackageKey, revision: &str) -> Result<Value> {
        let result = self.inspect_draft(key, revision)?;
        if result["validation"]["valid"] != true {
            return Err(error("Only a valid saved draft can be reviewed"));
        }
        let mut index = self.drafts()?;
        if index
            .iter()
            .rev()
            .find(|d| &d.key == key)
            .is_none_or(|d| d.revision != revision)
        {
            return Err(error("Draft changed; review its latest saved revision"));
        }
        let item = index
            .iter_mut()
            .find(|d| &d.key == key && d.revision == revision)
            .unwrap();
        item.accepted = true;
        atomic_bytes_in(
            &self.dir,
            "draft-index.json".as_ref(),
            &serde_json::to_vec_pretty(&index).map_err(io::Error::other)?,
            max_source(),
            "draft index exceeds budget",
        )?;
        self.inspect_draft(key, revision)
    }
}

fn materialize(validation: &draft::Validation, evidence: &Evidence) -> Result<Option<Vec<u8>>> {
    let Some(mut descriptor) = validation.descriptor.clone() else {
        return Ok(None);
    };
    let mut source = evidence.source.clone();
    if !source.is_object() {
        source = json!({});
    }
    if evidence.status == "stale" {
        if let Some(p) = source.get_mut("provenance").and_then(Value::as_object_mut) {
            p.insert("status".into(), json!("stale"));
        }
    }
    // Draft response arrays use indices; executable responses use status keys.
    if let (Some(entries), Some(ops)) = (
        source
            .pointer_mut("/provenance/entries")
            .and_then(Value::as_array_mut),
        validation
            .preview
            .as_ref()
            .and_then(|v| v["operations"].as_array()),
    ) {
        for (i, op) in ops.iter().enumerate() {
            for (j, response) in op["responses"].as_array().unwrap().iter().enumerate() {
                let old = format!("#/operations/{i}/responses/{j}");
                let next = format!("#/operations/{i}/responses/{}", response["status"]);
                for entry in entries.iter_mut() {
                    if entry["target"]
                        .as_str()
                        .is_some_and(|t| t == old || t.starts_with(&format!("{old}/")))
                    {
                        entry["target"] = json!(next);
                    }
                }
            }
        }
    }
    source["manualTargets"] = json!(evidence.manual_targets);
    descriptor["source"] = source;

    let bytes = serde_json::to_vec_pretty(&descriptor).map_err(io::Error::other)?;
    validate_descriptor(&bytes)?;
    Ok(Some(bytes))
}
