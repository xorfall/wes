//! Saved draft MCP projection. Authority, validation and CAS belong to ApiLibrary.
use crate::{
    ApplicationHandle,
    api_library::Request,
    terminal::{BridgeReply, TerminalSession},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::sync::Arc;
use wes_adapters::api_library::PackageKey;

const SOURCE_LIMIT: usize = 64 * 1024;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Read {
    key: PackageKey,
    revision: String,
    #[serde(default)]
    evidence: bool,
    #[serde(default)]
    diagnostics_offset: usize,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Save {
    key: PackageKey,
    revision: String,
    text: String,
}
fn fail(message: &str) -> BridgeReply {
    BridgeReply::error(2, message)
}
fn identity(key: &PackageKey, revision: &str) -> Result<(), BridgeReply> {
    key.validate().map_err(|e| fail(&e.to_string()))?;
    if !wes_adapters::api_library::valid_hash(revision) {
        return Err(fail(
            "revision must be the exact saved SHA-256 revision from spec_list or spec_read.",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn projections_bound_source_and_page_diagnostics_without_duplicate_bodies() {
        let result = json!({"text":"é","descriptor":{"duplicate":true},"evidence":{"source":{"note":"source"},"status":"stale"},"validation":{"valid":false,"hash":"hash","preview":{"duplicate":true},"diagnostics":(0..205).collect::<Vec<_>>()}});
        let page =
            project(result.clone(), true, false, 100).unwrap_or_else(|e| panic!("{}", e.stderr));
        assert_eq!(page["validation"]["diagnosticsTotal"], 205);
        assert_eq!(page["validation"]["diagnosticsOffset"], 100);
        assert_eq!(
            page["validation"]["diagnostics"].as_array().unwrap().len(),
            100
        );
        assert_eq!(page["validation"]["diagnostics"][0], 100);
        assert!(page.get("descriptor").is_none());
        assert!(page["evidence"].get("source").is_none());
        assert!(page["validation"].get("preview").is_none());
        assert_eq!(
            project(result.clone(), true, true, usize::MAX)
                .unwrap_or_else(|e| panic!("{}", e.stderr))["validation"]["diagnostics"],
            json!([])
        );
        let mut large = result;
        large["text"] = json!("é".repeat(SOURCE_LIMIT / 2));
        assert!(project(large.clone(), true, false, 0).is_ok());
        large["text"] = json!("é".repeat(SOURCE_LIMIT / 2 + 1));
        assert!(project(large, true, false, 0).is_err());
    }
}
async fn perform(
    terminal: &Arc<TerminalSession>,
    application: &ApplicationHandle,
    request: Request,
) -> Result<Value, BridgeReply> {
    let library = terminal
        .api_library
        .clone()
        .ok_or_else(|| fail("API library service unavailable."))?;
    let permit = terminal
        .library_capacity
        .clone()
        .try_acquire_owned()
        .map_err(|_| {
            fail("Another API draft operation is in progress; inspect before retrying.")
        })?;
    let terminal = terminal.clone();
    let application = application.clone();
    terminal
        .library_tasks
        .clone()
        .spawn_blocking(move || {
            let _permit = permit;
            terminal
                .check(&application)
                .map_err(|e| fail(&e.to_string()))?;
            library
                .perform_agent(request)
                .map_err(|e| fail(&e.to_string()))
        })
        .await
        .map_err(|_| {
            fail("API draft worker failed; inspect the saved revisions before retrying.")
        })?
}
pub(super) async fn list(
    terminal: &Arc<TerminalSession>,
    app: &ApplicationHandle,
    offset: usize,
    limit: usize,
) -> Result<Value, BridgeReply> {
    if limit == 0 || limit > 100 {
        return Err(fail("limit must be between 1 and 100."));
    }
    let result = perform(terminal, app, Request::ListDrafts).await?;
    let drafts = result["drafts"].as_array().expect("library drafts");
    Ok(
        json!({"drafts":drafts.iter().skip(offset).take(limit).collect::<Vec<_>>(),"offset":offset,"total":drafts.len(),"scope":"shared saved API drafts; unsaved editor text is not included"}),
    )
}
fn project(
    mut result: Value,
    source: bool,
    evidence: bool,
    offset: usize,
) -> Result<Value, BridgeReply> {
    let text = result["text"].as_str().unwrap_or_default();
    if source && text.len() > SOURCE_LIMIT {
        return Err(fail(
            "Draft exceeds the 64 KiB MCP source limit; edit it in /spec.",
        ));
    }
    let object = result.as_object_mut().expect("draft result");
    object.remove("descriptor");
    if !source {
        object.remove("text");
    }
    if !evidence {
        result["evidence"]
            .as_object_mut()
            .map(|e| e.remove("source"));
    }
    let validation = result["validation"]
        .as_object_mut()
        .expect("draft validation");
    validation.remove("preview");
    let diagnostics = validation.remove("diagnostics").unwrap_or(json!([]));
    let diagnostics = diagnostics.as_array().expect("draft diagnostics");
    validation.insert(
        "diagnostics".into(),
        json!(
            diagnostics
                .iter()
                .skip(offset)
                .take(100)
                .collect::<Vec<_>>()
        ),
    );
    validation.insert("diagnosticsOffset".into(), json!(offset));
    validation.insert("diagnosticsTotal".into(), json!(diagnostics.len()));
    Ok(result)
}
pub(super) async fn read(
    terminal: &Arc<TerminalSession>,
    app: &ApplicationHandle,
    input: Read,
) -> Result<Value, BridgeReply> {
    identity(&input.key, &input.revision)?;
    project(
        perform(
            terminal,
            app,
            Request::InspectDraft {
                key: input.key,
                revision: input.revision,
            },
        )
        .await?,
        true,
        input.evidence,
        input.diagnostics_offset,
    )
}
pub(super) async fn save(
    terminal: &Arc<TerminalSession>,
    app: &ApplicationHandle,
    input: Save,
) -> Result<Value, BridgeReply> {
    identity(&input.key, &input.revision)?;
    if input.text.len() > SOURCE_LIMIT {
        return Err(fail(
            "Draft exceeds the 64 KiB MCP source limit; edit it in /spec.",
        ));
    }
    project(
        perform(
            terminal,
            app,
            Request::SaveDraft {
                key: input.key,
                revision: input.revision,
                text: input.text,
            },
        )
        .await?,
        false,
        false,
        0,
    )
}
