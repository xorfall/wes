//! Editable definitions are inert artifacts. Only materialize() may publish executable contracts.
use super::*;
use serde_json::{Value, json};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DraftSummary {
    pub key: PackageKey,
    pub revision: String,
    pub accepted: bool,
    pub origin: String,
    pub source_digest: Option<String>,
    pub valid: bool,
    pub descriptor_revision: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Evidence {
    pub source: Value,
    pub status: String,
    pub manual_targets: Vec<String>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Artifact {
    pub text: String,
    pub evidence: Evidence,
    // Parent makes restoring the same text a new, unreviewed revision (no ABA reuse).
    pub parent: Option<String>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Diagnostic {
    pub severity: &'static str,
    pub code: &'static str,
    pub target: String,
    pub message: String,
    pub fix: String,
    pub from: usize,
    pub to: usize,
    pub line: usize,
}
#[derive(Clone, Debug, Serialize)]
pub struct Validation {
    pub hash: String,
    pub valid: bool,
    pub diagnostics: Vec<Diagnostic>,
    pub preview: Option<Value>,
    #[serde(skip)]
    pub descriptor: Option<Value>,
}

pub fn validate(text: &str) -> Validation {
    let mut check = Checker {
        text,
        spans: BTreeMap::new(),
        diagnostics: vec![],
    };
    let mut result = Validation {
        hash: digest(text.as_bytes()),
        valid: false,
        diagnostics: vec![],
        preview: None,
        descriptor: None,
    };
    let parsed = if text.len() <= max_descriptor() {
        serde_json::from_str::<Value>(text)
    } else {
        Ok(Value::Null)
    };
    if text.len() > max_descriptor() {
        check.issue(
            "#",
            "DRAFT_SIZE",
            "Draft exceeds 1 MiB",
            "Reduce the document size.",
            false,
        );
    } else if let Err(e) = &parsed {
        let start = text
            .split_inclusive('\n')
            .take(e.line().saturating_sub(1))
            .map(str::len)
            .sum::<usize>()
            + e.column().saturating_sub(1);
        let mut start = start.min(text.len());
        while !text.is_char_boundary(start) {
            start = start.saturating_sub(1);
        }
        check.spans.insert("#".into(), (start, start));
        check.issue(
            "#",
            "DRAFT_JSON",
            &e.to_string(),
            "Correct the JSON syntax; saving remains available.",
            false,
        );
    } else if crate::codec::decode_json_preserving(
        text.as_bytes(),
        crate::codec::Limits {
            bytes: max_descriptor(),
            nodes: 20_000,
        },
    )
    .is_err()
    {
        check.issue(
            "#",
            "DRAFT_JSON",
            "Duplicate keys or JSON structural budget exceeded",
            "Use unique keys and bounded nesting.",
            false,
        );
    } else {
        let value = parsed.unwrap();
        let mut scanner = Scanner {
            bytes: text.as_bytes(),
            at: 0,
            spans: BTreeMap::new(),
        };
        scanner.value("#");
        check.spans = scanner.spans;
        result.preview = Some(value.clone());
        result.descriptor = check.convert(value);
    }
    result.valid =
        !check.diagnostics.iter().any(|d| d.severity == "error") && result.descriptor.is_some();
    if !result.valid {
        result.descriptor = None;
    }
    result.diagnostics = check.diagnostics;
    result
}
struct Checker<'a> {
    text: &'a str,
    spans: BTreeMap<String, (usize, usize)>,
    diagnostics: Vec<Diagnostic>,
}
impl Checker<'_> {
    fn issue(&mut self, target: &str, code: &'static str, message: &str, fix: &str, warning: bool) {
        let mut anchor = target;
        while !self.spans.contains_key(anchor) && anchor != "#" {
            anchor = anchor.rsplit_once('/').map_or("#", |v| v.0);
        }
        let (from, to) = self.spans.get(anchor).copied().unwrap_or((0, 0));
        self.diagnostics.push(Diagnostic {
            severity: if warning { "warning" } else { "error" },
            code,
            target: target.into(),
            message: message.into(),
            fix: fix.into(),
            from: self.text[..from].encode_utf16().count(),
            to: self.text[..to].encode_utf16().count(),
            line: self.text[..from].bytes().filter(|b| *b == b'\n').count() + 1,
        });
    }
    fn convert(&mut self, mut doc: Value) -> Option<Value> {
        let Some(root) = doc.as_object_mut() else {
            self.issue(
                "#",
                "DRAFT_ROOT",
                "Draft must be an object",
                "Use the draftVersion 1 document format.",
                false,
            );
            return None;
        };
        if root.remove("draftVersion") != Some(json!(1)) {
            self.issue(
                "#/draftVersion",
                "DRAFT_VERSION",
                "Expected draftVersion 1",
                "Set draftVersion to 1.",
                false,
            );
        }
        if let Some(raw) = root.get("notes") {
            let original = Value::Object(root.clone());
            match crate::descriptor::notes::validate(raw) {
                Ok(notes) => {
                    for (i, note) in notes.iter().enumerate() {
                        let target = note.target.strip_prefix('#').unwrap();
                        let resolves = target.is_empty() || original.pointer(target).is_some();
                        self.issue(&format!("#/notes/{i}"),
                        if resolves { "DRAFT_CONSTRAINT_NOTE" } else { "DRAFT_NOTE_TARGET" },
                        &note.description,
                        if resolves { "Not checked locally. Review this service constraint before calling the API." }
                        else { "Point the note at an existing draft field." }, resolves);
                    }
                }
                Err(message) => self.issue(
                    "#/notes",
                    "DRAFT_NOTES",
                    message,
                    "Use bounded constraint notes with enforcement not-checked-locally.",
                    false,
                ),
            }
        }
        if let Some(problems) = root.remove("problems") {
            if let Some(problems) = problems.as_array() {
                for (i, problem) in problems.iter().enumerate() {
                    self.issue(
                        &format!("#/problems/{i}"),
                        "DRAFT_UNRESOLVED",
                        problem
                            .get("message")
                            .and_then(Value::as_str)
                            .unwrap_or("Unresolved extraction requirement"),
                        "Resolve this requirement, then remove its entry from problems.",
                        false,
                    );
                }
            } else {
                self.issue(
                    "#/problems",
                    "DRAFT_PROBLEMS",
                    "Problems must be an array",
                    "Keep unresolved requirements as objects with target and message.",
                    false,
                );
            }
        }
        // Report inert notes independently of executable validity. Validate their shape here,
        // then restore them after contract checking so descriptor warnings are not duplicated.
        // Keep the original key position: older published descriptors are byte-pinned.
        let mut advisories = root
            .get_mut("diagnostics")
            .map(|notes| std::mem::replace(notes, json!([])));
        let mut uncertainty_notes: Vec<Value> = vec![];
        if let Some(notes) = &advisories {
            match serde_json::from_value::<Vec<String>>(notes.clone()) {
                Ok(notes) => {
                    for (i, note) in notes.iter().enumerate() {
                        self.issue(
                            &format!("#/diagnostics/{i}"),
                            "DRAFT_ADVISORY",
                            note,
                            "Behavior note; does not block import. Review before calling the API.",
                            true,
                        );
                    }
                }
                Err(_) => self.issue(
                    "#/diagnostics",
                    "DRAFT_DIAGNOSTICS",
                    "Diagnostics must be an array of advisory strings",
                    "Keep behavior notes as text; unresolved requirements belong in problems.",
                    false,
                ),
            }
        }
        // Provenance is server-owned, never accepted from editable source.
        if root.contains_key("source") {
            self.issue(
                "#/source",
                "DRAFT_EVIDENCE",
                "Source evidence is stored separately",
                "Remove source from the editable draft.",
                false,
            );
        }
        if root.contains_key("version") {
            self.issue(
                "#/version",
                "DRAFT_VERSION",
                "Executable version does not belong in a draft",
                "Use draftVersion only.",
                false,
            );
        }
        root.insert("version".into(), json!(crate::descriptor::VERSION));
        let Some(ops) = root.get_mut("operations").and_then(Value::as_array_mut) else {
            self.issue(
                "#/operations",
                "DRAFT_OPERATIONS",
                "Operations must be an array",
                "Add the known operations.",
                false,
            );
            return None;
        };
        if ops.is_empty() || ops.len() > 1000 {
            self.issue(
                "#/operations",
                "DRAFT_OPERATIONS",
                "Draft needs 1 to 1000 operations",
                "Keep the document within the operation budget.",
                false,
            );
            return None;
        }
        for (i, op) in ops.iter_mut().enumerate() {
            let at = format!("#/operations/{i}");
            let Some(op) = op.as_object_mut() else {
                self.issue(
                    &at,
                    "DRAFT_OPERATION",
                    "Operation must be an object",
                    "Supply a native operation definition.",
                    false,
                );
                continue;
            };
            const FIELDS: &[&str] = &[
                "path",
                "summary",
                "description",
                "responseDescriptions",
                "method",
                "safety",
                "route",
                "stream",
                "auth",
                "authOptions",
                "parameters",
                "responses",
                "evidence",
            ];
            for field in op.keys().filter(|field| !FIELDS.contains(&field.as_str())) {
                let name = field
                    .chars()
                    .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                    .take(64)
                    .collect::<String>();
                self.issue(
                    &format!("{at}/{}", token(field)),
                    "DRAFT_FIELD",
                    &format!("Unknown operation field: {name}"),
                    "Use supported contract fields; safety accepts safe or unsafe.",
                    false,
                );
            }
            if let Some(safety) = op.get("safety") {
                let location = format!("{at}/safety");
                if !matches!(safety.as_str(), Some("safe" | "unsafe")) {
                    self.issue(&location, "DRAFT_SAFETY", "Safety must be safe or unsafe", "SAFE permits automatic repetition; only declare it for operations without external effects.", false);
                } else {
                    self.issue(&location, "DRAFT_SAFETY", "Explicit safety classification controls automatic repetition", "Review the operation's effects before accepting this draft; HTTP idempotence alone does not establish safety.", true);
                }
            }
            if op.get("auth").is_none_or(Value::is_null) {
                op.insert("auth".into(), json!([]));
            }
            if !op.contains_key("authOptions")
                && op
                    .get("auth")
                    .and_then(Value::as_array)
                    .is_some_and(Vec::is_empty)
            {
                self.issue(&format!("{at}/auth"), "DRAFT_AUTH", "No credentials attached; access policy is not established by an empty auth list", "Review authentication separately before calling the API.", true);
            }
            for field in ["path", "method", "route", "parameters"] {
                if op.get(field).is_none_or(Value::is_null) {
                    self.issue(
                        &format!("{at}/{field}"),
                        "DRAFT_REQUIRED",
                        &format!("Missing {field}"),
                        "Supply this field from documentation or your own confirmed contract.",
                        false,
                    );
                }
            }
            if let Some(params) = op.get_mut("parameters").and_then(Value::as_array_mut) {
                for (j, param) in params.iter_mut().enumerate() {
                    let pat = format!("{at}/parameters/{j}/required");
                    match param.get("required") {
                        Some(Value::Bool(_)) => (),
                        None | Some(Value::Null) => {
                            let path =
                                param.get("location").and_then(Value::as_str) == Some("path");
                            let message = if path {
                                "Requiredness is undocumented; this path value is required to build the URL"
                            } else {
                                "Requiredness is unknown; omission is allowed locally but the server may reject it"
                            };
                            if let Some(param) = param.as_object_mut() {
                                param.insert("required".into(), json!(path));
                            }
                            self.issue(&pat, "DRAFT_REQUIREDNESS_UNKNOWN", message, "Confirm requiredness when known; the draft retains the uncertainty.", true);
                            uncertainty_notes.push(json!(format!("{pat}: {message}")));
                        }
                        _ => self.issue(
                            &pat,
                            "DRAFT_REQUIREDNESS",
                            "Requiredness must be true, false or null",
                            "Use null for unknown requiredness.",
                            false,
                        ),
                    }
                }
            }
            let mut responses = serde_json::Map::new();
            match op.remove("responses") {
                Some(Value::Array(mut items)) if !items.is_empty() => for (j, response) in items.iter_mut().enumerate() {
                    let rat = format!("{at}/responses/{j}");
                    if response.as_object().is_none_or(|r| r.keys().any(|k| !matches!(k.as_str(), "status" | "mediaType" | "type"))) {
                        self.issue(&rat, "DRAFT_RESPONSE", "Response accepts status, mediaType and type only", "Correct the response object.", false);
                    }
                    let status = response.get("status").and_then(Value::as_u64).filter(|v| (100..600).contains(v));
                    if status.is_none() { self.issue(&format!("{rat}/status"), "DRAFT_STATUS", "An exact HTTP status is required", "Supply the actual 100–599 status; it is not inferred from an example body.", false); }
                    let missing_type = response.get("type").is_none();
                    if missing_type {
                        if let Some(response) = response.as_object_mut() {
                            response.insert("type".into(), json!("Unknown"));
                        }
                    }
                    if response.get("type").and_then(Value::as_str) == Some("Unknown") {
                        let message = "JSON response shape is unknown; no field or schema guarantees are available";
                        self.issue(&format!("{rat}/type"), "DRAFT_UNKNOWN_RESPONSE", message, "Replace Unknown with a confirmed schema when available; null means an explicitly empty body.", true);
                        if missing_type { uncertainty_notes.push(json!(format!("{rat}/type: {message}"))); }
                    }
                    let kind = response.get("type");
                    if kind.is_none_or(|v| !v.is_string() && !v.is_null()) { self.issue(&format!("{rat}/type"), "DRAFT_TYPE", "Response type must be a type name or null", "Use Unknown for an unknown JSON body shape, or null for an explicitly empty body.", false); }
                    if kind.is_some_and(Value::is_string) {
                        let media = response.get("mediaType");
                        if media.is_none_or(Value::is_null) {
                            let message = "Response media type is unknown; actual headers determine body interpretation";
                            self.issue(&format!("{rat}/mediaType"), "DRAFT_MEDIA_UNKNOWN", message, "Confirm the response media type when available; no header is inferred.", true);
                            uncertainty_notes.push(json!(format!("{rat}/mediaType: {message}")));
                        } else if !media.and_then(Value::as_str).is_some_and(valid_response_media) {
                            self.issue(&format!("{rat}/mediaType"), "DRAFT_MEDIA", "A valid response media type is required", "Supply the documented type/subtype. JSON, text and bytes are interpreted from the received headers.", false);
                        }
                    } else if kind.is_some_and(Value::is_null) && response.get("mediaType").is_some_and(|v| !v.is_null()) {
                        self.issue(&format!("{rat}/mediaType"), "DRAFT_MEDIA", "An empty response must not declare a body media type", "Use null mediaType for an explicitly empty body.", false);
                    }
                    if let (Some(status), Some(kind)) = (status, kind) {
                        if responses.insert(status.to_string(), kind.clone()).is_some() { self.issue(&format!("{rat}/status"), "DRAFT_STATUS", "Duplicate response status", "Define each status once.", false); }
                    }
                },
                _ => self.issue(&format!("{at}/responses"), "DRAFT_RESPONSES", "At least one documented response is required", "Add a response with status, mediaType and type; unknown facts may remain null while drafting.", false),
            }
            op.insert("responses".into(), Value::Object(responses));
        }
        if !self.diagnostics.iter().any(|d| d.severity == "error") {
            match validate_descriptor(&serde_json::to_vec(&doc).ok()?) {
                Ok(warnings) => for warning in warnings { self.issue("#", "DRAFT_WARNING", &warning, "Review before importing.", true); },
                Err(e) => self.issue("#", "DRAFT_CONTRACT", &e.to_string(), "Correct the native types or HTTP mapping; executable validation cannot be bypassed.", false),
            }
        }
        if !uncertainty_notes.is_empty() {
            match advisories.as_mut() {
                Some(Value::Array(notes)) => notes.extend(uncertainty_notes),
                None => advisories = Some(json!(uncertainty_notes)),
                _ => (), // Malformed diagnostics already block materialization.
            }
        }
        if let Some(notes) = advisories {
            doc["diagnostics"] = notes;
        }
        Some(doc)
    }
}

/// Offsets from an already bounded, duplicate-free, valid JSON document. No semantic rules here.
struct Scanner<'a> {
    bytes: &'a [u8],
    at: usize,
    spans: BTreeMap<String, (usize, usize)>,
}
impl Scanner<'_> {
    fn space(&mut self) {
        while self.bytes.get(self.at).is_some_and(u8::is_ascii_whitespace) {
            self.at += 1;
        }
    }
    fn string(&mut self) {
        self.at += 1;
        while let Some(b) = self.bytes.get(self.at) {
            self.at += 1;
            match b {
                b'\\' => self.at += 1,
                b'"' => break,
                _ => (),
            }
        }
    }
    fn value(&mut self, path: &str) {
        self.space();
        let start = self.at;
        match self.bytes.get(self.at) {
            Some(b'{') => {
                self.at += 1;
                self.space();
                while self.bytes.get(self.at) != Some(&b'}') {
                    let key_start = self.at;
                    self.string();
                    let key: String =
                        serde_json::from_slice(&self.bytes[key_start..self.at]).unwrap();
                    self.space();
                    self.at += 1;
                    self.value(&format!("{path}/{}", token(&key)));
                    self.space();
                    if self.bytes.get(self.at) != Some(&b',') {
                        break;
                    }
                    self.at += 1;
                    self.space();
                }
                self.at += 1;
            }
            Some(b'[') => {
                self.at += 1;
                self.space();
                let mut index = 0;
                while self.bytes.get(self.at) != Some(&b']') {
                    self.value(&format!("{path}/{index}"));
                    index += 1;
                    self.space();
                    if self.bytes.get(self.at) != Some(&b',') {
                        break;
                    }
                    self.at += 1;
                }
                self.at += 1;
            }
            Some(b'"') => self.string(),
            _ => {
                while self
                    .bytes
                    .get(self.at)
                    .is_some_and(|b| !b.is_ascii_whitespace() && !b",]}".contains(b))
                {
                    self.at += 1;
                }
            }
        }
        self.spans.insert(path.into(), (start, self.at));
    }
}
pub fn token(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

/// An extraction envelope is data, never an executable descriptor; still reject unusable envelopes.
pub fn extraction(bytes: &[u8]) -> Result<(String, Value)> {
    crate::codec::decode_json_preserving(
        bytes,
        crate::codec::Limits {
            bytes: max_descriptor(),
            nodes: 20_000,
        },
    )
    .map_err(|_| error("invalid or excessive draft envelope"))?;
    let root: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let draft = &root["draft"];
    if root
        .as_object()
        .is_none_or(|r| r.len() != 2 || !r.contains_key("source"))
        || draft["draftVersion"] != 1
        || draft["operations"]
            .as_array()
            .is_none_or(|v| v.is_empty() || v.len() > 1000)
        || !draft["types"].is_object()
        || !draft["provider"].is_string()
        || !root["source"].is_object()
    {
        return Err(error(
            "extractor did not produce a usable API draft envelope",
        ));
    }
    Ok((
        serde_json::to_string_pretty(draft).map_err(io::Error::other)?,
        root["source"].clone(),
    ))
}

pub fn from_descriptor(bytes: &[u8]) -> Result<(String, Value)> {
    validate_descriptor(bytes)?;
    let mut doc: Value = serde_json::from_slice(bytes).map_err(io::Error::other)?;
    let root = doc.as_object_mut().unwrap();
    let source = root.remove("source").unwrap_or(json!({}));
    root.remove("version");
    root.insert("draftVersion".into(), json!(1));
    root.insert("problems".into(), json!([]));
    let mut source = source;
    for (i, op) in root["operations"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .enumerate()
    {
        if let Some(entries) = source
            .pointer_mut("/provenance/entries")
            .and_then(Value::as_array_mut)
        {
            for (j, status) in op["responses"].as_object().unwrap().keys().enumerate() {
                let old = format!("#/operations/{i}/responses/{}", token(status));
                for entry in entries.iter_mut() {
                    if entry["target"].as_str() == Some(&old) {
                        entry["target"] = json!(format!("#/operations/{i}/responses/{j}/status"));
                    }
                }
            }
        }
        op["responses"] = Value::Array(op["responses"].as_object().unwrap().iter().map(|(status, kind)| json!({"status":status.parse::<u16>().unwrap(),"mediaType":Value::Null,"type":kind})).collect());
    }
    Ok((
        serde_json::to_string_pretty(&doc).map_err(io::Error::other)?,
        source,
    ))
}

pub(super) fn changed(before: &Value, after: &Value, at: &str, out: &mut Vec<String>) {
    if before == after {
        return;
    }
    match (before, after) {
        (Value::Object(a), Value::Object(b)) => {
            for key in a
                .keys()
                .chain(b.keys())
                .collect::<std::collections::BTreeSet<_>>()
            {
                if a.contains_key(key) != b.contains_key(key) {
                    out.push(format!("{at}/{}", token(key)));
                } else {
                    changed(&a[key], &b[key], &format!("{at}/{}", token(key)), out);
                }
            }
        }
        (Value::Array(a), Value::Array(b)) => {
            for i in 0..a.len().max(b.len()) {
                changed(
                    a.get(i).unwrap_or(&Value::Null),
                    b.get(i).unwrap_or(&Value::Null),
                    &format!("{at}/{i}"),
                    out,
                );
            }
        }
        _ => out.push(at.into()),
    }
}

fn valid_response_media(media: &str) -> bool {
    let essence = media.split(';').next().unwrap_or("").trim();
    let Some((kind, subtype)) = essence.split_once('/') else {
        return false;
    };
    let token = |s: &str| {
        !s.is_empty()
            && s.bytes()
                .all(|c| c.is_ascii_alphanumeric() || b"!#$%&'+-.^_`|~".contains(&c))
    };
    essence != "text/event-stream"
        && token(kind)
        && token(subtype)
        && reqwest::header::HeaderValue::from_str(media).is_ok()
}

#[cfg(test)]
mod safety_tests {
    use super::*;
    #[test]
    fn safety_is_a_reviewed_draft_field_and_materializes_without_changing_method() {
        let descriptor = json!({"version":1,"provider":"inventory","types":{},"operations":[{
            "path":["search"],"method":"POST","route":"/search","auth":[],"parameters":[],"responses":{"200":null},"safety":"safe"
        }]});
        let (text, _) = from_descriptor(&serde_json::to_vec(&descriptor).unwrap()).unwrap();
        let validated = validate(&text);
        assert!(validated.valid, "{:?}", validated.diagnostics);
        assert!(validated.diagnostics.iter().any(
            |diagnostic| diagnostic.code == "DRAFT_SAFETY" && diagnostic.severity == "warning"
        ));
        let materialized = validated.descriptor.unwrap();
        assert_eq!(materialized["operations"][0]["safety"], "safe");
        assert_eq!(materialized["operations"][0]["method"], "POST");
        let mut invalid: Value = serde_json::from_str(&text).unwrap();
        invalid["operations"][0]["safe"] = json!(true);
        let validated = validate(&invalid.to_string());
        assert!(!validated.valid);
        assert!(
            validated
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "DRAFT_FIELD"
                    && diagnostic.message.contains("safe"))
        );
        invalid["operations"][0]
            .as_object_mut()
            .unwrap()
            .remove("safe");
        invalid["operations"][0]["safety"] = json!("unknown");
        assert!(!validate(&invalid.to_string()).valid);
    }
}
