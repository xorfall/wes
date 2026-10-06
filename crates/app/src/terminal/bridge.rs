//! Narrow child-process API: providers, permitted values, and pane control; no raw source submission.
use super::*;
use serde_json::json;
use wes_adapters::codec::{Limits, encode_json, encode_value};
use wes_core::{Value, capability::Catalogue};
use wes_engine::{
    graph::{NodeId, NodeState, OutputPort},
    session::SessionObservation,
    source::SourceInput,
};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BridgeRequest {
    pub tool: String,
    pub args: Vec<String>,
}
#[derive(Serialize, Deserialize)]
pub struct BridgeReply {
    pub code: u8,
    pub stdout: String,
    pub stderr: String,
}
impl BridgeReply {
    pub fn error(code: u8, text: impl Into<String>) -> Self {
        Self {
            code,
            stdout: String::new(),
            stderr: text.into(),
        }
    }
    fn output(text: String) -> Self {
        Self {
            code: 0,
            stdout: text,
            stderr: String::new(),
        }
    }
}
fn identifier(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .all(|c| c.is_alphanumeric() || c == '_' || c == '-')
}
fn quote(value: &str) -> Result<String, BridgeReply> {
    if value.chars().any(char::is_control) {
        return Err(BridgeReply::error(
            2,
            "Control characters are not supported in CLI literals.",
        ));
    }
    Ok(format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    ))
}
pub(super) fn context(
    observation: &SessionObservation,
    client: &str,
) -> Option<wes_core::environments::EnvironmentContext> {
    let saved = observation.environment_clients.get(client);
    (observation.environment_managed || saved.is_some()).then(|| {
        wes_core::environments::EnvironmentContext {
            selected: saved.map_or_else(
                || observation.default_environment.clone(),
                |c| c.selected.clone(),
            ),
            revisions: observation.environment_revisions.clone(),
        }
    })
}
/// A terminal's captured environment does not follow later UI selections. Revisions
/// for new workspace calls remain current and are checked by normal engine admission.
pub(super) fn terminal_context(
    observation: &SessionObservation,
    terminal: &TerminalSession,
) -> Option<wes_core::environments::EnvironmentContext> {
    terminal
        .environment
        .as_ref()
        .map(|environment| wes_core::environments::EnvironmentContext {
            selected: Some(environment.clone()),
            revisions: observation.environment_revisions.clone(),
        })
        .or_else(|| context(observation, &terminal.client))
}
pub(super) fn terminal_catalogue<'a>(
    observation: &'a SessionObservation,
    terminal: &TerminalSession,
) -> Option<&'a Catalogue> {
    match &terminal.environment {
        Some(environment) => observation.environment_catalogues.get(environment),
        None => catalogue(observation, &terminal.client),
    }
}
pub(super) fn catalogue<'a>(
    observation: &'a SessionObservation,
    client: &str,
) -> Option<&'a Catalogue> {
    if let Some(context) = context(observation, client) {
        context
            .selected
            .as_ref()
            .and_then(|name| observation.environment_catalogues.get(name))
    } else {
        Some(&observation.catalogue)
    }
}
pub(super) fn exported(value: &Value, typed: bool) -> Result<String, BridgeReply> {
    exported_bounded(value, typed, 8 * 1024 * 1024)
}
pub(super) fn exported_bounded(
    value: &Value,
    typed: bool,
    bytes: usize,
) -> Result<String, BridgeReply> {
    let policy = value.provenance().policy();
    if policy.is_private() || policy.is_unknown() {
        return Err(BridgeReply::error(
            1,
            "This value cannot be exported to terminal processes.",
        ));
    }
    if !value.data().is_materialized() {
        return Err(BridgeReply::error(
            1,
            "Materialize this value in the workspace before exporting it.",
        ));
    }
    let limits = Limits {
        bytes,
        nodes: 100_000,
    };
    let encoded = if typed {
        encode_value(value, limits)
    } else {
        encode_json(value.data(), limits)
    };
    encoded
        .map(|b| String::from_utf8(b).expect("JSON UTF-8"))
        .map_err(|e| BridgeReply::error(1, e.to_string()))
}
pub(super) fn current_value<'a>(
    observation: &'a SessionObservation,
    node: &NodeId,
) -> Option<&'a Value> {
    let execution = &observation.state.execution;
    (execution.graph.node(node)?.state() == NodeState::Ready)
        .then(|| execution.values.get(node))
        .flatten()
}
/// Display reads can return a labelled stopped observation; execution inputs still use current_value.
pub(super) fn named_observation<'a>(
    observation: &'a SessionObservation,
    name: &str,
) -> Result<(&'a Value, Option<&'a wes_engine::runtime::StoppedValue>), BridgeReply> {
    let reference = name.strip_prefix('$').unwrap_or(name);
    let output = wes_engine::bindings::Bindings::resolve_names(
        &observation.state.names,
        reference,
        &observation.state.execution.graph,
    )
        .filter(|o| o.port == OutputPort::Data)
        .ok_or_else(|| {
            BridgeReply::error(
                1,
                format!(
                    "No data result for reference {:?} in this workspace. Use a result name or node ID (optional leading $).",
                    name.chars().take(256).collect::<String>()
                ),
            )
        })?;
    if let Some(value) = current_value(observation, &output.node) {
        return Ok((value, None));
    }
    if let Some(last) = observation.state.execution.stopped_values.get(&output.node) {
        return Ok((&last.value, Some(last)));
    }
    Err(BridgeReply::error(
        1,
        match observation
            .state
            .execution
            .graph
            .node(&output.node)
            .map(|node| node.state())
        {
            Some(NodeState::Pending) => "Value is pending; its producer has not completed.",
            Some(NodeState::Running) => "Value is running; its producer has not completed.",
            Some(NodeState::Stale) => {
                "Value is stale; no current result is available. If refresh was requested, await its execution_read; otherwise refresh the producer."
            }
            Some(NodeState::Failed) => "Value producer failed; no data result is available.",
            Some(NodeState::Cancelled) => {
                "Value producer was cancelled; no data result is available."
            }
            Some(NodeState::Skipped) => "Value producer was skipped; no data result is available.",
            Some(NodeState::Ready) => "Value contents are no longer available; it was not re-run.",
            None => "Value producer no longer exists.",
        },
    ))
}
pub(super) fn observed_result(
    value: serde_json::Value,
    stopped: Option<&wes_engine::runtime::StoppedValue>,
) -> serde_json::Value {
    match stopped {
        Some(last) => {
            json!({"status":"stopped", "source":last.source.as_str(), "run":last.run.as_str(), "value":value})
        }
        None => value,
    }
}

fn source(catalogue: &Catalogue, tool: &str, args: &[String]) -> Result<String, BridgeReply> {
    let path_end = args
        .iter()
        .position(|s| s.starts_with("--"))
        .unwrap_or(args.len());
    let path = &args[..path_end];
    if !identifier(tool) || path.iter().any(|s| !identifier(s)) {
        return Err(BridgeReply::error(2, "Invalid provider/operation name."));
    }
    let provider = catalogue
        .provider(tool)
        .ok_or_else(|| BridgeReply::error(2, "Provider is unavailable in this environment."))?;
    let cap = provider
        .capability(path)
        .ok_or_else(|| BridgeReply::error(2, "Unknown operation; use --help."))?;
    if cap.streaming {
        return Err(BridgeReply::error(
            2,
            "Streaming operations belong in the wes console.",
        ));
    }
    let mut source = std::iter::once(tool)
        .chain(path.iter().map(String::as_str))
        .collect::<Vec<_>>()
        .join(" ");
    let mut seen = std::collections::BTreeSet::new();
    let mut rest = args[path_end..].iter();
    while let Some(flag) = rest.next() {
        let value = rest
            .next()
            .ok_or_else(|| BridgeReply::error(2, "Every parameter flag needs a value."))?;
        let (key, literal) = if flag == "--wes-ref" {
            let (key, value) = value
                .split_once('=')
                .ok_or_else(|| BridgeReply::error(2, "Use --wes-ref parameter=name.field"))?;
            if !value.split('.').all(identifier) {
                return Err(BridgeReply::error(2, "Invalid workspace reference."));
            }
            (key, format!("${value}"))
        } else {
            let key = flag
                .strip_prefix("--")
                .ok_or_else(|| BridgeReply::error(2, "Expected --parameter VALUE."))?;
            (key, quote(value)?)
        };
        if !identifier(key) || cap.parameter(key).is_none() || !seen.insert(key) {
            return Err(BridgeReply::error(2, "Unknown or duplicate parameter."));
        }
        source.push_str(&format!(" {key}:{literal}"));
    }
    Ok(source)
}
pub(super) async fn dispatch(
    terminal: Arc<TerminalSession>,
    application: ApplicationHandle,
    request: BridgeRequest,
) -> BridgeReply {
    let current = terminal.current.clone();
    dispatch_at(terminal, application, &current, request).await
}
pub(super) async fn dispatch_at(
    terminal: Arc<TerminalSession>,
    application: ApplicationHandle,
    current: &crate::CurrentSession,
    mut request: BridgeRequest,
) -> BridgeReply {
    match perform(&terminal, &application, current, &mut request).await {
        Ok(output) => BridgeReply::output(output),
        Err(error) => error,
    }
}
async fn perform(
    terminal: &TerminalSession,
    application: &ApplicationHandle,
    current: &crate::CurrentSession,
    request: &mut BridgeRequest,
) -> Result<String, BridgeReply> {
    application
        .session_for_generation(&current.generation)
        .map_err(|_| BridgeReply::error(3, "Workspace authority has ended."))?;
    terminal
        .check(application)
        .map_err(|_| BridgeReply::error(3, "Terminal authority has ended."))?;
    if request.tool == "wesx" {
        match request.args.first().map(String::as_str) {
            None | Some("--help") if request.args.len() <= 1 => return Ok(
                "wesx provider --list | wesx provider PROVIDER OPERATION [ARGS]\nwesx value list | wesx value get REFERENCE [--wes-typed]\nwesx tab | wesx --cmd \"/terminal-tab env:DEV target:api\"\nwesx --cmd \"/rsplit xterm\" | wesx exit\nPane commands run in this terminal's pane.".into()),
            Some("provider") => { request.tool = "wes-provider".into(); request.args.remove(0); }
            Some("value") => { request.tool = "wes-value".into(); request.args.remove(0); }
            Some("tab") if request.args.len() == 1 => return commands::dispatch(terminal, "/terminal-tab".into(), None).await,
            Some("exit") if request.args.len() == 1 => return commands::dispatch(terminal, "/close".into(), None).await,
            Some("--cmd") if request.args.len() == 2 => return commands::dispatch(terminal, request.args[1].clone(), None).await,
            _ => return Err(BridgeReply::error(2, "Use wesx --help for provider, value, tab, exit and --cmd usage.")),
        }
    }
    let session = &current.session;
    let mut updates = session
        .subscribe_updates()
        .map_err(|_| BridgeReply::error(3, "Workspace unavailable."))?;
    let observation = session
        .observe()
        .await
        .map_err(|_| BridgeReply::error(3, "Workspace unavailable."))?;
    terminal
        .check(application)
        .map_err(|_| BridgeReply::error(3, "Terminal authority has ended."))?;
    application
        .session_for_generation(&current.generation)
        .map_err(|_| BridgeReply::error(3, "Workspace authority has ended."))?;
    let typed = request.args.last().is_some_and(|s| s == "--wes-typed");
    if typed {
        request.args.pop();
    }
    if request.tool == "wes-value" {
        if request.args.as_slice() == ["--help"] || request.args.is_empty() {
            return Ok("wesx value list | wesx value get REFERENCE [--wes-typed]\nReads permitted data by binding name or node ID (optional leading $), including labelled stopped observations, without invoking their producer.".into());
        }
        if request.args.as_slice() == ["list"] {
            let names: Vec<_> = observation
                .state
                .names
                .iter()
                .filter_map(|(name, output)| {
                    let value = current_value(&observation, &output.node).or_else(|| {
                        observation
                            .state
                            .execution
                            .stopped_values
                            .get(&output.node)
                            .map(|last| &last.value)
                    })?;
                    (output.port == OutputPort::Data
                        && !value.provenance().policy().is_private()
                        && !value.provenance().policy().is_unknown()
                        && value.data().is_materialized())
                    .then_some(name)
                })
                .collect();
            return Ok(json!(names).to_string());
        }
        if request.args.len() != 2 || request.args[0] != "get" {
            return Err(BridgeReply::error(
                2,
                "Use wesx value get REFERENCE [--wes-typed].",
            ));
        }
        let (value, stopped) = named_observation(&observation, &request.args[1])?;
        let text = exported(value, typed)?;
        return match stopped {
            None => Ok(text),
            Some(last) => {
                let value = serde_json::from_str(&text)
                    .map_err(|_| BridgeReply::error(1, "Value unavailable."))?;
                Ok(observed_result(value, Some(last)).to_string())
            }
        };
    }
    let catalogue = terminal_catalogue(&observation, terminal)
        .ok_or_else(|| BridgeReply::error(2, "Select an environment in wes first."))?;
    if request.tool == "wes-provider" {
        if request.args.is_empty() || request.args[0] == "--list" || request.args[0] == "--help" {
            return Ok(json!({"usage":"wesx provider PROVIDER OPERATION --parameter VALUE [--wes-ref parameter=name.field] [--wes-typed]", "providers":catalogue.provider_names().collect::<Vec<_>>(), "values":"wesx value list / get NAME", "scope":"current workspace and current selected environment; native commands win PATH collisions"}).to_string());
        }
        request.tool = request.args.remove(0);
    }
    if request.args.is_empty() || request.args.last().is_some_and(|s| s == "--help") {
        let provider = catalogue
            .provider(&request.tool)
            .ok_or_else(|| BridgeReply::error(2, "Unknown provider."))?;
        return Ok(json!({"provider":request.tool,"operations":provider.capabilities().map(|cap| json!({"path":cap.path,"summary":cap.summary,"streaming":cap.streaming,"parameters":cap.parameters.iter().map(|p| json!({"flag":format!("--{}",p.name),"type":p.shape.to_string(),"required":p.required})).collect::<Vec<_>>(),"result":cap.result.to_string()})).collect::<Vec<_>>()}).to_string());
    }
    let source = source(catalogue, &request.tool, &request.args)?;
    let mut input = SourceInput::new(format!("terminal-{}", uuid::Uuid::new_v4()), source)
        .map_err(|_| BridgeReply::error(2, "Invalid call."))?
        .with_client(terminal.client.clone())
        .map_err(|_| BridgeReply::error(2, "Invalid client."))?;
    if let Some(context) = terminal_context(&observation, terminal) {
        input = input
            .with_environments(context)
            .map_err(|_| BridgeReply::error(2, "Invalid context."))?;
    }
    terminal
        .check(application)
        .map_err(|_| BridgeReply::error(3, "Terminal authority has ended."))?;
    let reply = session
        .submit(input)
        .await
        .map_err(|e| BridgeReply::error(1, e.to_string()))?;
    let Some(node) = reply.nodes.last() else {
        // Diagnostics can contain source literals; engine already applies its source policy.
        return Err(BridgeReply::error(
            2,
            "Call rejected by engine contract validation; inspect its workspace diagnostic.",
        ));
    };
    loop {
        terminal.check(application).map_err(|_| {
            BridgeReply::error(3, "Terminal closed; admitted call may still have effects.")
        })?;
        let observation = session
            .observe()
            .await
            .map_err(|_| BridgeReply::error(3, "Workspace ended; call outcome may be unknown."))?;
        if let Some(value) = current_value(&observation, node) {
            return exported(value, typed);
        }
        let state = observation
            .state
            .execution
            .graph
            .node(node)
            .map(|n| n.state());
        if !matches!(state, Some(NodeState::Pending | NodeState::Running)) {
            return Err(BridgeReply::error(
                1,
                "Provider did not produce a current successful value; inspect the workspace diagnostic.",
            ));
        }
        tokio::select! { _ = terminal.stopped.cancelled() => return Err(BridgeReply::error(3, "Terminal closed; call outcome may be unknown.")), _ = updates.recv() => {} }
    }
}

/// Entry point shared by the ordinary CLI and installed desktop executable. No GUI is initialized.
/// A pane's published program is this executable under the command's own name.
#[cfg(windows)]
pub(super) fn published_tool() -> Option<(String, bool)> {
    super::published::tool()
}
#[cfg(not(windows))]
pub(super) fn published_tool() -> Option<(String, bool)> {
    None
}
pub async fn client() -> Option<u8> {
    let mut args = std::env::args().skip(1);
    let tool = match published_tool() {
        Some((tool, _)) => tool,
        None => {
            if args.next().as_deref() != Some("--terminal-bridge") {
                return None;
            }
            let Some(tool) = args.next() else {
                eprintln!("Missing terminal tool.");
                return Some(2);
            };
            tool
        }
    };
    let result = exchange(tool, args.collect()).await;
    let reply = result.unwrap_or_else(|()| {
        BridgeReply::error(
            3,
            "Terminal bridge returned no reply. An admitted call may have run; no retry was made.",
        )
    });
    if !reply.stdout.is_empty() {
        println!("{}", reply.stdout);
    }
    if !reply.stderr.is_empty() {
        eprintln!("{}", reply.stderr);
    }
    Some(reply.code)
}

/// One connection per MCP process; CLI invocations create their own short-lived connection.
#[derive(Clone)]
pub(super) struct Connection {
    url: reqwest::Url,
    token: String,
    client: reqwest::Client,
}
impl Connection {
    pub(super) fn from_env() -> Result<Self, ()> {
        Self::new(
            &std::env::var("WES_BRIDGE_URL").map_err(|_| ())?,
            std::env::var("WES_BRIDGE_TOKEN").map_err(|_| ())?,
        )
    }
    /// A pane's own record of its bridge, for a server started by a client that passes on
    /// none of the pane's environment.
    pub(super) fn from_file(path: &std::path::Path) -> Result<Self, ()> {
        use std::io::Read;
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Record {
            url: String,
            token: String,
        }
        let mut bytes = Vec::new();
        std::fs::File::open(path)
            .map_err(|_| ())?
            .take(16 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ())?;
        let record: Record = serde_json::from_slice(&bytes).map_err(|_| ())?;
        Self::new(&record.url, record.token)
    }
    fn new(url: &str, token: String) -> Result<Self, ()> {
        let url = reqwest::Url::parse(url).map_err(|_| ())?;
        if url.scheme() != "http"
            || url.host_str() != Some("127.0.0.1")
            || url.path() != "/terminal-bridge"
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(());
        }
        let client = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(185))
            .build()
            .map_err(|_| ())?;
        Ok(Self { url, token, client })
    }
    pub(super) async fn exchange(
        &self,
        tool: String,
        args: Vec<String>,
    ) -> Result<BridgeReply, ()> {
        let mut response = self
            .client
            .post(self.url.clone())
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .body(serde_json::to_vec(&BridgeRequest { tool, args }).map_err(|_| ())?)
            .send()
            .await
            .map_err(|_| ())?
            .error_for_status()
            .map_err(|_| ())?;
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| ())? {
            if bytes.len() + chunk.len() > 16 * 1024 * 1024 {
                return Err(());
            }
            bytes.extend_from_slice(&chunk);
        }
        serde_json::from_slice::<BridgeReply>(&bytes).map_err(|_| ())
    }
}
pub(super) async fn exchange(tool: String, args: Vec<String>) -> Result<BridgeReply, ()> {
    Connection::from_env()?.exchange(tool, args).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_core::{
        Data, Primitive, Provenance, Shape,
        capability::{Capability, Parameter, ProviderDescription, Safety},
    };
    #[test]
    fn argv_is_literal_data_and_references_are_explicit() {
        let mut cap = Capability::new(["echo"], Shape::Unknown, Safety::Safe);
        cap.parameters
            .push(Parameter::new("value", Shape::Unknown, true));
        let mut catalogue = Catalogue::new();
        catalogue.register(ProviderDescription::new("demo", [cap], vec![]).unwrap());
        let hostile = r#"x\" > stolen; $(touch nope) $secret `command`"#;
        let args = vec!["echo".into(), "--value".into(), hostile.into()];
        let text = source(&catalogue, "demo", &args).unwrap_or_else(|e| panic!("{}", e.stderr));
        let parsed = wes_language::parse(&wes_language::SourceText::new("test", &text));
        assert!(parsed.diagnostics.is_empty(), "{:?}", parsed.diagnostics);
        assert_eq!(parsed.script.statements.len(), 1);
        let statement = &parsed.script.statements[0];
        assert!(statement.binding.is_none());
        let wes_language::Expression::Call(call) = &statement.expression else {
            panic!()
        };
        assert_eq!(call.arguments[0].value.name().text, hostile);
        for args in [
            vec!["echo", "--value", "x\n:name unbind \"all\""],
            vec!["echo", "--value", "x", "--value", "y"],
            vec!["echo", "--wes-ref", "value=a;drop"],
            vec!["echo", "--unknown", "x"],
        ] {
            assert!(
                source(
                    &catalogue,
                    "demo",
                    &args.into_iter().map(str::to_owned).collect::<Vec<_>>()
                )
                .is_err()
            );
        }
        assert_eq!(
            source(
                &catalogue,
                "demo",
                &["echo".into(), "--wes-ref".into(), "value=rates.TRY".into()]
            )
            .unwrap_or_default(),
            "demo echo value:$rates.TRY"
        );
    }

    #[tokio::test]
    async fn result_identity_preserves_availability_stopped_labels_and_egress_policy() {
        let root = tempfile::tempdir().unwrap();
        let runtime = crate::runtime::launch(crate::runtime::RuntimeOptions::new(
            root.path().join("home"),
            root.path().into(),
        ))
        .await
        .unwrap();
        let session = runtime.handle.current().unwrap().session;
        let reply = session
            .submit(
                wes_engine::source::SourceInput::new(
                    "result-read".into(),
                    ":calc { return 42; } > result".into(),
                )
                .unwrap(),
            )
            .await
            .unwrap();
        session.wait_idle().await.unwrap();
        let node = reply.nodes[0].clone();
        let mut observation = session.observe().await.unwrap();
        let spellings = [
            "result".to_string(),
            "$result".into(),
            node.to_string(),
            format!("${node}"),
        ];
        for reference in &spellings {
            let (value, stopped) = named_observation(&observation, reference)
                .unwrap_or_else(|e| panic!("{}", e.stderr));
            assert_eq!(value.data(), &Data::Int(42));
            assert!(stopped.is_none());
        }
        assert!(named_observation(&observation, &format!("{node}::error")).is_err());
        assert!(named_observation(&observation, "$id999999999").is_err());
        for policy in [
            wes_core::flow::FlowPolicy::default().private(),
            wes_core::flow::FlowPolicy::default().unknown(),
        ] {
            let private = Value::new(
                Shape::Primitive(Primitive::Text),
                Data::Text("withheld".into()),
                Provenance::default().with_policy(&policy),
            )
            .unwrap();
            observation
                .state
                .execution
                .values
                .insert(node.clone(), private);
            for reference in &spellings {
                let (value, _) = named_observation(&observation, reference)
                    .unwrap_or_else(|e| panic!("{}", e.stderr));
                assert!(exported(value, false).is_err());
                assert!(exported(value, true).is_err());
            }
        }
        observation
            .state
            .execution
            .graph
            .set_state(&node, NodeState::Cancelled)
            .unwrap();
        assert!(named_observation(&observation, &node.to_string()).is_err());
        let last = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(42),
            Provenance::default(),
        )
        .unwrap();
        observation.state.execution.stopped_values.insert(
            node.clone(),
            wes_engine::runtime::StoppedValue {
                value: last,
                source: node.clone(),
                run: wes_engine::runtime::RunId::new("synthetic-run").unwrap(),
            },
        );
        for reference in &spellings {
            let (value, stopped) = named_observation(&observation, reference)
                .unwrap_or_else(|e| panic!("{}", e.stderr));
            let data = observed_result(json!(value.data() == &Data::Int(42)), stopped);
            assert_eq!(data["status"], "stopped");
            assert_eq!(data["source"], node.as_str());
            assert!(current_value(&observation, &node).is_none());
        }
        observation.state.execution.stopped_values.clear();
        observation
            .state
            .execution
            .graph
            .set_state(&node, NodeState::Stale)
            .unwrap();
        assert!(
            named_observation(&observation, &node.to_string())
                .unwrap_err()
                .stderr
                .contains("stale")
        );
    }

    #[test]
    fn export_preserves_types_and_denies_private_and_unknown_values() {
        let decimal = Value::new(
            Shape::Primitive(Primitive::Decimal),
            Data::Decimal("1.2345678901234567890123456789".parse().unwrap()),
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(
            exported(&decimal, false).ok().unwrap(),
            "1.2345678901234567890123456789"
        );
        let typed: serde_json::Value =
            serde_json::from_str(&exported(&decimal, true).ok().unwrap()).unwrap();
        assert_eq!(typed["data"]["kind"], "decimal");
        assert_eq!(typed["data"]["value"], "1.2345678901234567890123456789");
        let bytes = Value::new(
            Shape::Primitive(Primitive::Bytes),
            Data::Bytes(vec![0, 255].into()),
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(exported(&bytes, false).ok().unwrap(), "\"AP8=\"");
        let option = Value::new(
            Shape::Option(Box::new(Shape::Primitive(Primitive::Text))),
            Data::Option(None),
            Provenance::default(),
        )
        .unwrap();
        assert_eq!(
            exported(&option, false).ok().unwrap(),
            "{\"kind\":\"none\"}"
        );
        // Policy labels are joined by the core constructor; never inferred from display facts.
        let private = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("never print".into()),
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().private()),
        )
        .unwrap();
        assert!(exported(&private, false).is_err());
        assert!(exported(&private, true).is_err());
        let unknown = Value::new(
            Shape::Primitive(Primitive::Text),
            Data::Text("unknown".into()),
            Provenance::default().with_policy(&wes_core::flow::FlowPolicy::default().unknown()),
        )
        .unwrap();
        assert!(exported(&unknown, false).is_err());
    }
}
