//! The CLI view of the nominal help value. The stored value and JSON export stay structured.
use serde_json::Value as Json;
use wes_adapters::codec::{Limits, encode_json};
use wes_core::{Shape, Value};

const MAX_ROWS: usize = 256;
const MAX_CHARS: usize = 64 * 1024;

pub(super) fn render(value: &Value) -> Result<Option<String>, super::Error> {
    if !matches!(value.shape(), Shape::Record(record) if record.name() == "wes.Help") {
        return Ok(None);
    }
    let data: Json = serde_json::from_slice(&encode_json(value.data(), Limits::default())?)?;
    let mut document = Document::default();
    let provider = text(&data["provider"]);
    let path = text(&data["path"]);
    let full_path = [provider, path]
        .into_iter()
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ");
    document.add("", &format!(":help {full_path}").trim_end());
    let invocation = &data["invocation"];
    document.add(
        "",
        invocation["summary"]
            .as_str()
            .unwrap_or(text(&data["summary"])),
    );
    if invocation.is_object() {
        document.add("Usage", invocation["usage"].as_str().unwrap_or(&full_path));
        document.add("Short form", text(&invocation["shortForm"]));
        if let Some(parameters) = invocation["parameters"].as_array() {
            for parameter in parameters {
                let name = text(&parameter["name"]);
                if !name.is_empty() {
                    document.add(
                        &format!("  {name}:"),
                        &format!(
                            "{} ({})",
                            type_name(&parameter["type"], 0),
                            if parameter["required"] == true {
                                "required"
                            } else {
                                "optional"
                            }
                        ),
                    );
                    document.detail("    Allowed values", &parameter["choices"], 0);
                    document.detail("    Constraints", &parameter["constraints"], 0);
                }
            }
        }
        for (key, value) in invocation.as_object().unwrap() {
            match key.as_str() {
                "command" | "provider" | "capability" | "summary" | "usage" | "shortForm"
                | "parameters" | "operands" | "takes" => (),
                "result" => document.add("Result", &type_name(value, 0)),
                "implemented" => {
                    if value == false {
                        document.add("Status", "Reserved; not implemented.");
                    }
                }
                "otherArguments" => {
                    if value == true {
                        document.add(
                            "Arguments",
                            "Additional arguments depend on the selected operation.",
                        );
                    }
                }
                "producesValue" => {
                    if value == true {
                        document.add(
                            "Result",
                            "Produces a value that can be named with > result.",
                        );
                    }
                }
                _ => document.detail(&label(key), value, 0),
            }
        }
    }
    if let Some(fields) = data.as_object() {
        for (key, value) in fields {
            match key.as_str() {
                "provider" | "path" | "summary" | "invocation" | "children" => (),
                _ => document.detail(&label(key), value, 0),
            }
        }
    }
    if let Some(children) = data["children"].as_array() {
        for child in children {
            let name = text(&child["name"]);
            if !name.is_empty() {
                let command = [full_path.as_str(), name]
                    .into_iter()
                    .filter(|s| !s.is_empty())
                    .collect::<Vec<_>>()
                    .join(" ");
                document.add(
                    &format!("  {}{command}", if provider.is_empty() { ":" } else { "" }),
                    child["summary"].as_str().unwrap_or("Command group."),
                );
            }
        }
        if !children.is_empty() {
            document.add(
                "More help",
                &format!(
                    ":help {}<subcommand>",
                    if full_path.is_empty() {
                        String::new()
                    } else {
                        format!("{full_path} ")
                    }
                ),
            );
        }
    }
    if document.truncated {
        document
            .output
            .push_str("Help display limit reached; choose a specific command or use --json.\n");
    }
    Ok(Some(document.output))
}
fn bounded(text: &str) -> String {
    let mut chars = text.chars();
    let mut bounded: String = chars.by_ref().take(4096).collect();
    if chars.next().is_some() {
        bounded.push('…');
    }
    bounded
}
fn text(value: &Json) -> &str {
    value.as_str().unwrap_or("")
}
fn label(key: &str) -> String {
    let mut result = String::new();
    for (at, ch) in key.chars().enumerate() {
        if at == 0 {
            result.extend(ch.to_uppercase());
        } else {
            if ch.is_uppercase() {
                result.push(' ');
            }
            result.push(ch);
        }
    }
    result
}
fn type_name(value: &Json, depth: usize) -> String {
    if depth >= 8 {
        return "…".into();
    }
    if let Some(text) = value.as_str() {
        return text.into();
    }
    match text(&value["kind"]) {
        "list" => format!("List<{}>", type_name(&value["element"], depth + 1)),
        "record" => text(&value["name"]).into(),
        "primitive" => match text(&value["name"]) {
            "INT" => "Int",
            "DECIMAL" => "Decimal",
            "TEXT" => "Text",
            "BOOL" => "Bool",
            "INSTANT" => "Instant",
            "DURATION" => "Duration",
            "INTERVAL" => "Interval",
            "BYTES" => "Bytes",
            other => other,
        }
        .into(),
        _ => "Unknown".into(),
    }
}
#[derive(Default)]
struct Document {
    output: String,
    rows: usize,
    work: usize,
    truncated: bool,
}
impl Document {
    fn step(&mut self) -> bool {
        self.work += 1;
        if self.work > MAX_ROWS * 8 || self.rows >= MAX_ROWS || self.output.len() >= MAX_CHARS {
            self.truncated = true;
            false
        } else {
            true
        }
    }
    fn add(&mut self, label: &str, text: &str) {
        if text.is_empty() || !self.step() {
            return;
        }
        for (at, line) in text.lines().enumerate() {
            if !self.step() {
                break;
            }
            let prefix = if label.is_empty() {
                String::new()
            } else if at == 0 {
                format!("{label}  ")
            } else {
                " ".repeat(label.chars().count() + 2)
            };
            for ch in prefix.chars().chain(line.chars()) {
                // Metadata must not inject terminal escapes or cursor controls into the text view.
                let safe = if ch.is_control() {
                    ch.escape_default().to_string()
                } else {
                    ch.to_string()
                };
                if self.output.len() + safe.len() + 1 > MAX_CHARS {
                    self.truncated = true;
                    break;
                }
                self.output.push_str(&safe);
            }
            self.output.push('\n');
            self.rows += 1;
        }
    }
    fn detail(&mut self, label: &str, value: &Json, depth: usize) {
        if !self.step() || value.is_null() || value["kind"] == "none" {
            return;
        }
        match value {
            Json::String(s) => self.add(label, s),
            Json::Number(_) | Json::Bool(_) => self.add(label, &value.to_string()),
            _ if depth >= 8 => self.truncated = true,
            Json::Array(items) => {
                // Error catalogues are records, not separate vertical cells. Compact each row
                // while retaining the same output/control-character limits as other metadata.
                if !items.is_empty()
                    && items
                        .iter()
                        .take(MAX_ROWS * 8 + 1)
                        .all(|item| item["code"].is_string() && item["meaning"].is_string())
                {
                    self.add(
                        label,
                        if items
                            .iter()
                            .take(MAX_ROWS)
                            .any(|item| item["fix"].is_string())
                        {
                            "Code · Meaning · Fix"
                        } else {
                            "Code · Meaning"
                        },
                    );
                    for item in items {
                        if !self.step() {
                            break;
                        }
                        let mut row = format!(
                            "{} · {}",
                            bounded(text(&item["code"])),
                            bounded(text(&item["meaning"]))
                        );
                        if let Some(fix) = item["fix"].as_str() {
                            row.push_str(" · ");
                            row.push_str(&bounded(fix));
                        }
                        self.add("  ", &row);
                    }
                    return;
                }
                let scalar = items
                    .iter()
                    .take(MAX_ROWS * 8 + 1)
                    .all(|item| item.is_string() || item.is_number() || item.is_boolean());
                if scalar {
                    let mut parts = Vec::new();
                    let mut bytes = 0;
                    for item in items {
                        if !self.step() {
                            break;
                        }
                        let part = item
                            .as_str()
                            .map(str::to_owned)
                            .unwrap_or_else(|| item.to_string());
                        bytes += part.len();
                        parts.push(part);
                        if bytes > MAX_CHARS {
                            self.truncated = true;
                            break;
                        }
                    }
                    self.add(
                        label,
                        &parts.join(if label.to_lowercase().contains("example") {
                            "\n"
                        } else {
                            ", "
                        }),
                    );
                } else {
                    for item in items {
                        if !self.step() {
                            break;
                        }
                        self.detail(label, item, depth + 1);
                    }
                }
            }
            Json::Object(fields) => {
                for (key, value) in fields {
                    if !self.step() {
                        break;
                    }
                    self.detail(&format!("{label} · {}", self::label(key)), value, depth + 1);
                }
            }
            Json::Null => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn metadata_is_bounded_and_cannot_inject_terminal_controls() {
        let mut doc = Document::default();
        doc.add("Example", "first\nsecond\u{1b}[2J\r");
        assert!(doc.output.contains("        second"));
        assert!(!doc.output.contains('\u{1b}'));
        assert!(doc.output.contains("\\u{1b}"));
        doc.detail(
            "Metadata",
            &serde_json::json!(vec!["entry"; MAX_ROWS * 9]),
            0,
        );
        assert!(doc.truncated);
        assert!(doc.rows <= MAX_ROWS);
        let mut large = Document::default();
        large.add("Text", &"x".repeat(MAX_CHARS * 2));
        assert!(large.truncated);
        assert!(large.output.len() <= MAX_CHARS);
    }
}

#[cfg(test)]
mod retrospective_tests {
    use super::*;
    #[test]
    fn catalogue_rows_keep_their_meaning_and_fix_on_one_line() {
        let mut doc = Document::default();
        doc.detail(
            "Codes",
            &serde_json::json!([
                {"code":"CAL003","meaning":"Internal invariant","fix":"Report the failure"},
                {"code":"CAL013","meaning":"Initialization","fix":"Initialize before use"}
            ]),
            0,
        );
        let rows: Vec<_> = doc.output.lines().collect();
        assert_eq!(rows.len(), 3);
        assert!(rows[1].contains("CAL003 · Internal invariant · Report the failure"));
        assert!(rows[2].contains("CAL013 · Initialization · Initialize before use"));
        assert!(!doc.truncated);
    }
}
