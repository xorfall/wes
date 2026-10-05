//! Private failure evidence lives outside successful spec revisions and workspace values.
use super::*;
use wes_core::{ErrorId, ErrorValue, ValidationIssue};
use wes_engine::providers::InvocationError;

impl ApiLibrary {
    pub(super) fn save_failure(&self, id: &str, details: &Value) -> Result<()> {
        let directory = self.home.join("diagnostics/describe");
        validate_directory(&directory)?;
        atomic_json(&directory.join(format!("{id}.json")), details)
    }
    pub(super) fn read_failure(&self, id: &str) -> Result<Value> {
        let parsed = uuid::Uuid::parse_str(id).map_err(|_| error("Invalid describe report ID"))?;
        if parsed.to_string() != id {
            return Err(error("Invalid describe report ID"));
        }
        let directory = self.home.join("diagnostics/describe");
        validate_directory(&directory)?;
        let path = directory.join(format!("{id}.json"));
        if !std::fs::symlink_metadata(&path)?.file_type().is_file() {
            return Err(error("Describe report must be a regular file"));
        }
        read_json_file(&path)
    }
}

pub(super) fn reported_failure(
    code: &str,
    summary: &str,
    id: String,
    report: &io_layer::ExtractionReport,
) -> InvocationError {
    let mut issues = vec![ValidationIssue {
        path: "/describeReport".into(),
        code: "DSC_REPORT".into(),
        message: id,
    }];
    let mut counts = std::collections::BTreeMap::new();
    for issue in &report.issues {
        *counts.entry(issue.kind.as_str()).or_insert(0usize) += 1;
    }
    for (kind, count) in counts {
        let advice = match kind {
            "missing" => {
                "Required API definitions are missing; complete the operation/schema/security definitions."
            }
            "conflict" => {
                "API definitions conflict; resolve ambiguous or inconsistent declarations."
            }
            "unsupported" => {
                "Unsupported API constructs were found; simplify them or review failure details."
            }
            "blocked" => "Some definitions could not be resolved; check their schema references.",
            "validation" => {
                "Generated contract validation failed; review the document's types and operations."
            }
            "advisory" => "The extractor reported conditions requiring review.",
            _ => continue,
        };
        issues.push(ValidationIssue {
            path: "/describeReport".into(),
            code: "DSC_REASON".into(),
            message: format!("{count} {kind}: {advice}"),
        });
    }

    InvocationError::Failed(
        ErrorValue::new(
            ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
            code,
            format!("{summary}. Open failure details for the specific reasons; no spec was saved."),
            issues,
            None,
        )
        .expect("fixed report reference"),
    )
}
