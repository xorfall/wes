//! Shared workspace reads expose identities/status, and only exportable result contents.
use super::super::bridge;
use serde_json::{Value, json};
use wes_engine::session::{ObservedCell, SessionObservation};

pub(super) fn cell(
    observation: &SessionObservation,
    cell: &ObservedCell,
    values: bool,
    source: Option<&str>,
) -> Value {
    let mut result = json!({"cell":cell.input.cell(),"parent":cell.input.parent()});
    if let Some(source) = source {
        let mut end = source.len().min(64 * 1024);
        while !source.is_char_boundary(end) {
            end -= 1;
        }
        let text = &source[..end];
        result["source"] = json!({"available":true,"text":text,"truncated":text.len() < source.len(),"bytes":source.len()});
    }
    let Some(Ok(reply)) = &cell.reply else {
        admission(&mut result, cell);
        return result;
    };
    let graph = &observation.state.execution;
    let mut public = !reply.nodes.is_empty() && reply.nodes.len() <= 32;
    let nodes: Vec<_> = reply.nodes.iter().take(32).map(|id| {
        let mut node = json!({"node":id.to_string(),"state":graph.graph.node(id).map(|n| format!("{:?}", n.state()).to_lowercase()),"run":graph.runs.get(id).map(ToString::to_string)});
        node["updatePending"] = json!(graph.input_updates.contains(id));
        if let Some(completion)=crate::execution_status::public_completion(observation,id,source.is_some()) { node["completion"]=completion; }
        if let Some(reason) = graph.stale_reasons.get(id) {
            node["staleReason"] = json!({"code":reason.code(),"message":reason.message()});
        }
        if let Some(waits)=graph.waiting_inputs.get(id){node["waiting"]=crate::execution_status::waiting_inputs(waits, observation.state.names.iter());}
        let data = bridge::current_value(observation, id);
        let exportable = data.is_some_and(|v| !v.provenance().policy().is_confidential() && !v.provenance().policy().is_unknown() && v.data().is_storable_snapshot());
        public &= exportable;
        if exportable {
            node["names"] = json!(observation.state.names.iter().filter(|(_, output)| &output.node == id).map(|(name, _)| name).collect::<Vec<_>>());
        }
        if values {
            if let Some(value) = data { node["result"] = bounded(value); }
            if let Some(error) = graph.errors.get(id) { node["error"] = bounded(&error.to_value()); }
        }
        node
    }).collect();
    result["nodes"] = json!(nodes);
    result["node_total"] = json!(reply.nodes.len());
    result["nodes_truncated"] = json!(reply.nodes.len() > 32);
    result["receipts"] = json!(
        reply
            .receipts
            .iter()
            .map(crate::execution_status::operation_receipt)
            .collect::<Vec<_>>()
    );
    result["diagnostics"] = diagnostics(&reply.diagnostics.diagnostics, source);
    result["public_results"] = json!(public);
    result
}

/// Execution replies and later cell reads share safe admission status and causes.
pub(super) fn admission(result: &mut Value, cell: &ObservedCell) {
    result["settled"] = json!(cell.reply.is_some());
    result["status"] = json!(if cell.reply.is_some() {
        "admission_failed"
    } else {
        "admission_pending_or_unavailable"
    });
    if let Some(Err(error)) = &cell.reply
        && let Some(message) = error.authority_message()
    {
        result["error"] = json!({"code":"AUT001","message":message});
    }
}

pub(super) fn diagnostics(items: &[wes_language::Diagnostic], source: Option<&str>) -> Value {
    json!(items.iter().take(128).map(|d| {
        let mut result = json!({"code":d.code,"severity":format!("{:?}",d.severity).to_lowercase(),
            "message":d.public_summary(),"span":{"start":d.span.start(),"end":d.span.end()}});
        if let Some(subject) = source.and_then(|s| s.get(d.span.start()..d.span.end())) {
            result["subject"] = json!(subject.chars().take(160).collect::<String>());
        }
        result
    }).collect::<Vec<_>>())
}

fn bounded(value: &wes_core::Value) -> Value {
    match bridge::exported_bounded(value, false, 64 * 1024) {
        Ok(text) => {
            json!({"available":true,"value":serde_json::from_str::<Value>(&text).expect("encoded value")})
        }
        Err(_) => {
            json!({"available":false,"reason":"Private, unknown, lazy or larger than the 64 KiB cell preview limit. Use an explicit permitted value read for a larger named result."})
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{Data, Primitive, Provenance, Shape};
    #[test]
    fn import_categories_keep_actionable_causes_without_exporting_private_advisories() {
        use wes_engine::imports::{ImportError, ImportWarning, ImportWarningKind};
        use wes_language::{Diagnostic, Span};
        let mut items =
            vec![ImportError::MissingArgument("endpoint".into()).diagnostic(Span::at(0))];
        for kind in [
            ImportWarningKind::Advisory,
            ImportWarningKind::AuthenticationChoice,
            ImportWarningKind::CredentialUnavailable,
            ImportWarningKind::QueryCredential,
        ] {
            items.push(
                ImportWarning::new(kind, "PRIVATE_CREDENTIAL_AND_OPERATION")
                    .diagnostic(Span::at(0)),
            );
        }
        items.push(Diagnostic::error("IMP004", Span::at(0), "PRIVATE_ALIAS"));
        items.push(Diagnostic::error("ENV039", Span::at(0), "PRIVATE_ENDPOINT"));
        let result = diagnostics(&items, None);
        assert!(result[0]["message"].as_str().unwrap().contains("endpoint"));
        assert_eq!(result[2]["code"], "IMP007");
        assert!(result[2]["message"].as_str().unwrap().contains("bind.auth"));
        assert_eq!(result[3]["code"], "IMP008");
        assert_eq!(result[4]["code"], "IMP009");
        assert!(
            result[5]["message"]
                .as_str()
                .unwrap()
                .contains("replace:true")
        );
        assert!(
            result[6]["message"]
                .as_str()
                .unwrap()
                .contains("No provider was called")
        );
        assert!(!result.to_string().contains("PRIVATE"));
    }
    #[test]
    fn producer_explanations_survive_unknown_codes_without_private_detail_or_source() {
        use wes_language::{Diagnostic, Severity, Span};
        for severity in [Severity::Error, Severity::Warning, Severity::Info] {
            let diagnostic = Diagnostic::error(
                "FUTURE000",
                Span::new(0, 6).unwrap(),
                "PRIVATE_INFERRED_MARKER",
            )
            .with_public_message("Required producer evidence is missing.")
            .with_hint("PRIVATE_HINT")
            .with_severity(severity);
            let hidden = diagnostics(&[diagnostic.clone()], None);
            let visible = diagnostics(&[diagnostic], Some("secret"));
            assert_eq!(
                hidden[0]["message"],
                "Required producer evidence is missing."
            );
            assert_eq!(visible[0]["message"], hidden[0]["message"]);
            assert_eq!(visible[0]["subject"], "secret");
            assert!(hidden[0].get("subject").is_none());
            assert!(!visible.to_string().contains("PRIVATE"));
            assert_eq!(
                visible[0]["severity"],
                format!("{severity:?}").to_lowercase()
            );
        }
        let excessive = Diagnostic::error("FUTURE000", Span::at(0), "private")
            .with_public_message("x".repeat(513));
        assert!(
            !diagnostics(&[excessive], None)
                .to_string()
                .contains(&"x".repeat(513))
        );
    }
    #[test]
    fn diagnostic_explanations_do_not_export_inferred_private_messages_or_hidden_source() {
        let source = ":inspect $missing";
        let items = [wes_language::Diagnostic::error(
            "PLN001",
            wes_language::Span::new(9, 17).unwrap(),
            "PRIVATE_INFERRED_MARKER",
        )];
        let hidden = diagnostics(&items, None);
        assert!(
            hidden[0]["message"]
                .as_str()
                .unwrap()
                .contains("not defined")
        );
        assert!(hidden[0].get("subject").is_none());
        assert!(!hidden.to_string().contains("PRIVATE_INFERRED_MARKER"));
        let own = diagnostics(&items, Some(source));
        assert_eq!(own[0]["subject"], "$missing");
        assert!(!own.to_string().contains("PRIVATE_INFERRED_MARKER"));
    }
    #[test]
    fn public_messages_honor_severity_before_code_families_without_raw_data() {
        use wes_language::{Diagnostic, Severity, Span};
        for code in ["ENV000", "NEW000"] {
            let diagnostic = Diagnostic::error(code, Span::at(0), "PRIVATE_MESSAGE");
            for (severity, expected) in [
                (Severity::Info, "info"),
                (Severity::Warning, "warning"),
                (Severity::Error, "error"),
            ] {
                let result = diagnostics(&[diagnostic.clone().with_severity(severity)], None);
                assert_eq!(result[0]["severity"], expected);
                assert!(!result.to_string().contains("PRIVATE_MESSAGE"));
                let message = result[0]["message"].as_str().unwrap();
                match severity {
                    Severity::Info => {
                        assert_eq!(message, "Informational notice; this is not an error.")
                    }
                    Severity::Warning => assert!(message.starts_with("Warning;")),
                    Severity::Error => {
                        assert!(message.contains("could not") || message.contains("rejected"))
                    }
                }
            }
        }
    }
    #[test]
    fn shared_previews_enforce_size_and_flow_policy_without_leaking_contents() {
        let value = |text: String, provenance| {
            wes_core::Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text(text.into()),
                provenance,
            )
            .unwrap()
        };
        assert_eq!(
            bounded(&value("public".into(), Provenance::default()))["value"],
            "public"
        );
        for policy in [
            wes_core::flow::FlowPolicy::default().private(),
            wes_core::flow::FlowPolicy::default().unknown(),
        ] {
            let result = bounded(&value(
                "hidden-marker".into(),
                Provenance::default().with_policy(&policy),
            ));
            assert_eq!(result["available"], false);
            assert!(!result.to_string().contains("hidden-marker"));
        }
        let large = value("x".repeat(64 * 1024), Provenance::default());
        assert_eq!(bounded(&large)["available"], false);
        assert!(bridge::exported(&large, false).is_ok());
    }
}
