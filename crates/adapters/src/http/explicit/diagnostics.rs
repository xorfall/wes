//! Request-boundary diagnostics contain contract metadata, never supplied values.
use super::*;
use crate::codec::CodecError;
use wes_core::{ValidationIssue, contracts::ContractKind};

pub(in crate::http) fn failure(path: String, code: &str, message: impl Into<String>) -> Failure {
    Failure::RequestIssues(vec![ValidationIssue {
        path,
        code: code.into(),
        message: message.into(),
    }])
}
pub(super) fn pointer(text: &str) -> String {
    text.replace('~', "~0").replace('/', "~1")
}
impl Argument {
    pub(super) fn issue(&self, code: &str, reason: impl AsRef<str>) -> Failure {
        failure(
            format!("/arguments/{}", pointer(&self.name)),
            code,
            format!(
                "{} parameter {:?}: {}",
                self.location,
                self.name,
                reason.as_ref()
            ),
        )
    }
    pub(in crate::http) fn codec_issue(&self, error: CodecError) -> Failure {
        let (code, reason) = match error {
            CodecError::Bytes => (
                "HTTP_REQUEST_BYTES",
                "encoded argument exceeds the request byte limit",
            ),
            CodecError::Work => (
                "HTTP_REQUEST_WORK",
                "argument exceeds the parsing/encoding work limit",
            ),
            CodecError::Depth => (
                "HTTP_REQUEST_DEPTH",
                "argument exceeds the supported nesting depth",
            ),
            CodecError::Contract => (
                "HTTP_REQUEST_CONTRACT",
                "JSON does not satisfy any declared contract alternative",
            ),
            CodecError::AmbiguousContract => (
                "HTTP_REQUEST_AMBIGUOUS",
                "JSON has multiple distinct native interpretations under the declared contract",
            ),
            // Codec errors may carry source fragments; do not interpolate them.
            _ => (
                "HTTP_REQUEST_ENCODING",
                "argument cannot be represented using the declared JSON contract",
            ),
        };
        self.issue(code, format!("{reason}; expected {}", self.contract.name()))
    }
    pub(super) fn contract_issues(&self, issues: Vec<ValidationIssue>) -> Failure {
        Failure::RequestIssues(
            issues
                .into_iter()
                .map(|issue| ValidationIssue {
                    path: format!(
                        "/arguments/{}{}",
                        pointer(&self.name),
                        safe_path(&self.contract, &issue.path)
                    ),
                    code: issue.code,
                    message: format!(
                        "{} parameter {:?}: {}",
                        self.location, self.name, issue.message
                    ),
                })
                .collect(),
        )
    }
    pub(super) fn encoding_issue(&self, error: Failure) -> Failure {
        match error {
            Failure::Request(reason) => self.issue("HTTP_REQUEST_ENCODING", reason),
            other => other,
        }
    }
}

/// Only declared field names and list indices are identifiers. Map keys are data.
fn safe_path(mut contract: &Contract, path: &str) -> String {
    let mut out = String::new();
    for segment in path.split('/').skip(1) {
        while let ContractKind::Option(inner) = contract.kind() {
            contract = inner;
        }
        match contract.kind() {
            ContractKind::Record(fields) => {
                let key = segment.replace("~1", "/").replace("~0", "~");
                let Some(field) = fields.get(&key) else {
                    out.push_str("/*");
                    break;
                };
                out.push('/');
                out.push_str(segment);
                contract = &field.contract;
            }
            ContractKind::List(inner) if segment.bytes().all(|b| b.is_ascii_digit()) => {
                out.push('/');
                out.push_str(segment);
                contract = inner;
            }
            // Key/value validation share paths; stop here rather than expose a dynamic key.
            _ => {
                out.push_str("/*");
                break;
            }
        }
    }
    out
}

pub(in crate::http) fn bounded(issues: Vec<ValidationIssue>) -> Vec<ValidationIssue> {
    // The contract validator caps its own output at 100; reaching that cap may hide
    // further violations even when every returned issue fits the transport budget.
    let mut truncated = issues.len() >= 100;
    let mut out = Vec::new();
    let mut bytes = 0;
    for issue in issues {
        let size = issue.path.len() + issue.code.len() + issue.message.len();
        if out.len() >= 100 || bytes + size > 24 * 1024 {
            truncated = true;
            break;
        }
        bytes += size;
        out.push(issue);
    }
    if truncated {
        out.push(ValidationIssue {
            path: "/arguments".into(),
            code: "HTTP_DIAGNOSTICS_LIMIT".into(),
            message: "Request diagnostic limit reached; additional issues may be omitted.".into(),
        });
    }
    out
}
