use std::{
    io::{self, Write},
    process::ExitCode,
    sync::Arc,
};
use wes::{
    ApplicationHandle,
    runtime::{LaunchedRuntime, RuntimeOptions},
};
use wes_adapters::codec::{Limits, encode_json, encode_json_pretty};
use wes_engine::source::SourceInput;
type Error = Box<dyn std::error::Error + Send + Sync>;
mod arguments;
mod help_text;
mod sequential;
use arguments::{Arguments, USAGE, arguments};
#[tokio::main]
async fn main() -> ExitCode {
    if let Some(code) = wes::terminal::assistant::entry().await {
        return ExitCode::from(code);
    }
    if let Some(code) = wes::terminal::client().await {
        return ExitCode::from(code);
    }
    if let Some(code) = wes::view_toolchain::entry() {
        return ExitCode::from(code);
    }
    // The executable captures its operating policy before argument defaults
    // or runtime admission. Embedders remain explicit; changing a data home
    // must not replace this process-start policy.
    let initialized = std::env::home_dir()
        .ok_or_else(|| io::Error::other("User home is unavailable for operating budgets."))
        .and_then(|home| wes::budgets::initialize(&home));
    if let Err(error) = initialized {
        eprintln!("wes: {error}");
        return ExitCode::FAILURE;
    }
    let args = match arguments() {
        Ok(Some(args)) => args,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(error) => {
            eprintln!("wes: {error}\nUse wes --help for usage.");
            return ExitCode::from(2);
        }
    };
    match run(args).await {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("wes: {error}");
            for line in rejection(error.as_ref()) {
                eprintln!("  {line}");
            }
            ExitCode::FAILURE
        }
    }
}
/// The one place this program learns that its user asked it to stop. A server, a scenario and
/// a command all end through it, in their own orderly way.
///
/// Windows has two such requests. Ctrl+C reaches every process on a console and cannot be
/// addressed to one of them; Ctrl+Break can be sent to a single process group, which is how
/// a parent that started this program in its own group asks it, and only it, to stop.
fn interrupt() -> io::Result<impl std::future::Future<Output = io::Result<()>>> {
    #[cfg(windows)]
    {
        let mut ctrl_break = tokio::signal::windows::ctrl_break()?;
        let mut ctrl_c = tokio::signal::windows::ctrl_c()?;
        Ok(async move {
            let received = tokio::select! {
                received = ctrl_c.recv() => received,
                received = ctrl_break.recv() => received,
            };
            received.ok_or_else(|| io::Error::other("the console's stop requests are unavailable"))
        })
    }
    #[cfg(unix)]
    {
        let mut signal = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?;
        Ok(async move {
            signal
                .recv()
                .await
                .ok_or_else(|| io::Error::other("stop requests are unavailable"))
        })
    }
    #[cfg(not(any(unix, windows)))]
    Ok(tokio::signal::ctrl_c())
}

/// A rejection outside a submitted source (startup, definitions) has no cell to carry its
/// diagnostics. Each is reported by its code and its payload-independent summary: a detailed
/// message can hold inferred private types or provider validation data, and this stream is
/// not the authorized source-validation path.
fn rejection(error: &(dyn std::error::Error + 'static)) -> Vec<String> {
    use wes_engine::workspace::WorkspaceError;
    let workspace = match error.downcast_ref::<wes::ApplicationError>() {
        Some(wes::ApplicationError::Workspace(workspace)) => Some(workspace),
        _ => error.downcast_ref::<WorkspaceError>(),
    };
    match workspace {
        Some(WorkspaceError::Rejected { diagnostics, .. }) => diagnostics
            .iter()
            .map(|diagnostic| format!("{}: {}", diagnostic.code, diagnostic.public_summary()))
            .collect(),
        _ => vec![],
    }
}
async fn run(mut args: Arguments) -> Result<bool, Error> {
    let launch_base = std::env::current_dir()?;
    if let Some(path) = args.api_request.take() {
        let home = launch_base.join(&args.home);
        let response = tokio::task::spawn_blocking(move || -> Result<_, Error> {
            use std::io::Read;
            let mut bytes = Vec::new();
            let reader: Box<dyn Read> = if path == std::path::Path::new("-") {
                Box::new(io::stdin())
            } else {
                Box::new(std::fs::File::open(path)?)
            };
            reader.take(128 * 1024 + 1).read_to_end(&mut bytes)?;
            let request = wes::api_library::parse_request(&bytes)?;
            Ok(wes::api_library::ApiLibrary::new(home).perform(request)?)
        })
        .await??;
        println!("{}", serde_json::to_string_pretty(&response)?);
        return Ok(true);
    }
    let scenario_file = if let Some(path) = args.test.take() {
        let base = launch_base.clone();
        Some(
            tokio::task::spawn_blocking(move || -> Result<_, Error> {
                let file = wes_adapters::script_files::ScriptFile::read(&base, &path)?;
                let scenario = wes::scenarios::Scenario::parse(&file.text)?;
                Ok((file.directory, scenario))
            })
            .await??,
        )
    } else {
        None
    };
    let captured = if let Some(path) = args.file.take() {
        let base = launch_base.clone();
        Some(
            tokio::task::spawn_blocking(move || {
                wes_adapters::script_files::ScriptFile::read(&base, &path)
            })
            .await??,
        )
    } else {
        None
    };
    let base = captured.as_ref().map_or_else(
        || {
            scenario_file
                .as_ref()
                .map_or_else(|| launch_base.clone(), |(directory, _)| directory.clone())
        },
        |file| file.directory.clone(),
    );
    let diagnostic_source = captured
        .as_ref()
        .map(|file| wes_language::SourceText::new(file.path.to_string_lossy(), file.text.clone()));
    let command = captured.map(|file| file.text).or(args.command.take());
    if args.credentials_stdin
        && command
            .as_ref()
            .is_some_and(|text| text.contains("@interactive"))
    {
        return Err(io::Error::other(
            "credential stdin cannot share a script with interactive input",
        )
        .into());
    }
    // Runner flags keep normal shell cwd semantics; only source-level reads use the script base.
    args.environment_file = args
        .environment_file
        .map(|path| {
            launch_base
                .join(path)
                .into_os_string()
                .into_string()
                .map_err(|_| io::Error::other("definition path must be UTF-8"))
        })
        .transpose()?;
    // Validate source size/identity before opening any persistent directory.
    let mut source = command
        .map(|command| SourceInput::new(uuid::Uuid::new_v4().to_string(), command))
        .transpose()?;
    if let Some(file) = diagnostic_source.as_ref() {
        source = source
            .map(|input| input.with_source_name(file.name().to_owned()))
            .transpose()?;
    }
    let steps = source
        .as_ref()
        .filter(|_| args.sequential)
        .map(sequential::split)
        .transpose()?;
    let mut scenario_context = None;
    let runtime = wes::runtime::launch(RuntimeOptions {
        home: args.home.clone(),
        base,
        workspace: args.workspace.clone(),
        concurrency: args.concurrency,
        max_streams: args.max_streams,
        live_budget: args.live_budget,
        auto_keep: args.auto_keep,
        node_timeout: args.node_timeout,
        interactive_terminal: source.is_some(),
        docker_candidates: None,
        credential_store: None,
        credential_vault: None,
    })
    .await?;
    let _telemetry = wes::telemetry::install(runtime.home().join("diagnostics"));
    let handle = runtime.handle.clone();
    let outcome: Result<bool, Error> = async {
        announce_startup(&handle, source.is_some() || scenario_file.is_some()).await?;
        if let Some(path) = args.environment_lock {
            let records = tokio::task::spawn_blocking(move || -> Result<_, Error> { Ok(wes_adapters::environments::LocalEnvironments::new(std::env::current_dir()?)?.read_lock(&path)?) }).await??;
            let session = handle.current()?.session;
            let observation = session.observe().await?;
            if observation.environment_revisions.keys().any(|name| Some(name) != observation.default_environment.as_ref()) { return Err(io::Error::other("portable lock installation requires an empty user environment registry").into()); }
            for record in records {
                let plan = session.plan_environments(record.yaml().into(), record.sources().clone()).await?;
                if plan.revisions() != record.after() { return Err(io::Error::other("lock revision mismatch").into()); }
                session.apply_environments(plan).await?;
            }
        }
        if let Some(path) = args.environment_file {
            let session = handle.current()?.session;
            let plan = session.plan_environment_file(path, false).await?;
            for change in plan.changes() { eprintln!("[environment plan] {change}"); }
            session.apply_environments(plan).await?;
        }
        if let Some(name) = args.environment {
            let session = handle.current()?.session;
            let revisions = session.environment_revisions().await?;
            let revision = *revisions.get(&name).ok_or_else(|| io::Error::other("ENV008: selected environment unavailable"))?;
            if args.environment_revision.is_some_and(|expected| expected != revision) { return Err(io::Error::other("ENV008: environment revision conflict; no command dispatched").into()); }
            if args.activate_environment {
                // The name comes from the validated registry, not raw command interpolation.
                session.submit(SourceInput::new(uuid::Uuid::new_v4().to_string(), format!(":env enable \"{name}\""))?).await?;
            }
            if let Some(provider) = args.grant_provider {
                session.environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Grant { environment: name.clone(), revision, provider, seconds: 300 }).await?;
            }
            let context = wes_core::environments::EnvironmentContext { selected: Some(name), revisions };
            scenario_context = Some(context.clone());
            source = source.map(|input| input.with_environments(context)).transpose()?;
        }
        if args.credentials_stdin {
            let supplied = tokio::task::spawn_blocking(|| -> Result<std::collections::BTreeMap<String, String>, Error> {
                use std::io::Read;
                let mut bytes = vec![];
                io::stdin().lock().take(1024 * 1024 + 1).read_to_end(&mut bytes)?;
                if bytes.len() > 1024 * 1024 { return Err(io::Error::other("credential input exceeds 1 MiB").into()); }
                let material: std::collections::BTreeMap<String, String> = serde_json::from_slice(&bytes).map_err(|_| io::Error::other("invalid credential JSON map"))?;
                if material.len() > 128 { return Err(io::Error::other("credential input exceeds 128 references").into()); }
                Ok(material)
            }).await??;
            let session = handle.current()?.session;
            for (reference, value) in supplied {
                session.environment_authority(wes_engine::session::EnvironmentAuthorityCommand::Supply { reference, value: Arc::new(wes_engine::credentials::SecretString::from(value)) }).await?;
            }
        }
        if let Some((_, scenario)) = scenario_file {
            let session = handle.current()?.session;
            let cancel = wes_engine::driver::CancellationToken::new();
            let run = wes::scenarios::run(session, &scenario, scenario_context, cancel.clone());
            tokio::pin!(run);
            let report = tokio::select! {
                result = &mut run => result?,
                signal = interrupt()? => {
                    signal?;
                    cancel.cancel();
                    run.await?
                }
            };
            if let Some(json) = report.export() { println!("{}", serde_json::to_string_pretty(&json)?); }
            else { eprintln!("Test report withheld: private or unavailable input provenance; exit code still indicates pass/fail."); }
            return Ok(report.passed());
        }
        match source {
            Some(source) => tokio::select! {
                outcome = execute_source(&handle, source, steps, diagnostic_source.as_ref(), args.json) => outcome,
                signal = interrupt()? => signal.map(|()| false).map_err(Error::from),
            },
            None => {
                let server = runtime
                    .serve(args.serve.expect("validated server mode"), args.site)
                    .await?;
                // The startup line is readiness: the stop handler must already be registered.
                let stopping = interrupt()?;
                tokio::pin!(stopping);
                println!("Listening at http://{}", server.address());
                let signal = tokio::select! { signal = &mut stopping => signal, _ = server.stopped() => Err(io::Error::other("HTTP server stopped")) };
                let joined = server.shutdown().await;
                joined?;
                signal?;
                Ok(true)
            }
        }
    }
    .await;
    handle.shutdown().await;
    let LaunchedRuntime {
        task,
        worker,
        worker_task,
        ..
    } = runtime;
    let joined = task.join().await;
    let outcome = match outcome {
        Err(error) => Err(error),
        Ok(success) => joined.map(|()| success).map_err(Error::from),
    };
    // Shutdown always attempts and joins every owner, including failed serving or Ctrl-C.
    let drained = worker.shutdown().await;
    let worker_joined = worker_task.join().await;
    let success = outcome?;
    if drained?.failed != 0 {
        return Err(io::Error::other("value storage reported failed operations").into());
    }
    worker_joined?;
    Ok(success)
}
fn bounded_diagnostic(text: &str, limit: usize) -> String {
    let mut chars = text.chars();
    let mut result: String = chars.by_ref().take(limit).collect();
    if chars.next().is_some() {
        result.push('…');
    }
    result
}

async fn announce_startup(handle: &ApplicationHandle, command: bool) -> Result<(), Error> {
    if let Some(report) = &handle.current()?.session.snapshot().await?.restoration {
        let warnings: Vec<_> = if command {
            wes::startup::command_warnings(report).collect()
        } else {
            wes::startup::warnings(report)
        };
        for warning in warnings {
            eprintln!(
                "[startup] {}{}",
                warning
                    .node
                    .as_ref()
                    .map_or(String::new(), |node| format!("{}: ", node.as_str())),
                warning.message
            );
        }
    }
    Ok(())
}
async fn execute(
    handle: &ApplicationHandle,
    source: SourceInput,
    file: Option<&wes_language::SourceText>,
    json: bool,
) -> Result<bool, Error> {
    // Only needed if a successful management command removes its issuing session.
    let submitted = source.clone();
    let previous = handle.current()?;
    let before = previous.session.snapshot().await?.execution.runs;
    let reply = handle.submit(source).await.map_err(|error| match file {
        Some(file) => Error::from(io::Error::other(format!("{}: {error}", file.name()))),
        None => Error::from(error),
    })?;
    let mut success = print_submission(&reply, file, json)?;
    let current = match handle.current() {
        Ok(current) => current,
        Err(wes::ApplicationError::Stopped)
            if success
                && reply.nodes.is_empty()
                && is_workspace_delete(&submitted)
                && !handle
                    .subscribe_workspace_names()
                    .borrow()
                    .iter()
                    .any(|name| name == &previous.name) =>
        {
            if json {
                println!(
                    "{}",
                    serde_json::json!({"operation":"workspace delete", "state":"completed"})
                );
            }
            return Ok(true);
        }
        Err(error) => return Err(error.into()),
    };
    if current.generation != previous.generation {
        announce_startup(handle, true).await?;
    }
    current.session.wait_idle().await?;
    let snapshot = current.session.snapshot().await?;
    let cells = if snapshot.execution.errors.is_empty() {
        vec![]
    } else {
        current.session.observe().await?.cells
    };
    let sandbox = if let Some(sandbox) = &reply.sandbox {
        let observation = if let Some((reference, inspect)) = &sandbox.view {
            Some(
                current
                    .session
                    .read_sandbox_export(reference, *inspect)
                    .await,
            )
        } else if matches!(&sandbox.data, wes_core::Data::Record(fields) if fields.get("state") == Some(&wes_core::Data::Text("accepted".into())))
        {
            Some(
                current
                    .session
                    .read_sandbox_export(&sandbox.name, false)
                    .await,
            )
        } else {
            None
        };
        match observation {
            Some(Ok(observation)) => Some(observation),
            Some(Err(wes_engine::session::SessionError::Authority)) => {
                withheld_notice(json);
                None
            }
            Some(Err(error)) => return Err(error.into()),
            None => Some(sandbox.clone()),
        }
    } else {
        None
    };
    let stdout = io::stdout();
    let mut output = stdout.lock();
    if let Some(sandbox) = sandbox {
        write!(output, "{}: ", sandbox.name)?;
        output.write_all(&display_json(&sandbox.data, !json, !json)?)?;
        writeln!(output)?;
    }
    let mut nodes = reply.nodes.clone();
    let mut seen: std::collections::HashSet<_> = nodes.iter().cloned().collect();
    if current.generation == previous.generation {
        for (node, run) in &snapshot.execution.runs {
            if before.get(node) != Some(run) && seen.insert(node.clone()) {
                nodes.push(node.clone());
            }
        }
    }
    for node in &nodes {
        if let Some(error) = snapshot.execution.errors.get(node) {
            if let Some(file) = file {
                eprint!("{}: ", file.name());
            }
            eprintln!("{}: {}: {}", node.as_str(), error.code(), error.message());
            for issue in error.issues().iter().take(12) {
                eprintln!(
                    "  {}: {}: {}",
                    bounded_diagnostic(&issue.path, 256),
                    issue.code,
                    bounded_diagnostic(&issue.message, 1024)
                );
            }
            if error.issues().len() > 12 {
                eprintln!("  … {} more validation issues", error.issues().len() - 12);
            }
            let locations = error.locations();
            let mut index = 0;
            let mut groups = 0;
            while index < locations.len() && groups < 12 {
                let location = &locations[index];
                eprintln!(
                    "  {}{}: line {}, column {}",
                    if index == 0 {
                        "at "
                    } else if locations[index - 1].source == location.source {
                        "in "
                    } else {
                        "called from "
                    },
                    source_label(&location.source, node, &reply.cell, &cells, &snapshot),
                    location.line,
                    location.column
                );
                let repeated = locations[index + 1..]
                    .iter()
                    .take_while(|next| *next == location)
                    .count();
                if repeated > 0 {
                    eprintln!("  … {repeated} more identical frames");
                }
                index += 1 + repeated;
                groups += 1;
            }
            if index < locations.len() {
                eprintln!("  … {} more call frames", locations.len() - index);
            }
            success = false;
        } else if let Some(waits) = snapshot.execution.waiting_inputs.get(node) {
            for wait in waits {
                let name = snapshot
                    .names
                    .iter()
                    .find(|(_, output)| {
                        output.node == wait.source
                            && output.port == wes_engine::graph::OutputPort::Data
                    })
                    .map(|(name, _)| name.as_str());
                let message = wait.message_with_name(name);
                if json {
                    eprintln!(
                        "{}",
                        serde_json::json!({"node":node.as_str(),"waiting":{"source":wait.source.as_str(),"port":wait.port_name(),"state":if wait.closed {"closed"} else {"pending"},"run":wait.run.as_ref().map(ToString::to_string),"name":name,"message":message}})
                    );
                } else {
                    eprintln!("{}: {}", node.as_str(), message);
                }
            }
        } else if let Some(value) = snapshot.execution.values.get(node) {
            if value.provenance().policy().is_private() {
                withheld_notice(json);
                continue;
            }
            if value.provenance().fact("snapshot.kind") == Some("names") {
                if let Some(at) = value.provenance().fact("snapshot.capturedAt") {
                    writeln!(
                        output,
                        "Snapshot · captured at {at} · states and types are from that instant"
                    )?;
                    writeln!(
                        output,
                        "Refresh with :refresh ${}; pending/running results may not have a determined type yet.",
                        node.as_str()
                    )?;
                }
            }
            if !json && let Some(help) = help_text::render(value)? {
                writeln!(output, "{}:\n{help}", node.as_str())?;
                continue;
            }
            if !json && contains_bytes(value.data()) {
                eprintln!(
                    "[value] Bytes are base64-encoded in this JSON output; use text(bytes) to decode valid UTF-8 explicitly."
                );
            }
            let bytes = display_json(
                value.data(),
                !json
                    && matches!(
                        value.shape(),
                        wes_core::Shape::Meta(
                            wes_core::MetaType::WorkspaceDeletePlan
                                | wes_core::MetaType::ImportPlan
                        )
                    ),
                !json,
            )?;
            write!(output, "{}: ", node.as_str())?;
            output.write_all(&bytes)?;
            writeln!(output)?;
        }
    }
    output.flush()?;
    Ok(success)
}

fn source_label(
    source: &str,
    node: &wes_engine::graph::NodeId,
    current_cell: &str,
    cells: &[wes_engine::session::ObservedCell],
    snapshot: &wes_engine::session::SessionSnapshot,
) -> String {
    let Some(cell) = source.strip_prefix("cell ") else {
        return bounded_diagnostic(source, 256);
    };
    let nodes = cells
        .iter()
        .find(|item| item.input.cell() == cell)
        .and_then(|item| item.reply.as_ref())
        .and_then(|reply| reply.as_ref().ok())
        .map(|reply| &reply.nodes);
    if cell != current_cell && nodes.is_none() && uuid::Uuid::parse_str(cell).is_err() {
        return bounded_diagnostic(source, 256);
    }
    let name = snapshot
        .names
        .iter()
        .find(|(_, output)| {
            output.port == wes_engine::graph::OutputPort::Data
                && (if cell == current_cell {
                    output.node == *node
                } else {
                    nodes.is_some_and(|nodes| nodes.contains(&output.node))
                })
        })
        .map(|(name, _)| format!("${}", bounded_diagnostic(name, 128)));
    name.unwrap_or_else(|| {
        if cell == current_cell {
            "this cell".into()
        } else {
            "another cell".into()
        }
    })
}

fn contains_bytes(data: &wes_core::Data) -> bool {
    // Result trees already passed bounded model/codec validation. Never consume lazy Iter.
    match data {
        wes_core::Data::Bytes(_) => true,
        wes_core::Data::List(items) => items.iter().any(contains_bytes),
        wes_core::Data::Record(fields) => fields.values().any(contains_bytes),
        wes_core::Data::Option(Some(value)) => contains_bytes(value),
        _ => false,
    }
}

fn display_json(data: &wes_core::Data, pretty: bool, human: bool) -> Result<Vec<u8>, Error> {
    if human {
        Ok(wes_adapters::codec::encode_json_human(
            data,
            Limits::default(),
            pretty,
        )?)
    } else if pretty {
        Ok(encode_json_pretty(data, Limits::default())?)
    } else {
        Ok(encode_json(data, Limits::default())?)
    }
}

async fn execute_source(
    handle: &ApplicationHandle,
    source: SourceInput,
    steps: Option<Vec<wes_language::Span>>,
    file: Option<&wes_language::SourceText>,
    json: bool,
) -> Result<bool, Error> {
    let Some(steps) = steps else {
        return execute(handle, source, file, json).await;
    };
    let origin = wes_language::SourceText::new(source.source_name(), source.text());
    for (index, span) in steps.iter().copied().enumerate() {
        let text = sequential::statement(&source, span);
        let start = origin.position(span.start())?;
        let step_source =
            wes_language::SourceText::new(source.source_name(), text).with_start(start)?;
        let mut input = SourceInput::new(uuid::Uuid::new_v4().to_string(), text.into())?
            .with_client(source.client().into())?
            .with_source_name(source.source_name().into())?
            .with_source_start(start)?;
        if let Some(context) = source.environments() {
            input = input.with_environments(context.clone())?;
        }
        if !execute(handle, input, file.map(|_| &step_source), json).await? {
            return Ok(false);
        }
        if handle.current().is_err() && index + 1 < steps.len() {
            return Err(io::Error::other(format!(
                "Active workspace deleted; {} later workflow {} not run.",
                steps.len() - index - 1,
                if steps.len() - index - 1 == 1 {
                    "step was"
                } else {
                    "steps were"
                }
            ))
            .into());
        }
    }
    Ok(true)
}

fn is_workspace_delete(source: &SourceInput) -> bool {
    let parsed = wes_language::parse(&wes_language::SourceText::new(
        source.source_name(),
        source.text(),
    ));
    parsed.script.statements.len() == 1
        && matches!(&parsed.script.statements[0].expression,
        wes_language::Expression::Call(call) if wes_language::vocabulary::commands::invocation(call).is_ok_and(|invocation| invocation.spec.command == wes_language::vocabulary::MetaCommand::WorkspaceDelete))
}

fn withheld_notice(json: bool) {
    let message =
        "Private value withheld from stdout/export; inspect in the memory-only browser data plane.";
    if json {
        eprintln!(
            "{}",
            serde_json::json!({"severity":"info","code":"PRIVATE_VALUE_WITHHELD","message":message})
        );
    } else {
        eprintln!("{message}");
    }
}

fn print_submission(
    reply: &wes_engine::session::SubmissionResult,
    file: Option<&wes_language::SourceText>,
    json: bool,
) -> Result<bool, Error> {
    let mut success = true;
    for diagnostic in &reply.diagnostics.diagnostics {
        let mut diagnostic = diagnostic.clone();
        if diagnostic.code == "ENG005" {
            diagnostic.hints.push("For an ordered CLI workflow, use --sequential. Each step finishes before the next starts; execution stops on failure.".into());
        }
        let position = file.and_then(|file| file.position(diagnostic.span.start()).ok());
        if json {
            eprintln!(
                "{}",
                serde_json::json!({
                    "severity": match diagnostic.severity { wes_language::Severity::Info => "info", wes_language::Severity::Warning => "warning", wes_language::Severity::Error => "error" },
                    "code":diagnostic.code, "message":diagnostic.message, "hints":diagnostic.hints,
                    "source": file.map(wes_language::SourceText::name), "line": position.map(|p|p.line), "column":position.map(|p|p.column)
                })
            );
        } else if diagnostic.severity == wes_language::Severity::Info {
            println!("{}", diagnostic.message);
        } else {
            if let Some(file) = file {
                if let Some(position) = position {
                    eprintln!(
                        "{}:{}:{}: {diagnostic}",
                        file.name(),
                        position.line,
                        position.column
                    );
                } else {
                    eprintln!("{}: {diagnostic}", file.name());
                }
            } else {
                eprintln!("{diagnostic}");
            }
            for hint in &diagnostic.hints {
                eprintln!("  Hint: {hint}");
            }
        }
        success &= diagnostic.severity != wes_language::Severity::Error;
    }
    let mut output = io::stdout().lock();
    for receipt in &reply.receipts {
        if json {
            writeln!(
                output,
                "{}",
                serde_json::json!({"operation":receipt.operation,"target":receipt.target,"node":receipt.node.as_ref().map(|node|node.as_str()),"requested":receipt.requested,"started":receipt.started,"stale":receipt.stale,"skipped":receipt.skipped,"removed":receipt.removed,"unbound":receipt.unbound,"detail":receipt.detail,"summary":receipt.summary()})
            )?;
        } else {
            writeln!(output, "{}", receipt.summary())?;
        }
    }
    output.flush()?;
    Ok(success)
}

#[cfg(test)]
mod tests {
    use super::*;
    use wes_engine::workspace::WorkspaceError;
    use wes_language::{Diagnostic, Span};

    #[test]
    fn a_startup_rejection_reports_codes_and_public_summaries_never_detailed_messages() {
        const MARKER: &str = "SENSITIVE-MARKER-7f3a";
        let rejected = || WorkspaceError::Rejected {
            diagnostics: vec![
                Diagnostic::error(
                    "ENV010",
                    Span::at(0),
                    format!("provider validation saw {MARKER} in a private contract"),
                )
                .with_public_message("The default environment could not be prepared."),
                Diagnostic::error("TYP004", Span::at(0), format!("inferred type {MARKER}")),
                Diagnostic::error("ZZZ999", Span::at(0), MARKER),
            ],
            issues: vec![],
        };
        // The application boundary and the bare engine error are both recognised.
        let wrapped: Error = wes::ApplicationError::Workspace(rejected()).into();
        let bare: Error = rejected().into();
        for error in [wrapped, bare] {
            let lines = rejection(error.as_ref());
            assert_eq!(
                lines[0],
                "ENV010: The default environment could not be prepared."
            );
            assert!(lines[1].starts_with("TYP004: Type or contract validation failed"));
            assert!(lines[2].starts_with("ZZZ999: Operation was rejected"));
            assert_eq!(lines.len(), 3);
            assert!(lines.iter().all(|line| !line.contains(MARKER)), "{lines:?}");
        }
        let unrelated: Error = std::io::Error::other(MARKER).into();
        assert!(rejection(unrelated.as_ref()).is_empty());
    }
}
