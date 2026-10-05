use super::*;
use tokio::io::AsyncWriteExt;
use wes_engine::conversations::{Channel, ConversationIo, Input, InteractiveInvoker};

#[derive(Clone)]
pub struct ProcessConversation {
    invoker: ProcessInvoker,
    inherited: bool,
    terminal: Option<Arc<tokio::sync::Semaphore>>,
}
/// One embedding client's inherited descriptors. Share this owner across shell/imported providers
/// so two conversations cannot read the same terminal or draw on it concurrently.
#[derive(Clone)]
pub struct TerminalHandover(Arc<tokio::sync::Semaphore>);
impl Default for TerminalHandover {
    fn default() -> Self {
        Self::new()
    }
}
impl TerminalHandover {
    pub fn new() -> Self {
        Self(Arc::new(tokio::sync::Semaphore::new(1)))
    }
    pub fn conversation(&self, invoker: &ProcessInvoker) -> ProcessConversation {
        ProcessConversation {
            invoker: invoker.clone(),
            inherited: true,
            terminal: Some(self.0.clone()),
        }
    }
}
impl ProcessInvoker {
    /// A browser/client conversation over pipes; it is not a pseudo-terminal.
    pub fn piped(&self) -> ProcessConversation {
        ProcessConversation {
            invoker: self.clone(),
            inherited: false,
            terminal: None,
        }
    }
    /// Explicit native-terminal handover. The embedding client must own and suspend its terminal.
    pub fn handed_over(&self) -> ProcessConversation {
        ProcessConversation {
            invoker: self.clone(),
            inherited: true,
            terminal: None,
        }
    }
}
impl InteractiveInvoker for ProcessConversation {
    fn start(
        &self,
        call: Call,
        mut io: ConversationIo,
        cancellation: CancellationToken,
    ) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let (line, timeout) = this.invoker.prepare(&call).map_err(Failure::error)?;
            // Waiting for a person has no default finite invocation deadline. An explicit process
            // timeout remains meaningful, independently of any runtime-level annotated budget.
            let deadline = if call.arguments.contains_key("timeout") {
                Some(
                    Instant::now()
                        .checked_add(timeout)
                        .ok_or_else(|| Failure::Arguments.error())?,
                )
            } else {
                None
            };
            if this.inherited {
                io.close_input();
            }
            let _terminal_lease = match this.terminal {
                Some(terminal) => Some(
                    acquire_terminal(terminal, deadline, &cancellation)
                        .await
                        .map_err(Failure::error)?,
                ),
                None => None,
            };
            let spawn_line = line.clone();
            let spawn_cancel = cancellation.clone();
            let inherited = this.inherited;
            let launch = this.invoker.launch.clone();
            let spawned = tokio::task::spawn_blocking(move || {
                super::serialized_spawn(|| {
                    if spawn_cancel.is_cancelled() {
                        return Err(Failure::Cancelled);
                    }
                    if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                        return Err(Failure::Timeout);
                    }
                    let mut command = Command::new(&spawn_line[0]);
                    command.args(&spawn_line[1..]).kill_on_drop(true);
                    if let Some(launch) = launch {
                        launch.apply(&mut command)?;
                    }
                    if inherited {
                        command
                            .stdin(Stdio::inherit())
                            .stdout(Stdio::inherit())
                            .stderr(Stdio::inherit());
                    } else {
                        command
                            .stdin(Stdio::piped())
                            .stdout(Stdio::piped())
                            .stderr(Stdio::piped());
                    }
                    command.spawn().map_err(|_| Failure::Spawn)
                })
            })
            .await
            .map_err(|_| Failure::Internal.error())?;
            let mut child = spawned.map_err(Failure::error)?;
            let opened = io.output.opened();
            let result = if opened.is_err() {
                Err(Failure::Cancelled)
            } else {
                converse(
                    &mut child,
                    &mut io,
                    deadline,
                    this.invoker.config.output_bytes,
                    &cancellation,
                )
                .await
            };
            match result {
                Ok((status, stdout, stderr)) => {
                    let mut origin = Provenance::default().with_fact(
                        "ranLocally",
                        if this.invoker.launch.is_some() {
                            "managed local process".into()
                        } else {
                            line.join(" ")
                        },
                    );
                    if inherited {
                        origin = origin
                            .with_fact("ranInteractively", "output went to the terminal, not here");
                    }
                    Value::new(
                        output_shape(),
                        Data::Record(IndexMap::from_iter([
                            ("exitCode".into(), Data::Int(exit_code(status))),
                            ("stdout".into(), Data::Bytes(stdout.into())),
                            ("stderr".into(), Data::Bytes(stderr.into())),
                        ])),
                        origin,
                    )
                    .map_err(|_| Failure::Internal.error())
                }
                Err(reason) => {
                    let _signalled = child.start_kill();
                    if child.wait().await.is_err() {
                        return Err(Failure::Cleanup.error());
                    }
                    if cancellation.is_cancelled() {
                        Err(InvocationError::Cancelled)
                    } else {
                        Err(reason.error())
                    }
                }
            }
        })
    }
}
async fn acquire_terminal(
    terminal: Arc<tokio::sync::Semaphore>,
    deadline: Option<Instant>,
    cancellation: &CancellationToken,
) -> Result<tokio::sync::OwnedSemaphorePermit, Failure> {
    let acquire = terminal.acquire_owned();
    tokio::pin!(acquire);
    loop {
        let wake = deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400))
            .min(Instant::now() + Duration::from_secs(86_400));
        tokio::select! {
            biased;
            _ = cancellation.cancelled() => return Err(Failure::Cancelled),
            _ = tokio::time::sleep_until(wake) => if deadline.is_some_and(|deadline| Instant::now() >= deadline) { return Err(Failure::Timeout); },
            permit = &mut acquire => return permit.map_err(|_| Failure::Internal),
        }
    }
}
async fn converse(
    child: &mut Child,
    io: &mut ConversationIo,
    deadline: Option<Instant>,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Failure> {
    let mut stdin = child.stdin.take();
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut pending: Option<(Vec<u8>, usize)> = None;
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    loop {
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
        {
            return Ok((status, output, errors));
        }
        let wake = deadline
            .unwrap_or_else(|| Instant::now() + Duration::from_secs(86_400))
            .min(Instant::now() + Duration::from_secs(86_400));
        // A partial stdin write remains owned while stdout and stderr drain. Awaiting write_all
        // inline here would deadlock a child that fills stdout before reading a large answer.
        tokio::select! {
            _ = cancellation.cancelled() => return Err(Failure::Cancelled),
            _ = tokio::time::sleep_until(wake) => if deadline.is_some_and(|deadline| Instant::now() >= deadline) { return Err(Failure::Timeout); },
            waited = child.wait(), if status.is_none() => {
                status = Some(waited.map_err(|_| Failure::Wait)?);
                stdin = None;
                pending = None;
                io.close_input();
            },
            read = read_pipe(&mut stdout, &mut out_buf) => {
                let n = read.map_err(|_| Failure::Read)?;
                if n == 0 { stdout = None; } else {
                    append(&mut output, errors.len(), &out_buf[..n], limit)?;
                    io.output.write(Channel::Stdout, &out_buf[..n]).map_err(|_| Failure::Cancelled)?;
                }
            },
            read = read_pipe(&mut stderr, &mut err_buf) => {
                let n = read.map_err(|_| Failure::Read)?;
                if n == 0 { stderr = None; } else {
                    append(&mut errors, output.len(), &err_buf[..n], limit)?;
                    io.output.write(Channel::Stderr, &err_buf[..n]).map_err(|_| Failure::Cancelled)?;
                }
            },
            input = io.receive(), if stdin.is_some() && pending.is_none() => match input {
                Some(Input::Bytes(bytes)) if !bytes.is_empty() => pending = Some((bytes, 0)),
                Some(Input::Bytes(_)) => {},
                Some(Input::Eof) | None => { stdin = None; io.close_input(); },
            },
            written = write_input(&mut stdin, &pending), if pending.is_some() => {
                // Broken stdin does not discard a valid exit result or remaining output. Revoke
                // further input; queue acceptance was never a child-consumption acknowledgement.
                match written {
                    Ok(n) if n > 0 => {
                        let (bytes, offset) = pending.as_mut().expect("pending input");
                        *offset += n;
                        if *offset == bytes.len() { pending = None; }
                    },
                    _ => { stdin = None; pending = None; io.close_input(); },
                }
            },
        }
    }
}
async fn write_input(
    stdin: &mut Option<tokio::process::ChildStdin>,
    pending: &Option<(Vec<u8>, usize)>,
) -> io::Result<usize> {
    match (stdin, pending) {
        (Some(stdin), Some((bytes, offset))) => stdin.write(&bytes[*offset..]).await,
        _ => std::future::pending().await,
    }
}
