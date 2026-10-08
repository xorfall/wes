//! The advertised schema is also the argument contract at the authorized tool boundary.
use serde_json::Value;

pub(in crate::terminal::assistant) fn validate(request: &Value) -> Result<(), String> {
    let name = request["name"]
        .as_str()
        .ok_or("Tool request.name must be a string.")?;
    let tool = super::registry()["tools"]
        .as_array()
        .expect("registry")
        .iter()
        .find(|tool| tool["name"] == name)
        .ok_or("Unknown tool name; use tools/list to discover supported tools.")?;
    let arguments = request.get("arguments").ok_or_else(|| {
        format!("{name}: arguments must be an object; use its inputSchema from tools/list.")
    })?;
    check(arguments, &tool["inputSchema"], "arguments")
        .map_err(|message| format!("{name}: {message}"))
}

fn check(value: &Value, schema: &Value, path: &str) -> Result<(), String> {
    let kind = schema["type"].as_str().expect("registry type");
    let matches = match kind {
        "object" => value.is_object(),
        "array" => value.is_array(),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        "integer" => value.is_i64() || value.is_u64(),
        _ => panic!("unsupported tool schema type: {kind}"),
    };
    if !matches {
        return Err(format!("{path} must be {kind}."));
    }
    if let Some(choices) = schema["enum"].as_array() {
        if !choices.contains(value) {
            return Err(format!(
                "{path} must be one of {}.",
                choices
                    .iter()
                    .map(Value::to_string)
                    .collect::<Vec<_>>()
                    .join(", ")
            ));
        }
    }
    match kind {
        "object" => {
            let object = value.as_object().expect("checked");
            let properties = schema["properties"]
                .as_object()
                .expect("registry properties");
            let missing = schema["required"]
                .as_array()
                .expect("registry required")
                .iter()
                .filter_map(|key| key.as_str().filter(|key| !object.contains_key(*key)))
                .collect::<Vec<_>>();
            if !missing.is_empty() {
                return Err(format!(
                    "{path} is missing required field(s): {}.",
                    missing.join(", ")
                ));
            }
            for (key, value) in object {
                let Some(declaration) = properties.get(key) else {
                    // Do not echo arbitrary keys: they can contain private input too.
                    return Err(format!(
                        "{path} contains an unexpected field; allowed fields: {}.",
                        properties.keys().cloned().collect::<Vec<_>>().join(", ")
                    ));
                };
                check(value, declaration, &format!("{path}.{key}"))?;
            }
        }
        "array" => {
            for (i, item) in value.as_array().expect("checked").iter().enumerate() {
                check(item, &schema["items"], &format!("{path}[{i}]"))?;
            }
        }
        "integer" => {
            let number = value
                .as_i64()
                .map(i128::from)
                .or_else(|| value.as_u64().map(i128::from))
                .expect("checked");
            for (key, comparison) in [("minimum", true), ("maximum", false)] {
                if let Some(bound) = schema[key].as_i64() {
                    if (comparison && number < i128::from(bound))
                        || (!comparison && number > i128::from(bound))
                    {
                        return Err(format!(
                            "{path} must be {} {bound}.",
                            if comparison { "at least" } else { "at most" }
                        ));
                    }
                }
            }
        }
        "string" => {
            let text = value.as_str().expect("checked");
            if let Some(max) = schema["maxLength"].as_u64() {
                if text.chars().count() as u64 > max {
                    return Err(format!("{path} must have at most {max} characters."));
                }
            }
            if let Some(pattern) = schema["pattern"].as_str() {
                assert_eq!(pattern, "^[0-9a-f]{64}$", "unsupported registry pattern");
                if text.len() != 64
                    || !text
                        .bytes()
                        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                {
                    return Err(format!(
                        "{path} must contain exactly 64 lowercase hexadecimal characters."
                    ));
                }
            }
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn errors_explain_schema_without_echoing_values_or_untrusted_keys() {
        for (request, expected) in [
            (
                json!({"name":"execute","arguments":{"source":"PRIVATE","context":"SECRET"}}),
                "request_id",
            ),
            (json!({"name":"pane_command","arguments":{}}), "command"),
            (
                json!({"name":"value_read","arguments":{"name":"PRIVATE","typed":"SECRET"}}),
                "arguments.typed must be boolean",
            ),
            (
                json!({"name":"value_read","arguments":{"name":"PRIVATE","limit":0}}),
                "arguments.limit must be at least 1",
            ),
            (
                json!({"name":"help","arguments":{"tail":["calc",42]}}),
                "arguments.tail[1] must be string",
            ),
            (
                json!({"name":"cancel","arguments":{"workspace":"demo","request_id":"PRIVATE","SECRET":true}}),
                "unexpected field",
            ),
            (
                json!({"name":"spec_read","arguments":{"key":{"service":"PRIVATE"},"revision":"SECRET"}}),
                "apiVersion, scope",
            ),
            (
                json!({"name":"spec_read","arguments":{"key":{"service":"PRIVATE","apiVersion":"v1","scope":"test"},"revision":"SECRET"}}),
                "64 lowercase hexadecimal",
            ),
            (
                json!({"name":"execute","arguments":{"source":"PRIVATE","context":"SECRET","workspace":"demo","request_id":"x","results":"HIDDEN"}}),
                "\"summary\", \"full\"",
            ),
            (json!({"name":"HIDDEN","arguments":{}}), "Unknown tool name"),
        ] {
            let error = validate(&request).unwrap_err();
            assert!(error.contains(expected), "{error}");
            for secret in ["PRIVATE", "SECRET", "HIDDEN"] {
                assert!(!error.contains(secret), "{error}");
            }
        }
    }
    #[test]
    fn valid_nested_and_workspace_arguments_remain_accepted() {
        validate(&json!({"name":"spec_read","arguments":{"key":{"service":"demo","apiVersion":"v1","scope":"test"},"revision":"a".repeat(64)}})).unwrap();
        validate(&json!({"name":"value_read","arguments":{"name":"$id1005","workspace":"demo","limit":1000,"typed":true}})).unwrap();
        validate(&json!({"name":"help","arguments":{"command":"calc","tail":["iter.matches"],"depth":3}})).unwrap();
        assert!(validate(&json!({"name":"layout_read","arguments":{"workspace":"demo"}})).is_err());
    }
    #[test]
    fn dataset_streams_are_discoverable_closed_choices_before_dispatch() {
        for name in ["dataset_inspect", "dataset_page"] {
            for stream in [None, Some("outputs"), Some("coverage")] {
                let mut request =
                    json!({"name":name,"arguments":{"name":"analysis","select":"/outputs"}});
                if let Some(stream) = stream {
                    request["arguments"]["stream"] = json!(stream);
                }
                validate(&request).unwrap();
            }
            let refused = json!({"name":name,"arguments":{"name":"analysis","stream":"unknown"}});
            let error = validate(&refused).unwrap_err();
            assert!(error.contains("\"outputs\", \"coverage\""), "{error}");
        }
    }
}
