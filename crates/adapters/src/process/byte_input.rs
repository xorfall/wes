//! Owned bounded stdin is joined with process/output completion on every path.
use super::*;
use tokio::io::AsyncWriteExt;
use wes_engine::byte_input::OwnedByteInput;

pub(super) fn invoke(
    this: ProcessInvoker,
    call: Call,
    cancellation: CancellationToken,
    mut input: OwnedByteInput,
) -> InvocationFuture {
    let policy = input.provenance().policy().clone();
    let work = async move {
        if !this.supports_byte_input() || !call.authority.is_local_user() {
            return Err(Failure::Arguments.error());
        }
        input.check()?;
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }

        let (mut child, deadline, line) = this
            .launch_call_with_input(&call, &cancellation, Some(input.extent()))
            .await?;
        let mut stdin = child.inner().stdin.take().expect("owned piped stdin");
        let owned_cancel = cancellation.child_token();
        let feeding_cancel = owned_cancel.clone();
        let feeding = tokio::spawn(async move {
            let result = async {
                loop {
                    if feeding_cancel.is_cancelled() {return Err(InvocationError::Cancelled);}
                    // A submitted storage read is joined before inspecting cancellation.
                    let Some(bytes) = input.next().await? else {break;};
                    input.check()?;
                    tokio::select! {
                        biased;
                        _ = feeding_cancel.cancelled() => return Err(InvocationError::Cancelled),
                        _ = tokio::time::sleep_until(deadline) => return Err(Failure::Timeout.error()),
                        written = stdin.write_all(&bytes) => written.map_err(|_|input_pipe_error())?,
                    }
                }
                stdin.shutdown().await.map_err(|_|input_pipe_error())?;
                input.check()
            }.await;
            drop(stdin);
            if result.is_err() {
                feeding_cancel.cancel();
            }
            (result, input)
        });

        let captured = capture(
            child.inner(),
            deadline,
            this.config.output_bytes,
            &owned_cancel,
        )
        .await;
        let reaped =
            if captured.is_err() || owned_cancel.is_cancelled() || cancellation.is_cancelled() {
                owned_cancel.cancel();
                child
                    .cancel()
                    .await
                    .map_err(|e| Failure::Cleanup(e).error())
            } else {
                Ok(())
            };
        let (fed, input) = feeding.await.map_err(|_| Failure::Internal.error())?;
        reaped?;
        if (fed.is_err() || owned_cancel.is_cancelled() || cancellation.is_cancelled())
            && captured.is_ok()
        {
            owned_cancel.cancel();
            child
                .cancel()
                .await
                .map_err(|e| Failure::Cleanup(e).error())?;
        }
        if cancellation.is_cancelled() {
            return Err(InvocationError::Cancelled);
        }
        fed?;
        input.check()?;
        let (status, stdout, stderr) = captured.map_err(Failure::error)?;
        child.complete();
        Value::new(
            output_shape(),
            Data::Record(IndexMap::from([
                ("exitCode".into(), Data::Int(exit_code(status))),
                ("stdout".into(), Data::Bytes(stdout.into())),
                ("stderr".into(), Data::Bytes(stderr.into())),
            ])),
            input.provenance().clone().with_fact(
                "ranLocally",
                if this.launch.is_some() {
                    "managed local process".into()
                } else {
                    line.join(" ")
                },
            ),
        )
        .map_err(|_| Failure::Internal.error())
    };
    Box::pin(async move {
        work.await.map_err(|error| match error {
            InvocationError::Failed(error) => InvocationError::Failed(error.with_policy(&policy)),
            error => error,
        })
    })
}
fn input_pipe_error() -> InvocationError {
    InvocationError::Failed(
        ErrorValue::new(
            ErrorId::new(Uuid::new_v4().to_string()).expect("uuid"),
            "PROC009",
            "The process did not accept its complete original byte input; no replay was attempted.",
            vec![],
            None,
        )
        .expect("fixed error"),
    )
}
