use super::*;
type Fields = IndexMap<String, Data>;
fn fields(data: &Data) -> Result<&Fields, LocalError> {
    if let Data::Record(fields) = data {
        Ok(fields)
    } else {
        Err(invalid("HTTP trace requires record-shaped evidence"))
    }
}
fn string<'a>(fields: &'a Fields, key: &str, max: usize) -> Result<&'a str, LocalError> {
    match fields.get(key) {
        Some(Data::Text(s)) if s.len() <= max => Ok(s),
        _ => Err(invalid(
            "HTTP trace has a missing, oversized or non-text field",
        )),
    }
}
fn number(fields: &Fields, key: &str) -> Result<i64, LocalError> {
    match fields.get(key) {
        Some(Data::Int(n)) if *n >= 0 => Ok(*n),
        _ => Err(invalid("HTTP trace requires nonnegative integer evidence")),
    }
}
fn flag(fields: &Fields, key: &str) -> Result<bool, LocalError> {
    match fields.get(key) {
        Some(Data::Bool(b)) => Ok(*b),
        _ => Err(invalid("HTTP trace requires Boolean evidence")),
    }
}
fn array<'a>(fields: &'a Fields, key: &str, max: usize) -> Result<&'a [Data], LocalError> {
    match fields.get(key) {
        Some(Data::List(xs)) if xs.len() <= max => Ok(xs),
        _ => Err(invalid(
            "HTTP trace has a missing or excessive event/header list",
        )),
    }
}
fn preview(data: &Data) -> Result<(i64, bool), LocalError> {
    let f = fields(data)?;
    let bytes = number(f, "bytes")?;
    string(f, "preview", 16384)?;
    Ok((bytes, !flag(f, "complete")?))
}
fn headers(f: &Fields) -> Result<bool, LocalError> {
    let count = number(f, "headerCount")?;
    let headers = array(f, "headers", 64)?;
    if count < headers.len() as i64 {
        return Err(invalid(
            "HTTP trace header count contradicts its retained headers",
        ));
    }
    for header in headers {
        let header = fields(header)?;
        string(header, "name", 8192)?;
        string(header, "value", 2048)?;
    }
    Ok(count > headers.len() as i64)
}
fn finding(code: &str, severity: &str, message: &str, indexes: &[usize]) -> Value {
    record(
        "HttpFinding",
        [
            ("code".into(), text(code)),
            ("severity".into(), text(severity)),
            ("message".into(), text(message)),
            (
                "eventIndexes".into(),
                list(
                    Shape::Primitive(Primitive::Int),
                    indexes.iter().map(|i| integer(*i as i64)),
                ),
            ),
        ],
        Provenance::default(),
    )
}
pub(super) fn analyze(data: &Data, cancellation: &CancellationToken) -> Result<Value, LocalError> {
    let root = fields(data)?;
    if root.get("schema") != Some(&Data::Int(1))
        || root.get("profile") != Some(&Data::Text("http".into()))
    {
        return Err(invalid("HTTP analysis requires a schema-1 HTTP trace"));
    }
    let node = string(root, "node", 256)?;
    let run = string(root, "run", 256)?;
    if node.is_empty() || run.is_empty() || node.chars().chain(run.chars()).any(char::is_control) {
        return Err(invalid("HTTP trace node/run identity is invalid"));
    }
    let state = string(root, "state", 16)?;
    if !matches!(state, "running" | "completed" | "failed" | "cancelled") {
        return Err(invalid("HTTP trace execution state is unsupported"));
    }
    if !matches!(
        string(root, "persistence", 16)?,
        "memory" | "recorded" | "private" | "failed"
    ) {
        return Err(invalid("HTTP trace persistence state is unsupported"));
    }
    let dropped = number(root, "dropped")?;
    let events = array(root, "events", 64)?;
    let mut previous = 0;
    let (mut request, mut response, mut body, mut finish) = (None, None, None, None);
    let mut received = 0;
    let mut truncated = Vec::new();
    let mut unknown = Vec::new();
    let mut statuses = Vec::new();
    let mut errors = Vec::new();
    let mut findings = Vec::new();
    for (index, event) in events.iter().enumerate() {
        if cancellation.is_cancelled() {
            return Err(LocalError::Cancelled);
        }
        let event = fields(event)?;
        let elapsed = number(event, "elapsedMs")?;
        if elapsed < previous || finish.is_some() {
            return Err(invalid("HTTP trace event order is inconsistent"));
        }
        previous = elapsed;
        let kind = string(event, "kind", 64)?;
        if kind.is_empty() {
            return Err(invalid("HTTP trace event kind is empty"));
        }
        let details = event
            .get("details")
            .ok_or_else(|| invalid("HTTP trace event has no details"))?;
        match kind {
            "http.request" => {
                if request.is_some() || response.is_some() || body.is_some() {
                    return Err(invalid(
                        "HTTP trace contains repeated or out-of-order request evidence",
                    ));
                }
                request = Some(index);
                let f = fields(details)?;
                if string(f, "method", 64)?.is_empty() {
                    return Err(invalid("HTTP request method is empty"));
                }
                string(f, "url", 8192)?;
                let mut omitted = headers(f)?;
                if let Some(body) = f.get("body") {
                    omitted |= preview(body)?.1;
                }
                if omitted {
                    truncated.push(index);
                }
            }
            "http.response" => {
                if response.is_some() || body.is_some() {
                    return Err(invalid(
                        "HTTP trace contains repeated or out-of-order response evidence",
                    ));
                }
                let f = fields(details)?;
                let status = catalogue::status(number(f, "status")?)?;
                string(f, "version", 64)?;
                string(f, "url", 8192)?;
                if headers(f)? {
                    truncated.push(index);
                }
                statuses.push(status);
                response = Some(index);
                findings.push(finding("HTTPA_RESPONSE", "info", "An HTTP response status was observed; this alone does not determine whether the application result was accepted.", &[index]));
            }
            "http.progress" => {
                if body.is_some() {
                    return Err(invalid("HTTP progress follows a completed body"));
                }
                let bytes = number(fields(details)?, "receivedBytes")?;
                if bytes < received {
                    return Err(invalid("HTTP received-byte evidence decreases"));
                }
                received = bytes;
            }
            "http.body" => {
                if body.is_some() {
                    return Err(invalid("HTTP trace contains repeated body evidence"));
                }
                let (bytes, omitted) = preview(details)?;
                if bytes < received {
                    return Err(invalid("HTTP body size contradicts progress evidence"));
                }
                body = Some(index);
                if omitted {
                    truncated.push(index);
                }
            }
            "execution.finished" => {
                let f = fields(details)?;
                let completed = string(f, "state", 16)?;
                let code = string(f, "code", 64)?;
                if !matches!(completed, "completed" | "failed" | "cancelled")
                    || (state != "running" && completed != state)
                    || (completed == "failed") == code.is_empty()
                {
                    return Err(invalid("HTTP trace completion evidence is inconsistent"));
                }
                finish = Some(index);
                if !code.is_empty() {
                    let error = catalogue::error(code)?;
                    let (finding_code, message) = match code {
                        "HTTP001" => ("HTTPA_REQUEST_REJECTED", "Request preparation failed."),
                        "HTTP002" => (
                            "HTTPA_CREDENTIAL_UNAVAILABLE",
                            "Credential resolution failed.",
                        ),
                        "HTTP003" => (
                            "HTTPA_TRANSPORT_FAILED",
                            "HTTP transport failed; the remote outcome is not established.",
                        ),
                        "HTTP004" => (
                            "HTTPA_TIMEOUT",
                            "The local HTTP time budget expired; remote completion is uncertain.",
                        ),
                        "HTTP005" => (
                            "HTTPA_REDIRECT_REJECTED",
                            "A redirect was rejected or its limit was reached.",
                        ),
                        "HTTP006" => (
                            "HTTPA_RESPONSE_LIMIT",
                            "The response exceeded its byte budget.",
                        ),
                        "HTTP007" => (
                            "HTTPA_RESPONSE_REJECTED",
                            "Response interpretation or declared-result validation failed. This evidence does not identify the exact invalid field or decoding cause.",
                        ),
                        "HTTP008" => (
                            "HTTPA_STATUS_REJECTED",
                            "The imported operation rejected a received HTTP status.",
                        ),
                        "HTTP009" => (
                            "HTTPA_WORKER_FAILED",
                            "The local HTTP boundary worker failed.",
                        ),
                        _ => (
                            "HTTPA_EXECUTION_FAILED",
                            "Execution failed with a code outside the HTTP adapter catalogue.",
                        ),
                    };
                    let mut evidence = vec![index];
                    if matches!(code, "HTTP007" | "HTTP008")
                        && let Some(response) = response
                    {
                        evidence.insert(0, response);
                    }
                    findings.push(finding(finding_code, "error", message, &evidence));
                    errors.push(error);
                }
            }
            _ => unknown.push(index),
        }
    }
    let missing_finish = finish.is_none();
    let partial = state == "running"
        || dropped > 0
        || !truncated.is_empty()
        || !unknown.is_empty()
        || missing_finish;
    if state == "running" {
        findings.push(finding(
            "HTTPA_RUNNING",
            "info",
            "This is a running snapshot; later evidence may change the analysis.",
            &[],
        ));
    }
    if state == "cancelled" {
        findings.push(finding(
            "HTTPA_CANCELLED",
            "warning",
            "Local execution was cancelled; this does not prove that the remote operation stopped.",
            &finish.into_iter().collect::<Vec<_>>(),
        ));
    }
    if dropped > 0 {
        findings.push(finding("HTTPA_DROPPED", "warning", "Some observations were dropped; missing evidence cannot establish that an event did not happen.", &[]));
    }
    if !truncated.is_empty() {
        findings.push(finding("HTTPA_PREVIEW_TRUNCATED", "warning", "Retained header or body previews are incomplete; analysis does not inspect omitted content.", &truncated));
    }
    if !unknown.is_empty() {
        findings.push(finding(
            "HTTPA_UNSUPPORTED_EVENTS",
            "warning",
            "Some retained event kinds are not interpreted by this analyzer version.",
            &unknown,
        ));
    }
    if missing_finish && state != "running" {
        findings.push(finding(
            "HTTPA_COMPLETION_MISSING",
            "warning",
            "The snapshot is terminal but completion details were not retained.",
            &[],
        ));
    }
    if response.is_none() {
        findings.push(finding("HTTPA_RESPONSE_UNOBSERVED", "info", "No response event is retained in this snapshot. This does not prove that the server received no request or sent no response.", &[]));
    }
    Ok(record(
        "HttpAnalysis",
        [
            ("schema".into(), integer(1)),
            ("analyzer".into(), text("wes.http")),
            ("analyzerVersion".into(), integer(1)),
            ("node".into(), text(node)),
            ("run".into(), text(run)),
            ("executionState".into(), text(state)),
            ("partial".into(), boolean(partial)),
            ("dropped".into(), integer(dropped)),
            (
                "statuses".into(),
                list(catalogue::status(200)?.shape().clone(), statuses),
            ),
            (
                "errors".into(),
                list(catalogue::error("HTTP001")?.shape().clone(), errors),
            ),
            (
                "findings".into(),
                list(finding("", "", "", &[]).shape().clone(), findings),
            ),
        ],
        Provenance::default(),
    ))
}
