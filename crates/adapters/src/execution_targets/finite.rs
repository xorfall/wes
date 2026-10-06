//! Internal finite byte-channel execution. Public process argv policy stays in Program.
use std::{process::Stdio, time::Duration};
use wes_core::{
    Data,
    environments::{Binding, TargetKind},
};
use wes_engine::{driver::CancellationToken, environments::Authority, providers::InvocationError};

pub(crate) struct Output {
    pub code: i64,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}
fn failed(message: &str) -> InvocationError {
    InvocationError::Failed(wes_engine::runtime::RuntimeCode::ExecutionFailed.error(message, None))
}
fn interrupted() -> InvocationError {
    InvocationError::Failed(wes_core::ErrorValue::new(
        wes_core::ErrorId::new(uuid::Uuid::new_v4().to_string()).expect("UUID"),
        wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN,
        "HTTP helper execution interrupted, cancelled, revoked or exceeded its budget; the request may have reached its destination. No automatic retry.",
        vec![], None,
    ).expect("constant diagnostic"))
}

pub(crate) async fn execute(
    binding: Binding,
    authority: Authority,
    argv: Vec<String>,
    input: Vec<u8>,
    timeout: Duration,
    limit: usize,
    token: CancellationToken,
) -> Result<Output, InvocationError> {
    if token.is_cancelled() {
        return Err(InvocationError::Cancelled);
    }
    let value = match binding.import().target().kind() {
        TargetKind::Ssh(_) => {
            crate::ssh::execute(binding, authority, argv, Some(input), timeout, limit, token)
                .await?
        }
        TargetKind::Docker { .. } => {
            crate::docker::execute(binding, authority, argv, Some(input), timeout, limit, token)
                .await?
        }
        TargetKind::Local => {
            let lease = authority
                .availability_lease(binding.environment().identity())
                .map_err(|_| failed("ENV020: HTTP environment is disabled or unavailable"))?;
            let target = binding.import().target();
            let mut command = tokio::process::Command::new(&argv[0]);
            command
                .args(&argv[1..])
                .env_clear()
                .env("PATH", "/usr/bin:/bin:/usr/local/bin")
                .envs(target.variables())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            command.current_dir(crate::process::local_directory(target.cwd()).map_err(failed)?);
            let deadline = tokio::time::Instant::now() + timeout;
            let starting = crate::process::serialized_spawn_async(|| {
                if lease.is_cancelled() || token.is_cancelled() {
                    return Err(InvocationError::Cancelled);
                }
                if tokio::time::Instant::now() >= deadline {
                    return Err(failed(
                        "HTTP helper time budget ended before launch; no request was submitted",
                    ));
                }
                command.spawn().map_err(|_| failed("HTTP helper could not start on the selected local target; a POSIX shell and curl are required"))
            });
            let mut child = tokio::select! { biased;
                () = lease.cancelled() => return Err(InvocationError::Cancelled),
                () = token.cancelled() => return Err(InvocationError::Cancelled),
                result = tokio::time::timeout_at(deadline, starting) => result
                    .map_err(|_| failed("HTTP helper time budget ended before launch; no request was submitted"))??,
            };
            let result = tokio::select! { biased;
                () = lease.cancelled() => Err(crate::process::Failure::Cancelled),
                result = crate::process::capture_input(&mut child, deadline, limit, &token, Some(input)) => result,
            };
            match result {
                Ok((status, stdout, stderr)) if !lease.is_cancelled() && !token.is_cancelled() => {
                    return Ok(Output {
                        code: status.code().map(i64::from).ok_or_else(interrupted)?,
                        stdout,
                        stderr,
                    });
                }
                _ => {
                    let _ = child.start_kill();
                    let _ = child.wait().await;
                    return Err(interrupted());
                }
            }
        }
    };
    let Data::Record(mut fields) = value.data().clone() else {
        return Err(failed("Invalid target output"));
    };
    let (Some(Data::Int(code)), Some(Data::Bytes(stdout)), Some(Data::Bytes(stderr))) = (
        fields.shift_remove("exitCode"),
        fields.shift_remove("stdout"),
        fields.shift_remove("stderr"),
    ) else {
        return Err(failed("Invalid target output"));
    };
    Ok(Output {
        code,
        stdout: stdout.to_vec(),
        stderr: stderr.to_vec(),
    })
}
