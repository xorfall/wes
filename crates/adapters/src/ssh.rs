//! Finite execution through an explicitly configured OpenSSH client. The remote
//! command is POSIX shell text; every data operand is quoted, never interpolated.
use std::{path::Path, process::Stdio, sync::Arc};
use tokio::{process::Command, time::Instant};
use wes_core::{
    Data, ErrorId, ErrorValue, Provenance, Value,
    capability::ProviderDescription,
    environments::{Binding, SshTarget, Target, TargetKind},
};
use wes_engine::{
    driver::CancellationToken,
    environments::Authority,
    imports::ImportProduct,
    providers::{Call, InvocationError, InvocationFuture, Invoker},
};

fn argument_limit() -> usize {
    wes_budgets::get("ssh.argument.bytes") as usize
}
fn output_limit() -> usize {
    wes_budgets::get("ssh.output.bytes") as usize
}

fn host_path(value: &str) -> bool {
    Path::new(value).is_absolute()
        && value.len() <= 4096
        && !value
            .chars()
            .any(|c| c.is_control() || matches!(c, '%' | '$' | '"'))
}
fn word(value: &str, host: bool) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && !value.starts_with('-')
        && value.bytes().all(|c| {
            c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.') || (host && c == b':')
        })
}
fn variable(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .enumerate()
            .all(|(i, c)| c.is_ascii_alphabetic() || c == b'_' || (i > 0 && c.is_ascii_digit()))
}
pub(crate) fn validate(target: &Target) -> Result<(), &'static str> {
    let TargetKind::Ssh(config) = target.kind() else {
        return Err("Expected an SSH target");
    };
    if !host_path(&config.client)
        || !host_path(&config.identity_file)
        || !host_path(&config.known_hosts)
    {
        return Err(
            "SSH client, identity_file and known_hosts require absolute host paths without control characters or expansion tokens",
        );
    }
    if !word(&config.host, true) || !word(&config.user, false) || config.port == 0 {
        return Err(
            "SSH requires an explicit host, user and port; flags, aliases with whitespace and URI syntax are unsupported",
        );
    }
    if target.cwd().is_some_and(|p| !p.starts_with('/'))
        || !target.variables().keys().all(|k| variable(k))
    {
        return Err(
            "SSH requires an absolute POSIX remote cwd and POSIX environment variable names",
        );
    }
    #[cfg(windows)]
    if crate::process::is_windows_batch(&config.client) {
        return Err("SSH client must be a native executable, not a Windows batch file");
    }
    Ok(())
}

/// Live prerequisites, separate from inert binding validation and restore.
/// Metadata is sufficient: never read or retain private-key bytes.
pub(crate) fn validate_inputs(config: &SshTarget) -> Result<(), &'static str> {
    for (path, missing, invalid, unavailable) in [
        (
            &config.identity_file,
            "SSH identity file was not found",
            "SSH identity path must name a regular file",
            "SSH identity file metadata could not be accessed",
        ),
        (
            &config.known_hosts,
            "SSH known-hosts file was not found",
            "SSH known-hosts path must name a regular file",
            "SSH known-hosts file metadata could not be accessed",
        ),
    ] {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {}
            Ok(_) => return Err(invalid),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Err(missing),
            Err(_) => return Err(unavailable),
        }
    }
    Ok(())
}

pub(crate) fn build(
    alias: &str,
    binding: &Binding,
    authority: Authority,
) -> Result<ImportProduct, &'static str> {
    let import = binding.import();
    validate(import.target())?;
    let program = crate::execution_targets::command::Program::capture(binding)?;
    if import.endpoint().is_some() || !import.credential_refs().is_empty() {
        return Err(
            "SSH exec uses explicit key/known-hosts files; endpoint overrides and credential injection are unsupported",
        );
    }
    if import.timeout_ms().is_some_and(|n| n > 3_600_000) {
        return Err("SSH timeout must not exceed one hour");
    }
    let run = program.capability();
    ImportProduct::new(ProviderDescription::new(alias, [run], vec![]).map_err(|_| "Invalid SSH provider metadata")?,
        Arc::new(SshExec {binding: binding.clone(), authority}), vec![
            "SSH uses the explicit client, identity file and known-hosts file with strict host verification. Identity and known-hosts file metadata is checked during planning and before launch; their contents remain live and are not retained. The remote account environment is inherited; host environment is not forwarded. Client disconnect/exit 255 cannot prove remote completion. No automatic retry, TTY, stdin or secret injection.".into()
        ]).map_err(|_| "SSH provider metadata exceeds budget")
}

#[derive(Clone)]
struct SshExec {
    binding: Binding,
    authority: Authority,
}

fn quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
fn option_path(value: &str) -> String {
    format!("\"{}\"", value.replace('\\', "\\\\"))
}

fn remote_command(binding: &Binding, argv: &[String]) -> Result<String, InvocationError> {
    let target = binding.import().target();
    let mut command = String::new();
    if let Some(cwd) = target.cwd() {
        command.push_str(&format!("cd {} && ", quote(cwd)));
    }
    for (key, value) in target.variables() {
        command.push_str(&format!("{key}={} ", quote(value)));
    }
    command.push_str("exec ");
    command.push_str(
        &argv
            .iter()
            .map(|arg| quote(arg))
            .collect::<Vec<_>>()
            .join(" "),
    );
    if command.len() > argument_limit() {
        return Err(failure(
            "SSH_ARGUMENT",
            "Encoded remote command exceeds its byte budget",
        ));
    }
    Ok(command)
}

fn client_arguments(config: &SshTarget) -> Vec<String> {
    let mut arguments: Vec<String> = ["-F", "none", "-a", "-x"]
        .into_iter()
        .map(str::to_owned)
        .collect();
    for option in [
        "BatchMode=yes",
        "StrictHostKeyChecking=yes",
        "GlobalKnownHostsFile=none",
        "IdentityAgent=none",
        "IdentitiesOnly=yes",
        "IdentityFile=none",
        "CertificateFile=none",
        "ControlMaster=no",
        "ControlPath=none",
        "ClearAllForwardings=yes",
        "PermitLocalCommand=no",
        "ProxyCommand=none",
        "ProxyJump=none",
        "UpdateHostKeys=no",
        "VerifyHostKeyDNS=no",
        "NumberOfPasswordPrompts=0",
        "PreferredAuthentications=publickey",
        "ConnectionAttempts=1",
    ] {
        arguments.extend(["-o".to_owned(), option.to_owned()]);
    }
    arguments.extend(
        [
            "-o",
            &format!("IdentityFile={}", option_path(&config.identity_file)),
            "-o",
            &format!("UserKnownHostsFile={}", option_path(&config.known_hosts)),
            "-p",
            &config.port.to_string(),
            "-l",
            &config.user,
            "--",
            &config.host,
        ]
        .into_iter()
        .map(str::to_owned),
    );
    arguments
}

fn client_command(config: &SshTarget, remote: &str, input: bool) -> Command {
    let mut command = Command::new(&config.client);
    if !input {
        command.arg("-n");
    }
    command
        .env_clear()
        .arg("-T")
        .args(client_arguments(config))
        .arg(remote);
    command
        .stdin(if input { Stdio::piped() } else { Stdio::null() })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    command
}

impl Invoker for SshExec {
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture {
        let this = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let (argv, timeout) =
                crate::execution_targets::command::Program::capture(&this.binding)
                    .and_then(|program| program.prepare(&this.binding, &call))
                    .map_err(|message| failure("SSH_ARGUMENT", message))?;
            execute(
                this.binding,
                this.authority,
                argv,
                None,
                timeout,
                output_limit(),
                cancellation,
            )
            .await
        })
    }
}

pub(crate) async fn execute(
    binding: Binding,
    authority: Authority,
    argv: Vec<String>,
    input: Option<Vec<u8>>,
    timeout: std::time::Duration,
    output_limit: usize,
    cancellation: CancellationToken,
) -> Result<Value, InvocationError> {
    let this = SshExec { binding, authority };
    let remote = remote_command(&this.binding, &argv)?;
    let target = this.binding.import().target();
    let TargetKind::Ssh(config) = target.kind() else {
        return Err(failure("SSH_ARGUMENT", "Expected an SSH target"));
    };
    let lease = this
        .authority
        .availability_lease(this.binding.environment().identity())
        .map_err(|_| {
            failure(
                "ENV020",
                "SSH environment authority is disabled or unavailable",
            )
        })?;
    let deadline = Instant::now() + timeout;
    let mut command = client_command(config, &remote, input.is_some());
    let starting = cancellation.clone();
    let launch_lease = lease.clone();
    let authority = this.authority.clone();
    let identity = this.binding.environment().identity().to_owned();
    let launch_inputs = config.clone();
    let mut child = tokio::task::spawn_blocking(move || {
        if starting.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        validate_inputs(&launch_inputs).map_err(|message| {
            failure(
                "SSH_START",
                &format!(
                    "{message}; SSH client was not started and no remote command was submitted"
                ),
            )
        })?;
        crate::process::serialized_spawn(|| {
            if starting.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if launch_lease.is_cancelled() || !authority.available(&identity) {
                return Err(failure(
                    "ENV020",
                    "SSH environment authority ended before client launch",
                ));
            }
            if Instant::now() >= deadline {
                return Err(failure(
                    "SSH_START",
                    "SSH time budget ended before client launch; no remote command was submitted",
                ));
            }
            command.spawn().map_err(|_| {
                failure(
                    "SSH_START",
                    "Could not start the configured SSH client; no remote command was submitted",
                )
            })
        })
    })
    .await
    .map_err(|_| unknown("SSH client launch could not be observed"))??;
    let captured = tokio::select! { biased;
        () = lease.cancelled() => Err(crate::process::Failure::Cancelled),
        result = crate::process::capture_input(&mut child, deadline, output_limit, &cancellation, input) => result,
    };
    match captured {
        Ok((status, stdout, stderr)) if !cancellation.is_cancelled() && !lease.is_cancelled() => {
            let Some(code) = status.code().filter(|code| *code != 255) else {
                return Err(unknown_with_stderr(
                    "SSH client did not report a trustworthy remote exit (status 255 or signal)",
                    &stderr,
                ));
            };
            Value::new(crate::process::output_shape(), Data::Record(indexmap::IndexMap::from_iter([
                        ("exitCode".into(), Data::Int(i64::from(code))), ("stdout".into(), Data::Bytes(stdout.into())),
                        ("stderr".into(), Data::Bytes(stderr.into())),
                    ])), Provenance::default().with_fact("ssh.host", &config.host)
                        .with_fact("ssh.user", &config.user).with_fact("ssh.port", config.port.to_string())
                        .cautioned(["SSH remote account environment and filesystem are live; host identity is verified by the configured OpenSSH client.".into()]))
                        .map_err(|_| failure("SSH_OUTPUT", "SSH output construction failed"))
        }
        captured => {
            let _ = child.start_kill();
            if child.wait().await.is_err() {
                return Err(unknown("SSH local client cleanup could not be confirmed"));
            }
            let reason = match captured {
                Err(crate::process::Failure::Timeout) => "SSH execution exceeded its time budget",
                Err(crate::process::Failure::OutputLimit) => {
                    "SSH execution exceeded its output budget"
                }
                _ if lease.is_cancelled() => "SSH environment authority ended",
                _ if cancellation.is_cancelled() => "SSH execution was cancelled",
                _ => "SSH output or exit observation was interrupted",
            };
            Err(unknown(reason))
        }
    }
}

fn failure(code: &str, message: &str) -> InvocationError {
    InvocationError::Failed(
        ErrorValue::new(
            ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
            code,
            message,
            vec![],
            None,
        )
        .expect("static SSH diagnostic"),
    )
}
fn unknown(reason: &str) -> InvocationError {
    failure(
        ErrorValue::REMOTE_OUTCOME_UNKNOWN,
        &format!(
            "{reason}; remote work may have run or still be running. No automatic retry or remote cleanup is claimed."
        ),
    )
}

fn unknown_with_stderr(reason: &str, stderr: &[u8]) -> InvocationError {
    const LIMIT: usize = 4096;
    let sample = &stderr[..stderr.len().min(LIMIT)];
    let mut detail = String::from_utf8_lossy(sample)
        .chars()
        .map(|ch| {
            if ch.is_control() {
                ch.escape_default().to_string()
            } else {
                ch.to_string()
            }
        })
        .collect::<String>();
    if stderr.len() > LIMIT {
        detail.push_str(" … diagnostic truncated");
    }
    if detail.is_empty() {
        return unknown(reason);
    }
    InvocationError::Failed(ErrorValue::new(
        ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"), ErrorValue::REMOTE_OUTCOME_UNKNOWN,
        format!("{reason}; remote work may have run or still be running. No automatic retry or remote cleanup is claimed."),
        vec![wes_core::ValidationIssue { path: "/ssh/stderr".into(), code: "SSH_CLIENT_DIAGNOSTIC".into(), message: detail }], None,
    ).expect("bounded SSH diagnostic"))
}

#[cfg(all(test, unix))]
mod tests;

/// POSIX interactive terminal preparation. Host workspace credentials are never forwarded.
pub(crate) fn terminal(
    target: &Target,
) -> Result<crate::execution_targets::TerminalPlan, &'static str> {
    validate(target)?;
    let TargetKind::Ssh(config) = target.kind() else {
        return Err("Expected an SSH target");
    };
    let mut remote = String::new();
    if let Some(cwd) = target.cwd() {
        remote.push_str(&format!("cd {} || exit $?; ", quote(cwd)));
    }
    // Export assignments before selecting the account shell; every configured value is literal.
    for (key, value) in target.variables() {
        remote.push_str(&format!("export {key}={}; ", quote(value)));
    }
    remote.push_str("exec \"${SHELL:-/bin/sh}\" -i");
    if remote.len() > argument_limit() {
        return Err("Encoded remote terminal command exceeds its byte budget");
    }
    let mut launch = crate::execution_targets::HostLaunch {
        executable: config.client.clone().into(),
        ..Default::default()
    };
    launch.arg("-tt");
    for option in [
        "EscapeChar=none",
        "ConnectTimeout=10",
        "ServerAliveInterval=15",
        "ServerAliveCountMax=3",
    ] {
        launch.arg("-o");
        launch.arg(option);
    }
    for argument in client_arguments(config) {
        launch.arg(argument);
    }
    launch.arg(remote);
    launch.env("TERM", "xterm-256color");
    let inputs = config.clone();
    Ok(
        crate::execution_targets::TerminalPlan::remote(launch, "SSH").with_preflight(move || {
            validate_inputs(&inputs).map_err(|message| {
                std::io::Error::other(format!("{message}; SSH client was not started"))
            })
        }),
    )
}
