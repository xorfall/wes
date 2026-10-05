//! All Docker readers share one cancellation/authority lifetime. No detached reader or reconnect.
use super::*;
use wes_engine::streams::{StreamError, StreamFuture, StreamSink, StreamingInvoker};
impl StreamingInvoker for Observation {
    fn subscribe(
        &self,
        call: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
    ) -> StreamFuture {
        let this = self.clone();
        Box::pin(async move {
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if !call.capability.streaming {
                return Err(failure(
                    "DOCKER_ARGUMENT",
                    "Expected a streaming Docker capability",
                ));
            }
            this.permitted()?;
            let lease = this.authority.availability_lease(this.binding.environment().identity())
                .map_err(|_| failure("ENV020", "Docker environment availability lease is unavailable or its capacity is exhausted"))?;
            let run = async {
                match call
                    .capability
                    .path
                    .iter()
                    .map(String::as_str)
                    .collect::<Vec<_>>()
                    .as_slice()
                {
                    ["logs", "follow"] => {
                        this.follow_logs(call, sink, cancellation.clone(), lease.clone())
                            .await
                    }
                    ["stats"] | ["events"] => this.follow_metrics(call, sink).await,
                    _ => Err(failure(
                        "DOCKER_ARGUMENT",
                        "Unknown Docker stream capability",
                    )),
                }
            };
            tokio::select! { biased;
                () = cancellation.cancelled() => Err(InvocationError::Cancelled),
                () = lease.cancelled() => Err(authority_lost()),
                result = run => result,
            }
        })
    }
}
pub(super) fn authority_lost() -> InvocationError {
    failure(
        "ENV020",
        "Docker stream stopped because its environment authority ended; re-enable and explicitly refresh to start a new run",
    )
}
pub(super) fn sink_error(error: StreamError) -> InvocationError {
    match error {
        StreamError::Closed => InvocationError::Cancelled,
        _ => failure(
            "DOCKER_STREAM_WINDOW",
            "Docker stream could not publish within the stream window budget",
        ),
    }
}
