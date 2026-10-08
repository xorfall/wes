//! Bounded newline-delimited JSON-RPC stdio transport. No model SDK or HTTP MCP server.
use super::metrics::{Collection, CountingWriter, Meter, Outcome, Ticket};
use serde_json::{Value, json};
use std::{
    future::Future,
    io::{BufRead, Read, Write},
    sync::{Arc, LazyLock},
};
use tokio::sync::{Semaphore, mpsc};
use tokio_util::task::TaskTracker;
mod arguments;
pub(super) use arguments::validate as validate_call;

const LIMIT: u64 = 128 * 1024;
fn error(id: Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc":"2.0","id":id,"error":{"code":code,"message":message}})
}
fn schema(properties: Value, required: &[&str]) -> Value {
    json!({"type":"object","properties":properties,"required":required,"additionalProperties":false})
}
fn tool(name: &str, description: &str, mut properties: Value, required: &[&str]) -> Value {
    let explicit_workspace = matches!(name, "execute" | "cancel" | "tab_open");
    if !matches!(
        name,
        "workspace_open"
            | "layout_read"
            | "pane_command"
            | "draft_read"
            | "draft_update"
            | "spec_list"
            | "spec_read"
            | "spec_save"
            | "view_authoring"
            | "view_toolchain"
    ) {
        properties["workspace"] = json!({"type":"string","description":if explicit_workspace {"Explicit target workspace name, including the originating workspace. Use workspace_context or workspace_open to obtain it."} else {"Joined workspace name; omit for your originating workspace."}});
    }
    let required: Vec<_> = required
        .iter()
        .copied()
        .chain(explicit_workspace.then_some("workspace"))
        .collect();
    json!({"name":name,"description":description,"inputSchema":schema(properties, &required)})
}
fn tools() -> Value {
    registry().clone()
}
pub(super) fn registry() -> &'static Value {
    static REGISTRY: LazyLock<Value> = LazyLock::new(build_tools);
    &REGISTRY
}
fn build_tools() -> Value {
    let string = json!({"type":"string"});
    let extent = schema(
        json!({
            "store": {"type":"string","maxLength":36},
            "dataset": {"type":"string","maxLength":36},
            "generation": {"type":"string","maxLength":20},
            "manifest": {"type":"string","maxLength":36},
            "manifestDigest": {"type":"string","maxLength":71},
            "manifestBytes": {"type":"string","maxLength":20},
            "schemaDigest": {"type":"string","maxLength":71},
            "records": {"type":"string","maxLength":20},
            "authorizationGeneration": {"type":"string","maxLength":20}
        }),
        &[
            "store",
            "dataset",
            "generation",
            "manifest",
            "manifestDigest",
            "manifestBytes",
            "schemaDigest",
            "records",
            "authorizationGeneration",
        ],
    );
    let key = schema(
        json!({"service":string,"apiVersion":string,"scope":string}),
        &["service", "apiVersion", "scope"],
    );
    let revision = json!({"type":"string","pattern":"^[0-9a-f]{64}$"});
    let wait = json!({"type":"integer","minimum":0,"maximum":1000,"default":0,"description":"Return when ready/failed or after this many ms; an open stream can be ready without ending."});
    json!({"tools":[
        tool("view_authoring","Read current View authoring guidance, actual SDK declarations, theme roles, layout sizing/placement rules and builtin layouts, or a complete starter. No repository checkout required. Read overview and types first, then sdk/theme/layout before coding; readonly, no files, builds, installs or provider calls.",json!({"topic":{"type":"string","enum":["overview","sdk","theme","layout","examples","types"],"default":"overview"}}),&[]),
        tool("view_toolchain","Readonly inventory of the Wes backend host's embedded View compiler sources, native validator and Node presence, with repository-free export/setup instructions. Not the agent terminal or selected environment. Does not execute versions, install, export, compile or grant filesystem access.",json!({}),&[]),
        tool("view_render_status","Read drawing acknowledgements for one public live ViewInstance by binding or node ID (optional $) on the originating connected UI document. No mounting, source execution, observation changes, input/pixels/console bodies or cross-window guarantees. Current draw acknowledgement proves delivery, not visual correctness; nested members are separate, linked-input freshness is unverified. No UI means unverified.",json!({"name":{"type":"string","maxLength":256}}),&["name"]),
        tool("spec_list","List saved API draft revisions explicitly shared by the user in /spec. Home-wide, not workspace-local; no unsaved editor buffers. Default page 50, oldest first.",json!({"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100}}),&[]),
        tool("spec_read","Read a shared saved API draft revision (source up to 64 KiB), exact-text validation and evidence status. evidence:true includes provenance. Diagnostics pages contain up to 100 entries; diagnostics_offset selects the page. Unsaved editor changes are not included.",json!({"key":key,"revision":revision,"evidence":{"type":"boolean"},"diagnostics_offset":{"type":"integer","minimum":0}}),&["key","revision"]),
        tool("spec_save","Save up to 64 KiB of edited API draft text against its exact latest revision. Returns the saved revision and validation, including problems; no import, API call, merge or UI buffer replacement. User must share this draft in /spec. Stale saves fail. After uncertain replies list/read before retrying.",json!({"key":key,"revision":revision,"text":string}),&["key","revision","text"]),
        tool("workspace_open","Join an existing named workspace in this data home, or create if create=true. Membership belongs to the live terminal session: after reopening a pane or backend restart, join other workspaces again with create:false and use the fresh returned context. Joining does not replay requests or restore prior grants. Multiple agents and the user share one session. Does not redirect later calls: pass returned workspace explicitly. Does not move terminal/editor or open a tab; no inherited environment grants.",json!({"name":string,"create":{"type":"boolean","default":false}}),&["name"]),
        tool("workspace_snapshot","Read a bounded page of shared user/agent cell IDs, node status and public result names. No source or result bodies. Does not rerun.",json!({"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":100}}),&[]),
        tool("cell_read","Read one cell's public results/errors and actionable diagnostics. source:true reads your own or explicitly source-granted text (bounded); editing permission is separate. No rerun.",json!({"cell":string,"source":{"type":"boolean"}}),&["cell"]),
        tool("layout_read","Read connected UI pane IDs and workspace tabs, without drafts or result bodies. Use before tab_open; UI must be connected.",json!({}),&[]),
        tool("tab_open","Open a joined workspace in an explicit existing wes pane. Same workspace/pane is reused. Keeps focus unless activate=true; does not rebind the terminal. Read layout after an uncertain reply.",json!({"pane":string,"activate":{"type":"boolean"}}),&["pane"]),
        tool("workspace_context","Read the attached workspace, current environment token, available providers. Start here; reuse context until it changes.",json!({}),&[]),
        tool("help","Discover provider operations or a meta command. No execution. depth:1..3 includes descendant signatures in one bounded read (128 entries, 64 KiB); choose a narrower path if too large. Use command import with tail [openapi] for OpenAPI JSON/YAML or [spec] for a ready Wes descriptor; both require endpoint and exactly one of file/url. For calculation functions use command calc with tail [iter.matches] (or another listed operation): signatures, returns, behavior and examples.",json!({"provider":string,"command":string,"tail":{"type":"array","items":{"type":"string"}},"depth":{"type":"integer","minimum":0,"maximum":3,"default":0}}),&[]),
        tool("values_list","List current public materialized values without rerunning producers.",json!({}),&[]),
        tool("value_read","Read an existing public data result by binding name or node ID, with an optional leading $ (e.g. total, $total, id1005, $id1005). No new cells or reruns. Error-output references are not data; use cell_read for errors. Omit selection options for full value (8 MiB). select is a JSON Pointer; offset/limit page a selected list. shape_only returns kind/length; typed:true adds the schema and optional captured contract meta (also for shape_only). Missing meta means unknown. Selected responses wrap value and optional page; 64 KiB budget. typed preserves scalar kinds. Terminal evidence returns {status:stopped|incomplete,source,run,value}; stopped labels a stopped stream observation and incomplete labels committed partial data from a failed analysis. Neither is a successful calc input; the producer remains stopped or failed. Use calc for filtering/aggregation.",json!({"name":string,"typed":{"type":"boolean"},"select":{"type":"string","maxLength":2048,"description":"JSON Pointer, e.g. /body/items/0; empty selects root. Record keys and list indices only."},"offset":{"type":"integer","minimum":0},"limit":{"type":"integer","minimum":1,"maximum":1000,"description":"List page size; default 50 when paging."},"shape_only":{"type":"boolean","description":"Metadata only; cannot combine with offset/limit."}}),&["name"]),
        tool("dataset_inspect","Inspect a public Dataset selected from an existing result. name is a binding or node ID; select is a JSON Pointer (for example /outputs). Returns the exact committed prefix, lifecycle, protection and persistence. stream defaults to outputs; coverage selects acknowledged rejected frames only when the analysis actually has that stream. Optional coverage summary describes that same parent manifest; reference.records always counts ordinary outputs. head:true explicitly reads the newer committed head within the same recording epoch or analysis attempt; it refuses a changed attempt. Omit head for the saved input. Does not start, resume or rerun any work.",json!({"name":string,"select":{"type":"string","maxLength":2048},"head":{"type":"boolean"},"stream":{"type":"string","enum":["outputs","coverage"],"default":"outputs"}}),&["name"]),
        tool("dataset_page","Read a bounded typed Dataset page selected from an existing public result. from is a canonical unsigned decimal STRING; never use a floating point ordinal. limit is 1–100, default 50. Optional extent is the exact reference returned by dataset_inspect head:true; it remains constrained to the existing result and never grants control. stream defaults to outputs; coverage pages the analysis's acknowledged rejected frames with their captured typed schema and bounded original-byte excerpts. Coverage rows have dense page ordinals; recordOrdinal inside a row is the original input ordinal. Parent reference.records still counts ordinary outputs; coverage.records counts skipped rows. Missing coverage refuses. Returned cursor binds the exact manifest, selection and stream; use cursor OR from, never both. extentExhausted means end of this snapshot, not source EOF. No source start, recording, analysis or automatic refresh. Replies obey 64 KiB; reduce limit on an encoding refusal.",json!({"name":string,"select":{"type":"string","maxLength":2048},"from":{"type":"string","maxLength":20},"cursor":{"type":"string","maxLength":4096},"limit":{"type":"integer","minimum":1,"maximum":100},"extent":extent,"stream":{"type":"string","enum":["outputs","coverage"],"default":"outputs"}}),&["name"]),
        tool("validate","Check source syntax and assistant admission only. Does not execute or guarantee type/runtime success.",json!({"source":string}),&["source"]),
        tool("execute","Execute up to 64 declarations/calls, including streams and pipelines. Immediate env/refresh/cancel/wait commands run separately. Shared work is protected at commit. reactive applies only to created nodes. Results default to summary; request full only when needed. sequential:true waits for each top-level statement to finish before the next; stops on failure, cancellation or open stream, without rollback. Prechecks syntax/admission; binding/types are checked per step. Use it for create/connect workflows. Reuse workspace/request_id/source/context/reactive/sequential on lost replies; response includes next context.",json!({"source":string,"context":string,"request_id":string,"wait_ms":wait,"reactive":{"type":"boolean","default":false},"sequential":{"type":"boolean","default":false},"results":{"type":"string","enum":["summary","full"],"default":"summary"}}),&["source","context","request_id"]),
        tool("execution_read","Read a request in this saved terminal pane and workspace, including after reopening. Public results/errors and optional trace. Does not rerun or restore grants.",json!({"request_id":string,"trace":{"type":"boolean"},"wait_ms":wait,"results":{"type":"string","enum":["summary","full"],"default":"summary"}}),&["request_id"]),
        tool("cancel","Request cancellation of an execution belonging to this terminal. During sequential admission this may return busy; read the same request and retry cancellation later. External effects may remain.",json!({"request_id":string}),&["request_id"]),
        tool("pane_command","Apply a pane /command in the originating terminal pane, regardless of current focus. Supports /split, /lsplit, /rsplit, /tsplit, /bsplit, x variants, /terminal-tab and /close. /terminal-tab creates and activates a terminal tab in the originating terminal pane; /close closes the originating tab. xterm accepts env:NAME and target:NAME; defaults to this actor’s environment and its unique target (wesx exit). UI must be connected. /close ends this terminal and may close MCP before a reply. Never blindly retry an uncertain reply; inspect the layout first.",json!({"command":{"type":"string","maxLength":1024,"description":"One slash command, up to 1024 UTF-8 bytes; e.g. /rsplit xterm or /bsplitx /graph. No engine source."}}),&["command"]),
        tool("draft_read","Ask the attached Workspace editor for its current text and revision. UI must be open; no execution.",json!({}),&[]),
        tool("draft_update","Write code into the attached Workspace prompt only if the exact draft revision still matches. Waits for UI acknowledgement; does not execute. On timeout read before retrying.",json!({"text":string,"revision":string}),&["text","revision"])
    ]})
}
#[derive(Default)]
struct Protocol {
    initialized: bool,
    ready: bool,
}
impl Protocol {
    fn receive(
        &mut self,
        message: Value,
        call: impl FnOnce(Value, Value) -> Option<Value>,
    ) -> Option<Value> {
        let id = message.get("id").cloned();
        if !message.is_object()
            || message["jsonrpc"] != "2.0"
            || !message["method"].is_string()
            || id
                .as_ref()
                .is_some_and(|id| !id.is_string() && !id.is_i64() && !id.is_u64())
        {
            return Some(error(Value::Null, -32600, "Invalid JSON-RPC request."));
        }
        let method = message["method"].as_str().expect("method");
        if id.is_none() {
            if method == "notifications/initialized" && self.initialized {
                self.ready = true;
            }
            return None;
        }
        let id = id.expect("request");
        let params = message.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" if !self.initialized => {
                if !params["protocolVersion"].is_string()
                    || !params["clientInfo"].is_object()
                    || !params["capabilities"].is_object()
                {
                    return Some(error(id, -32602, "Invalid initialize parameters."));
                }
                self.initialized = true;
                let proposed = params["protocolVersion"].as_str().expect("version");
                let version = if matches!(
                    proposed,
                    "2024-11-05" | "2025-03-26" | "2025-06-18" | "2025-11-25"
                ) {
                    proposed
                } else {
                    "2025-11-25"
                };
                json!({"protocolVersion":version,"capabilities":{"tools":{}},"serverInfo":{"name":"wes-workspace","version":env!("CARGO_PKG_VERSION")},"instructions":super::GUIDE})
            }
            "ping" => json!({}),
            _ if !self.ready => {
                return Some(error(
                    id,
                    -32000,
                    "Initialize and send notifications/initialized first.",
                ));
            }
            "tools/list" => tools(),
            "tools/call" => {
                if !params["name"].is_string()
                    || params.get("arguments").is_some_and(|a| !a.is_object())
                {
                    return Some(error(
                        id,
                        -32602,
                        if !params["name"].is_string() {
                            "tools/call.name must be a string; use tools/list to discover tool names."
                        } else {
                            "tools/call.arguments must be an object; use the tool inputSchema from tools/list."
                        },
                    ));
                }
                call(
                    id.clone(),
                    json!({"name":params["name"],"arguments":params.get("arguments").cloned().unwrap_or_else(||json!({}))}),
                )?
            }
            _ => return Some(error(id, -32601, "Method not found.")),
        };
        Some(json!({"jsonrpc":"2.0","id":id,"result":result}))
    }
}
const CALL_CAPACITY: usize = 16;
const CANCEL_CAPACITY: usize = 4;

fn tool_error(message: &str) -> Value {
    json!({"content":[{"type":"text","text":message}],"isError":true})
}
struct CallReply {
    value: Value,
    outcome: Outcome,
}
impl From<Value> for CallReply {
    fn from(value: Value) -> Self {
        let outcome = if value["isError"] == true {
            Outcome::Failed
        } else {
            Outcome::Ok
        };
        Self { value, outcome }
    }
}
struct Reply {
    value: Value,
    ticket: Ticket,
    outcome: Outcome,
}
pub(super) fn serve(
    runtime: tokio::runtime::Handle,
    connection: Result<super::super::bridge::Connection, ()>,
) -> u8 {
    let collection = Collection::from_env();
    let code = serve_io(
        std::io::stdin().lock(),
        std::io::stdout(),
        runtime,
        collection.meter.clone(),
        move |tool| {
            let connection = connection.clone();
            async move {
                let result = match connection {
                    Ok(connection) => {
                        connection
                            .exchange("wes-agent".into(), vec![tool.to_string()])
                            .await
                    }
                    Err(()) => Err(()),
                };
                match result {
                    Ok(reply) => CallReply {
                        outcome: match reply.code {
                            0 => Outcome::Ok,
                            2 => Outcome::Rejected,
                            3 => Outcome::Unavailable,
                            _ => Outcome::Failed,
                        },
                        value: json!({"content":[{"type":"text","text":if reply.code==0 {reply.stdout}else{reply.stderr}}],"isError":reply.code!=0}),
                    },
                    Err(()) => CallReply {
                        outcome: Outcome::Unavailable,
                        value: tool_error(if tool["name"] == "pane_command" {
                            "Pane connection unavailable or reply lost. The command may have been applied; /close can end this connection. Inspect the layout before repeating. No retry was made."
                        } else {
                            "wes connection unavailable or reply lost. An admitted call may have run. Read execution status with the original request_id before repeating."
                        }),
                    },
                }
            }
        },
    );
    collection.finish(code);
    code
}
/// Read/validate in arrival order, dispatch independently, serialize every reply in one writer.
/// There is no unbounded pending queue. Cancellation has separate capacity from waiting calls.
fn serve_io<R, W, F, Fut>(
    mut reader: R,
    mut writer: W,
    runtime: tokio::runtime::Handle,
    meter: Meter,
    call: F,
) -> u8
where
    R: BufRead,
    W: Write + Send,
    F: Fn(Value) -> Fut + Clone + Send + 'static,
    Fut: Future<Output = CallReply> + Send + 'static,
{
    let known_tools = tools()["tools"]
        .as_array()
        .expect("registry")
        .iter()
        .map(|t| t["name"].as_str().expect("tool name").to_owned())
        .collect();
    let (replies, mut output) = mpsc::channel::<Reply>(CALL_CAPACITY + CANCEL_CAPACITY);
    let calls = Arc::new(Semaphore::new(CALL_CAPACITY));
    let cancellations = Arc::new(Semaphore::new(CANCEL_CAPACITY));
    let tasks = TaskTracker::new();
    std::thread::scope(|scope| {
        let writing = scope.spawn(move || {
            let mut failed = false;
            while let Some(reply) = output.blocking_recv() {
                if failed {
                    reply.ticket.finish(0, Outcome::TransportError);
                    continue;
                }
                let mut counted = CountingWriter {
                    writer: &mut writer,
                    bytes: 0,
                };
                let result = writeln!(counted, "{}", reply.value).and_then(|_| counted.flush());
                failed = result.is_err();
                reply.ticket.finish(
                    counted.bytes,
                    if failed {
                        Outcome::TransportError
                    } else {
                        reply.outcome
                    },
                );
            }
            if failed { Err(()) } else { Ok(()) }
        });
        let mut protocol = Protocol::default();
        let code = loop {
            let mut bytes = Vec::new();
            let Ok(n) = (&mut reader).take(LIMIT + 1).read_until(b'\n', &mut bytes) else {
                meter
                    .receive(None, bytes.len(), &known_tools)
                    .finish(0, Outcome::TransportError);
                break 1;
            };
            if n == 0 {
                break 0;
            }
            if n as u64 > LIMIT {
                meter
                    .receive(None, n, &known_tools)
                    .finish(0, Outcome::InputLimit);
                eprintln!("wes MCP input exceeds 128 KiB.");
                break 2;
            }
            let message = serde_json::from_slice::<Value>(&bytes);
            let ticket = meter.receive(message.as_ref().ok(), n, &known_tools);
            let mut dispatched = false;
            let mut outcome = Outcome::Ok;
            let reply = match message {
                Err(_) => Some(error(Value::Null, -32700, "Invalid JSON.")),
                Ok(message) => protocol.receive(message, |id, tool| {
                    let capacity = if tool["name"] == "cancel" { &cancellations } else { &calls };
                    let Ok(permit) = capacity.clone().try_acquire_owned() else {
                        outcome = Outcome::Rejected;
                        return Some(tool_error("MCP call capacity reached; this call was not admitted. Await an outstanding call before retrying."));
                    };
                    let replies = replies.clone();
                    let call = call.clone();
                    let ticket = ticket.clone();
                    dispatched = true;
                    tasks.spawn_on(async move {
                        let result = call(tool).await;
                        let reply = Reply { value: json!({"jsonrpc":"2.0","id":id,"result":result.value}), ticket, outcome: result.outcome };
                        if let Err(error) = replies.send(reply).await {
                            error.0.ticket.finish(0, Outcome::TransportError);
                        }
                        drop(permit);
                    }, &runtime);
                    None
                }),
            };
            if let Some(value) = reply {
                if value.get("error").is_some() {
                    outcome = Outcome::ProtocolError;
                }
                if let Err(error) = replies.blocking_send(Reply {
                    value,
                    ticket,
                    outcome,
                }) {
                    error.0.ticket.finish(0, Outcome::TransportError);
                    break 1;
                }
            } else if !dispatched {
                ticket.finish(0, Outcome::Notification);
            }
        };
        tasks.close();
        runtime.block_on(tasks.wait());
        drop(replies);
        if writing.join().is_ok_and(|result| result.is_ok()) {
            code
        } else {
            1
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lifecycle_schema_errors_and_notifications_have_no_effects() {
        let mut p = Protocol::default();
        let forbidden = |_, _| panic!("must not dispatch");
        assert_eq!(
            p.receive(
                json!({"jsonrpc":"2.0","id":1,"method":"tools/list"}),
                forbidden
            )
            .unwrap()["error"]["code"],
            -32000
        );
        let init=p.receive(json!({"jsonrpc":"2.0","id":2,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{},"capabilities":{}}}),forbidden).unwrap();
        assert_eq!(init["result"]["serverInfo"]["name"], "wes-workspace");
        assert!(
            p.receive(
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
                forbidden
            )
            .is_none()
        );
        let listed = p
            .receive(
                json!({"jsonrpc":"2.0","id":"list","method":"tools/list"}),
                forbidden,
            )
            .unwrap();
        let advertised = tools();
        assert_eq!(listed["id"], "list");
        assert_eq!(listed["result"], advertised);
        let pane = advertised["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "pane_command")
            .unwrap();
        assert_eq!(pane["inputSchema"]["required"], json!(["command"]));
        assert_eq!(pane["inputSchema"]["additionalProperties"], false);
        assert!(
            init["result"]["instructions"]
                .as_str()
                .unwrap()
                .contains("pane_command")
        );
        assert_eq!(p.receive(json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"execute","arguments":[]}}),forbidden).unwrap()["error"]["code"],-32602);
        assert!(
            p.receive(
                json!({"jsonrpc":"2.0","method":"tools/call","params":{"name":"execute"}}),
                forbidden
            )
            .is_none()
        );
        assert_eq!(
            p.receive(json!([]), forbidden).unwrap()["error"]["code"],
            -32600
        );
    }
}

#[cfg(test)]
mod concurrency_tests {
    use super::*;
    use std::io::Cursor;
    use tokio_util::sync::CancellationToken;

    #[test]
    fn tool_registry_has_unique_names_and_complete_closed_input_schemas() {
        let advertised = tools();
        let tools = advertised["tools"].as_array().unwrap();
        let mut names = std::collections::BTreeSet::new();
        for tool in tools {
            let name = tool["name"].as_str().unwrap();
            assert!(
                !name.is_empty() && names.insert(name),
                "duplicate or blank tool: {name}"
            );
            assert!(!tool["description"].as_str().unwrap().is_empty());
            let schema = &tool["inputSchema"];
            assert_eq!(schema["type"], "object", "{name}");
            assert_eq!(schema["additionalProperties"], false, "{name}");
            let properties = schema["properties"].as_object().unwrap();
            let required = schema["required"].as_array().unwrap();
            let mut seen = std::collections::BTreeSet::new();
            for key in required {
                let key = key.as_str().unwrap();
                assert!(
                    properties.contains_key(key) && seen.insert(key),
                    "invalid required key in {name}: {key}"
                );
            }
        }
    }

    struct Replies {
        bytes: Vec<u8>,
        lines: std::sync::mpsc::Sender<Value>,
    }
    impl Write for Replies {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.bytes.extend_from_slice(bytes);
            while let Some(end) = self.bytes.iter().position(|b| *b == b'\n') {
                let line: Vec<_> = self.bytes.drain(..=end).collect();
                self.lines
                    .send(serde_json::from_slice(&line).unwrap())
                    .unwrap();
            }
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn waiting_calls_allow_independent_calls_ping_and_reserved_cancel_without_unbounded_queue() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        for blocked in [1, CALL_CAPACITY] {
            let gate = CancellationToken::new();
            let _release_on_failure = gate.clone().drop_guard();
            let mut input = vec![
                json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{},"capabilities":{}}}),
                json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            ];
            for index in 0..blocked {
                input.push(json!({"jsonrpc":"2.0","id":10+index,"method":"tools/call","params":{"name":"blocked"}}));
            }
            input.extend([
                json!({"jsonrpc":"2.0","id":98,"method":"tools/call","params":{"name":"independent"}}),
                json!({"jsonrpc":"2.0","id":99,"method":"ping"}),
                json!({"jsonrpc":"2.0","id":100,"method":"tools/call","params":{"name":"cancel"}}),
            ]);
            let input = input
                .iter()
                .map(|value| format!("{value}\n"))
                .collect::<String>();
            let (send, receive) = std::sync::mpsc::channel();
            let runtime_handle = runtime.handle().clone();
            let waiting = gate.clone();
            let meter = Meter::testing();
            let recording = meter.clone();
            let serving = std::thread::spawn(move || {
                serve_io(
                    Cursor::new(input),
                    Replies {
                        bytes: vec![],
                        lines: send,
                    },
                    runtime_handle,
                    recording,
                    move |tool| {
                        let waiting = waiting.clone();
                        async move {
                            if tool["name"] == "blocked" {
                                waiting.cancelled().await;
                            }
                            json!({"name":tool["name"]}).into()
                        }
                    },
                )
            });
            let mut replies = BTreeMap::new();
            for _ in 0..4 {
                let reply = receive
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                replies.insert(reply["id"].as_u64().unwrap(), reply["result"].clone());
            }
            assert_eq!(
                replies.keys().copied().collect::<Vec<_>>(),
                [1, 98, 99, 100]
            );
            assert_eq!(replies[&99], json!({}));
            assert_eq!(replies[&100]["name"], "cancel");
            if blocked == CALL_CAPACITY {
                assert_eq!(replies[&98]["isError"], true);
                assert!(
                    replies[&98]["content"][0]["text"]
                        .as_str()
                        .unwrap()
                        .contains("not admitted")
                );
            } else {
                assert_eq!(replies[&98]["name"], "independent");
            }
            gate.cancel();
            assert_eq!(serving.join().unwrap(), 0);
            for _ in 0..blocked {
                let reply = receive
                    .recv_timeout(std::time::Duration::from_secs(5))
                    .unwrap();
                assert_eq!(reply["result"]["name"], "blocked");
                assert!(
                    replies
                        .insert(reply["id"].as_u64().unwrap(), reply)
                        .is_none()
                );
            }
            assert!(receive.try_recv().is_err());
            let report = meter.snapshot();
            assert_eq!(report["totals"]["messages"], blocked + 5);
            assert_eq!(report["totals"]["completed"], blocked + 5);
            assert_eq!(report["groups"]["tools"]["messages"], blocked + 2);
            assert_eq!(report["buckets"]["tools/cancel"]["outcomes"]["ok"], 1);
            if blocked == CALL_CAPACITY {
                assert_eq!(report["totals"]["outcomes"]["rejected"], 1);
            }
        }
    }

    #[test]
    fn metrics_count_exact_unicode_wire_bytes_without_changing_replies() {
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let requests = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","clientInfo":{},"capabilities":{}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
            json!({"jsonrpc":"2.0","id":"secret-id","method":"tools/call","params":{"name":"execute","arguments":{"source":"özel sır 🦀"}}}),
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"value_read"}}),
            json!({"jsonrpc":"2.0","id":5,"method":"secret-method"}),
        ];
        let input = requests
            .iter()
            .map(|v| format!("{v}\n"))
            .collect::<String>()
            + "{bad-json\n";
        let meter = Meter::testing();
        let mut measured = Vec::new();
        let call = |tool: Value| async move {
            if tool["name"] == "execute" {
                CallReply {
                    value: tool_error("özel hata"),
                    outcome: Outcome::Rejected,
                }
            } else {
                CallReply {
                    value: tool_error("fixture failure"),
                    outcome: Outcome::Failed,
                }
            }
        };
        assert_eq!(
            serve_io(
                Cursor::new(&input),
                &mut measured,
                runtime.handle().clone(),
                meter.clone(),
                call
            ),
            0
        );
        let mut baseline = Vec::new();
        assert_eq!(
            serve_io(
                Cursor::new(&input),
                &mut baseline,
                runtime.handle().clone(),
                Meter::default(),
                call
            ),
            0
        );
        let sorted = |bytes: &[u8]| {
            let mut replies: Vec<String> = std::str::from_utf8(bytes)
                .unwrap()
                .lines()
                .map(str::to_owned)
                .collect();
            replies.sort();
            replies
        };
        assert_eq!(sorted(&measured), sorted(&baseline));
        let report = meter.snapshot();
        assert_eq!(report["totals"]["messages"], 7);
        assert_eq!(report["totals"]["completed"], 7);
        assert_eq!(report["totals"]["input_bytes"], input.len());
        assert_eq!(report["totals"]["output_bytes"], measured.len());
        assert_eq!(report["groups"]["discovery"]["messages"], 2);
        assert_eq!(report["groups"]["tools"]["messages"], 2);
        assert_eq!(
            report["totals"]["outcomes"],
            json!({"ok":2,"rejected":1,"tool_failed":1,"protocol_error":2,"no_reply":1})
        );
        assert!(
            report["totals"]["duration_us"].as_u64().unwrap()
                >= report["totals"]["max_duration_us"].as_u64().unwrap()
        );
        for secret in [
            "secret-id",
            "secret-method",
            "özel",
            "fixture failure",
            "bad-json",
        ] {
            assert!(!report.to_string().contains(secret));
        }
    }

    #[test]
    fn metrics_account_partial_output_and_oversized_input_without_claiming_delivery() {
        struct Broken {
            remaining: usize,
        }
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
                if self.remaining == 0 {
                    return Err(std::io::ErrorKind::BrokenPipe.into());
                }
                let n = bytes.len().min(self.remaining);
                self.remaining -= n;
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                Ok(())
            }
        }
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let meter = Meter::testing();
        let input = "{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\"}\n{bad\n";
        assert_eq!(
            serve_io(
                Cursor::new(input),
                Broken { remaining: 13 },
                runtime.handle().clone(),
                meter.clone(),
                |_| async { panic!("no tool") }
            ),
            1
        );
        let report = meter.snapshot();
        assert_eq!(report["totals"]["output_bytes"], 13);
        assert_eq!(report["totals"]["outcomes"]["transport_error"], 2);
        assert_eq!(report["totals"]["completed"], 2);
        let oversized = vec![b' '; LIMIT as usize + 100];
        let meter = Meter::testing();
        assert_eq!(
            serve_io(
                Cursor::new(oversized),
                Vec::new(),
                runtime.handle().clone(),
                meter.clone(),
                |_| async { panic!("no tool") }
            ),
            2
        );
        let report = meter.snapshot();
        assert_eq!(report["totals"]["input_bytes"], LIMIT + 1);
        assert_eq!(report["totals"]["output_bytes"], 0);
        assert_eq!(report["totals"]["outcomes"]["input_limit"], 1);
    }
    use std::collections::BTreeMap;
}
