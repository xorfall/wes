//! Local execution ownership. Finite invocation, piped interaction and terminal handover are distinct.
mod interactive;
mod launch;
mod ownership;
use indexmap::IndexMap;
pub use interactive::{ProcessConversation, TerminalHandover};
pub(crate) use launch::local_directory;
pub use launch::{serialized_spawn, serialized_spawn_async};
use std::{
    fmt, io,
    process::{ExitStatus, Stdio},
    sync::Arc,
    time::Duration,
};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt},
    process::{Child, Command},
    time::Instant,
};
use uuid::Uuid;
use wes_core::{
    Data, ErrorId, ErrorValue, Primitive, Provenance, RecordShape, Shape, Value,
    capability::{Capability, Parameter, ProviderDescription, Safety},
};
use wes_engine::{
    driver::CancellationToken,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
};

#[derive(Clone, Copy, Debug)]
pub struct ProcessConfig {
    pub timeout: Duration,
    /// Combined captured stdout and stderr bytes, not allocator or OS pipe buffers.
    pub output_bytes: usize,
    /// Fixed command and optional appended argument, including separators used by provenance.
    pub argument_bytes: usize,
}
impl Default for ProcessConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_millis(wes_budgets::get("process.timeout.ms")),
            output_bytes: wes_budgets::get("process.output.bytes") as usize,
            argument_bytes: wes_budgets::get("process.argument.bytes") as usize,
        }
    }
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("invalid process configuration: {0}")]
pub struct ProcessConfigError(&'static str);

pub fn output_shape() -> Shape {
    Shape::Record(
        RecordShape::new(
            "ProcessOutput",
            [
                ("exitCode".into(), Shape::Primitive(Primitive::Int)),
                ("stdout".into(), Shape::Primitive(Primitive::Bytes)),
                ("stderr".into(), Shape::Primitive(Primitive::Bytes)),
            ],
        )
        .expect("constant output fields"),
    )
}
/// A fixed argv prefix plus at most one Text argument. No whitespace splitting or shell expansion.
pub fn provider(
    name: impl Into<String>,
    command: Vec<String>,
    argument_key: impl Into<String>,
    config: ProcessConfig,
) -> Result<(ProviderDescription, ProcessInvoker), ProcessConfigError> {
    let argument_key = argument_key.into();
    if command.is_empty()
        || command[0].is_empty()
        || command.len() > 1024
        || command.iter().any(|part| part.contains('\0'))
        || argument_key.trim().is_empty()
        || argument_key == "timeout"
        || argument_key.len() > 256
        || !valid_timeout(config.timeout)
    {
        return Err(ProcessConfigError(
            "invalid command, argument key or timeout",
        ));
    }
    if line_bytes(&command).is_none_or(|n| n > config.argument_bytes) {
        return Err(ProcessConfigError(
            "command exceeds its argument byte budget",
        ));
    }
    if cfg!(windows) && is_windows_batch(&command[0]) {
        return Err(ProcessConfigError(
            "batch files require the explicit native shell provider",
        ));
    }
    let mut capability = Capability::new(["run"], output_shape(), Safety::Unsafe);
    capability.summary = "Runs a local process with one optional literal argument".into();
    capability.parameters = vec![
        Parameter::new(&argument_key, Shape::Primitive(Primitive::Text), false),
        Parameter::new("timeout", Shape::Primitive(Primitive::Duration), false),
    ];
    let description = ProviderDescription::new(name, [capability], vec![])
        .map_err(|_| ProcessConfigError("invalid provider metadata"))?;
    Ok((
        description,
        ProcessInvoker {
            launch: None,
            command: command.into(),
            argument_key: argument_key.into(),
            config,
            program_text: false,
        },
    ))
}
pub(crate) fn is_windows_batch(program: &str) -> bool {
    let name = program.to_ascii_lowercase();
    let name = name.trim_end_matches([' ', '.']);
    name.ends_with(".bat") || name.ends_with(".cmd")
}
pub fn wrapping(
    name: impl Into<String>,
    executable: impl Into<String>,
    config: ProcessConfig,
) -> Result<(ProviderDescription, ProcessInvoker), ProcessConfigError> {
    provider(name, vec![executable.into()], "args", config)
}
/// Native shell semantics are explicit: POSIX sh on Unix, cmd.exe on Windows.
pub(crate) const SHELL_NAME: &str = if cfg!(windows) { "cmd" } else { "sh" };

pub fn shell(
    config: ProcessConfig,
) -> Result<(ProviderDescription, ProcessInvoker), ProcessConfigError> {
    shell_named(SHELL_NAME, config)
}
pub(crate) fn shell_named(
    alias: &str,
    config: ProcessConfig,
) -> Result<(ProviderDescription, ProcessInvoker), ProcessConfigError> {
    #[cfg(unix)]
    let (name, command, language) = ("sh", vec!["/bin/sh".into(), "-c".into()], "sh");
    #[cfg(windows)]
    let (name, command, language) = (
        "cmd",
        vec!["cmd.exe".into(), "/D".into(), "/S".into(), "/C".into()],
        "cmd",
    );
    #[cfg(not(any(unix, windows)))]
    return Err(ProcessConfigError("no default shell on this platform"));
    #[cfg(any(unix, windows))]
    {
        let _ = name;
        let (description, mut invoker) = provider(alias, command, "cmd", config)?;
        invoker.program_text = true;
        let mut capability = description
            .capabilities()
            .next()
            .expect("one capability")
            .as_ref()
            .clone();
        capability.summary = "Runs an explicitly supplied native shell program".into();
        capability.parameters[0].content = Some(language.into());
        Ok((
            ProviderDescription::new(alias, [capability], vec![])
                .expect("validated shell metadata"),
            invoker,
        ))
    }
}
#[derive(Clone)]
pub struct ProcessInvoker {
    launch: Option<Arc<ManagedLaunch>>,
    command: Arc<[String]>,
    argument_key: Arc<str>,
    config: ProcessConfig,
    /// The argument is a program in the native shell's language rather than one literal
    /// argument, and reaches that shell exactly as written.
    program_text: bool,
}
impl fmt::Debug for ProcessInvoker {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProcessInvoker")
            .field("fixed_arguments", &self.command.len())
            .finish_non_exhaustive()
    }
}
impl Invoker for ProcessInvoker {
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let (line, timeout) = this.prepare(&call).map_err(Failure::error)?;
            let deadline = Instant::now()
                .checked_add(timeout)
                .ok_or_else(|| Failure::Arguments.error())?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let mut command = Command::new(&line[0]);
            let supplied = usize::from(line.len() > this.command.len());
            let literal = if this.program_text && cfg!(windows) {
                line.len() - supplied
            } else {
                line.len()
            };
            command.args(&line[1..literal]);
            // cmd.exe does not read the escaping other programs expect in an argument: a
            // quote inside the program would arrive as a backslash and a quote. With /S it
            // takes everything between the first and the last quote as the program.
            #[cfg(windows)]
            if let Some(program) = line.get(literal) {
                command.raw_arg(format!("\"{program}\""));
            }
            command
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            let starting = cancellation.clone();
            let launch = this.launch.clone();
            let mut child = tokio::task::spawn_blocking(move || {
                serialized_spawn(|| {
                    if starting.is_cancelled() {
                        return Err(Failure::Cancelled);
                    }
                    if Instant::now() >= deadline {
                        return Err(Failure::Timeout);
                    }
                    if let Some(launch) = launch {
                        launch.apply(&mut command)?;
                    }
                    ownership::LocalChild::spawn(command, false).map_err(|_| Failure::Spawn)
                })
            })
            .await
            .map_err(|_| Failure::Internal.error())?
            .map_err(Failure::error)?;
            let captured = capture(
                child.inner(),
                deadline,
                this.config.output_bytes,
                &cancellation,
            )
            .await;
            match captured {
                Ok((status, stdout, stderr)) => {
                    if cancellation.is_cancelled() {
                        child
                            .cancel()
                            .await
                            .map_err(|error| Failure::Cleanup(error).error())?;
                        return Err(InvocationError::Cancelled);
                    }
                    child.complete();
                    Value::new(
                        output_shape(),
                        Data::Record(IndexMap::from_iter([
                            ("exitCode".into(), Data::Int(exit_code(status))),
                            ("stdout".into(), Data::Bytes(stdout.into())),
                            ("stderr".into(), Data::Bytes(stderr.into())),
                        ])),
                        Provenance::default().with_fact(
                            "ranLocally",
                            if this.launch.is_some() {
                                "managed local process".into()
                            } else {
                                line.join(" ")
                            },
                        ),
                    )
                    .map_err(|_| Failure::Internal.error())
                }
                Err(reason) => {
                    // start_kill alone is not a reap. Always wait, even if signalling reports an
                    // error (the process may have exited concurrently or be outside our privilege).
                    let reaped = child.cancel().await;
                    // A successful wait also resolves the signal-vs-natural-exit race.
                    if let Err(error) = reaped {
                        return Err(Failure::Cleanup(error).error());
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
impl ProcessInvoker {
    pub fn managed(
        mut self,
        target: &wes_core::environments::Target,
        credentials: Arc<dyn wes_engine::credentials::Credentials>,
        names: Vec<String>,
    ) -> Result<Self, ProcessConfigError> {
        let cwd = local_directory(target.cwd()).map_err(ProcessConfigError)?;
        if names.len() > 128
            || names.iter().any(|n| {
                n.is_empty()
                    || !n.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
                    || target.variables().contains_key(n)
            })
        {
            return Err(ProcessConfigError(
                "managed launch requires an absolute cwd and distinct explicit credential env slots",
            ));
        }
        self.launch = Some(Arc::new(ManagedLaunch {
            cwd,
            variables: target.variables().clone(),
            credentials,
            names,
        }));
        Ok(self)
    }
    fn prepare(&self, call: &Call) -> Result<(Vec<String>, Duration), Failure> {
        if call.capability.streaming {
            return Err(Failure::Arguments);
        }
        let mut line = self.command.to_vec();
        if let Some(argument) = call.arguments.get(self.argument_key.as_ref()) {
            let Data::Text(text) = argument.data() else {
                return Err(Failure::Arguments);
            };
            if text.contains('\0') || text.len() > self.config.argument_bytes {
                return Err(Failure::Arguments);
            }
            line.push(text.to_string());
        }
        if line_bytes(&line).is_none_or(|n| n > self.config.argument_bytes) {
            return Err(Failure::Arguments);
        }
        let timeout = match call.arguments.get("timeout") {
            None => self.config.timeout,
            Some(value) => {
                let Data::Duration(duration) = value.data() else {
                    return Err(Failure::Arguments);
                };
                let parts = duration.parts();
                let seconds = u64::try_from(parts.seconds()).map_err(|_| Failure::Arguments)?;
                Duration::new(seconds, parts.nanos())
            }
        };
        if !valid_timeout(timeout) {
            return Err(Failure::Arguments);
        }
        Ok((line, timeout))
    }
}
struct ManagedLaunch {
    cwd: std::path::PathBuf,
    variables: std::collections::BTreeMap<String, String>,
    credentials: Arc<dyn wes_engine::credentials::Credentials>,
    names: Vec<String>,
}
impl ManagedLaunch {
    fn apply(&self, command: &mut Command) -> Result<(), Failure> {
        use wes_engine::credentials::ExposeSecret;
        let snapshots = self
            .credentials
            .snapshot(&self.names)
            .map_err(|_| Failure::Arguments)?;
        command
            .env_clear()
            .envs(&self.variables)
            .current_dir(&self.cwd);
        for (name, secret) in snapshots {
            let secret = secret.ok_or(Failure::Arguments)?;
            if secret.expose_secret().contains('\0') {
                return Err(Failure::Arguments);
            }
            command.env(name, secret.expose_secret());
        }
        Ok(())
    }
}
fn line_bytes(line: &[String]) -> Option<usize> {
    line.iter()
        .try_fold(line.len().saturating_sub(1), |n, s| n.checked_add(s.len()))
}
fn valid_timeout(timeout: Duration) -> bool {
    !timeout.is_zero() && timeout.as_nanos() <= i64::MAX as u128
}
async fn read_pipe<P: AsyncRead + Unpin>(
    pipe: &mut Option<P>,
    buffer: &mut [u8],
) -> io::Result<usize> {
    match pipe {
        Some(pipe) => pipe.read(buffer).await,
        None => std::future::pending().await,
    }
}
pub(crate) async fn capture(
    child: &mut Child,
    deadline: Instant,
    limit: usize,
    cancellation: &CancellationToken,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Failure> {
    capture_input(child, deadline, limit, cancellation, None).await
}

pub(crate) async fn capture_input(
    child: &mut Child,
    deadline: Instant,
    limit: usize,
    cancellation: &CancellationToken,
    input: Option<Vec<u8>>,
) -> Result<(ExitStatus, Vec<u8>, Vec<u8>), Failure> {
    use tokio::io::AsyncWriteExt;
    let mut stdin = child.stdin.take();
    let input = input.unwrap_or_default();
    let mut offset = 0;
    let mut stdout = child.stdout.take();
    let mut stderr = child.stderr.take();
    let mut output = Vec::new();
    let mut errors = Vec::new();
    let mut status = None;
    let mut out_buf = [0u8; 8192];
    let mut err_buf = [0u8; 8192];
    loop {
        if offset == input.len() {
            stdin = None;
        }
        if let Some(status) = status
            && stdout.is_none()
            && stderr.is_none()
        {
            return Ok((status, output, errors));
        }
        // A daily wake keeps even valid long duration arguments inside practical timer-wheel spans.
        let wake = deadline.min(Instant::now() + Duration::from_secs(86_400));
        tokio::select! {
            biased;
            _=cancellation.cancelled()=>return Err(Failure::Cancelled),
            _=tokio::time::sleep_until(wake)=>if Instant::now()>=deadline { return Err(Failure::Timeout); },
            written = async { stdin.as_mut().expect("guarded").write(&input[offset..]).await }, if stdin.is_some() => {
                match written { Ok(n) if n > 0 => offset += n, _ => stdin = None }
            },
            waited=child.wait(),if status.is_none()=>status=Some(waited.map_err(|_|Failure::Wait)?),
            read=read_pipe(&mut stdout,&mut out_buf)=> {
                let n=read.map_err(|_|Failure::Read)?;
                if n==0 { stdout=None; } else { append(&mut output,errors.len(),&out_buf[..n],limit)?; }
            }
            read=read_pipe(&mut stderr,&mut err_buf)=> {
                let n=read.map_err(|_|Failure::Read)?;
                if n==0 { stderr=None; } else { append(&mut errors,output.len(),&err_buf[..n],limit)?; }
            }
        }
    }
}
fn append(target: &mut Vec<u8>, other: usize, bytes: &[u8], limit: usize) -> Result<(), Failure> {
    if bytes.len() > limit.saturating_sub(target.len()).saturating_sub(other) {
        return Err(Failure::OutputLimit);
    }
    target.extend_from_slice(bytes);
    Ok(())
}
fn exit_code(status: ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        128 + i64::from(status.signal().unwrap_or(0))
    }
    #[cfg(not(unix))]
    {
        -1
    }
}
pub(crate) enum Failure {
    Arguments,
    Spawn,
    Timeout,
    OutputLimit,
    Read,
    Wait,
    Cleanup(ownership::CleanupError),
    Cancelled,
    Internal,
}
impl Failure {
    pub(crate) fn error(self) -> InvocationError {
        let cleanup = match &self {
            Self::Cleanup(error) => Some(error.summary()),
            _ => None,
        };
        let (code, message) = match self {
            Self::Arguments => (
                "PROC001",
                "Invalid process argument, timeout or argument byte budget",
            ),
            Self::Spawn => ("PROC002", "Could not start the local process"),
            Self::Timeout => (
                "PROC003",
                "The local process or its output pipes exceeded the time budget",
            ),
            Self::OutputLimit => ("PROC004", "Combined process output exceeds its byte budget"),
            Self::Read => ("PROC005", "Could not read a process output pipe"),
            Self::Wait => ("PROC006", "Could not observe the local process exit"),
            Self::Cleanup(_) => ("PROC007", "Could not confirm normal local process cleanup"),
            Self::Cancelled => return InvocationError::Cancelled,
            Self::Internal => ("PROC008", "Process output construction failed"),
        };
        InvocationError::Failed(
            ErrorValue::new(
                ErrorId::new(Uuid::new_v4().to_string()).expect("UUID"),
                code,
                cleanup.map_or_else(
                    || message.to_owned(),
                    |detail| format!("{message}: {detail}"),
                ),
                vec![],
                None,
            )
            .expect("constant process error"),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn batch_extensions_and_timeout_bounds_are_checked_without_platform_guessing() {
        for file in ["tool.bat", "C:\\path\\TOOL.CMD", "tool.bat. "] {
            assert!(is_windows_batch(file));
        }
        assert!(!is_windows_batch("cmd.exe"));
        assert!(!is_windows_batch("tool.bat.exe"));
        assert!(!valid_timeout(Duration::ZERO));
        assert!(valid_timeout(Duration::from_nanos(i64::MAX as u64)));
        assert!(!valid_timeout(Duration::from_nanos(i64::MAX as u64 + 1)));
    }

    #[test]
    fn output_accounting_is_aggregate_checked_and_atomic() {
        let mut target = vec![1, 2];
        assert!(append(&mut target, 2, &[3], 4).is_err());
        assert_eq!(target, [1, 2]);
        assert!(append(&mut target, usize::MAX, &[3], 4).is_err());
        append(&mut target, 1, &[3], 4).ok().unwrap();
        assert_eq!(target, [1, 2, 3]);
    }
}
