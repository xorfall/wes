use super::*;
use std::sync::LazyLock;

pub(super) const REGISTRY: &str = "iana-2025-09-15";
struct Status {
    code: i64,
    name: String,
    reference: String,
}
static STATUSES: LazyLock<Vec<Status>> = LazyLock::new(|| {
    csv::Reader::from_reader(include_bytes!("status-codes.csv").as_slice())
        .records()
        .map(|r| r.expect("checked-in IANA snapshot"))
        .filter_map(|r| {
            if &r[1] == "Unassigned" {
                return None;
            }
            Some(Status {
                code: r[0].parse().ok()?,
                name: r[1].to_owned(),
                reference: r[2].to_owned(),
            })
        })
        .collect()
});
pub(super) fn status(code: i64) -> Result<Value, LocalError> {
    if !(100..600).contains(&code) {
        return Err(invalid("HTTP status must be an integer from 100 to 599"));
    }
    let entry = STATUSES.iter().find(|s| s.code == code);
    let class = match code / 100 {
        1 => "informational",
        2 => "success",
        3 => "redirection",
        4 => "client-error",
        _ => "server-error",
    };
    let registration = match entry {
        None => "unassigned",
        Some(s) if s.name.contains("TEMPORARY") => "temporary",
        Some(s) if s.name.contains("Unused") => "unused",
        Some(s) if s.name.contains("OBSOLETED") => "obsolete",
        Some(_) => "registered",
    };
    Ok(record(
        "HttpStatusDefinition",
        [
            ("code".into(), integer(code)),
            (
                "name".into(),
                text(entry.map_or("Unassigned", |s| s.name.as_str())),
            ),
            ("class".into(), text(class)),
            ("registration".into(), text(registration)),
            (
                "reference".into(),
                text(entry.map_or("", |s| s.reference.as_str())),
            ),
            ("catalogueVersion".into(), text(REGISTRY)),
        ],
        Provenance::default(),
    ))
}
pub(super) fn status_input(data: &Data) -> Result<Value, LocalError> {
    let code = match data {
        Data::Int(n) => *n,
        Data::Text(s) if s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()) => {
            s.parse().expect("three digits")
        }
        _ => {
            return Err(invalid(
                "HTTP status lookup needs an Int or three written digits",
            ));
        }
    };
    status(code)
}
struct ErrorDefinition {
    code: &'static str,
    category: &'static str,
    summary: &'static str,
}
const ERRORS: &[ErrorDefinition] = &[
    ErrorDefinition {
        code: "HTTP001",
        category: "request",
        summary: "The request could not be prepared; check method, URL, arguments and encoding.",
    },
    ErrorDefinition {
        code: "HTTP002",
        category: "credential",
        summary: "Credential access was not granted, a required value is missing, material is invalid, or its store is unavailable. Supply values separately from provider access grants.",
    },
    ErrorDefinition {
        code: "HTTP003",
        category: "transport",
        summary: "Transport failed. The remote outcome is not established by this code.",
    },
    ErrorDefinition {
        code: "HTTP004",
        category: "timeout",
        summary: "The local HTTP time budget expired. Remote completion remains uncertain.",
    },
    ErrorDefinition {
        code: "HTTP005",
        category: "redirect",
        summary: "A redirect was rejected or the redirect limit was exceeded.",
    },
    ErrorDefinition {
        code: "HTTP006",
        category: "size",
        summary: "The response exceeded its byte budget.",
    },
    ErrorDefinition {
        code: "HTTP007",
        category: "response",
        summary: "Response interpretation failed: content type, decoding or the declared result contract may be responsible.",
    },
    ErrorDefinition {
        code: "HTTP008",
        category: "status",
        summary: "An imported operation rejected a received HTTP status. Inspect the status evidence or HTTP_STATUS issue.",
    },
    ErrorDefinition {
        code: "HTTP009",
        category: "internal",
        summary: "The local HTTP boundary worker failed.",
    },
    ErrorDefinition {
        code: "HTTP_TRANSIENT",
        category: "transient-hint",
        summary: "A later request may succeed; no retry was performed and this hint does not authorize repeating an operation.",
    },
    ErrorDefinition {
        code: "HTTPD001",
        category: "domain-input",
        summary: "HTTP definition lookup or analysis received unsupported or inconsistent input.",
    },
    ErrorDefinition {
        code: "HTTPD002",
        category: "domain-limit",
        summary: "HTTP domain input exceeded its bounded size or work budget.",
    },
];
pub(super) fn error(code: &str) -> Result<Value, LocalError> {
    if code.is_empty()
        || code.len() > 64
        || !code.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
    {
        return Err(invalid(
            "Error lookup needs a nonempty code of at most 64 ASCII letters, digits or underscores",
        ));
    }
    let found = ERRORS.iter().find(|e| e.code == code);
    let status_detail = code
        .strip_prefix("HTTP_STATUS_")
        .filter(|s| s.len() == 3 && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<i64>().ok())
        .filter(|n| (100..600).contains(n));
    let detail = status_detail.map(|n| format!("An HTTP status issue records received status {n}; use http-status for its protocol definition."));
    Ok(record(
        "HttpErrorDefinition",
        [
            ("code".into(), text(code)),
            (
                "known".into(),
                boolean(found.is_some() || status_detail.is_some()),
            ),
            (
                "category".into(),
                text(found.map_or(
                    if status_detail.is_some() {
                        "status-detail"
                    } else {
                        "unknown"
                    },
                    |e| e.category,
                )),
            ),
            (
                "summary".into(),
                text(detail.as_deref().unwrap_or_else(|| {
                    found.map_or("No definition in this HTTP catalogue.", |e| e.summary)
                })),
            ),
            ("catalogueVersion".into(), text("wes-http-errors-1")),
        ],
        Provenance::default(),
    ))
}
pub(super) fn error_input(data: &Data) -> Result<Value, LocalError> {
    let data = if let Data::Record(fields) = data {
        fields
            .get("code")
            .ok_or_else(|| invalid("Error value has no code"))?
    } else {
        data
    };
    let Data::Text(code) = data else {
        return Err(invalid(
            "Error lookup needs a written code or structured error value",
        ));
    };
    error(code)
}
pub(super) fn catalogue(data: &Data) -> Result<Value, LocalError> {
    match data {
        Data::Text(s) if s.as_ref() == "statuses" => Ok(list(
            status(200)?.shape().clone(),
            STATUSES
                .iter()
                .map(|s| status(s.code).expect("registry code")),
        )),
        Data::Text(s) if s.as_ref() == "errors" => Ok(list(
            error("HTTP001")?.shape().clone(),
            ERRORS.iter().map(|e| error(e.code).expect("defined code")),
        )),
        _ => Err(invalid("HTTP catalogue expects statuses or errors")),
    }
}
