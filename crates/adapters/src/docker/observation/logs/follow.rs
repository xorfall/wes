//! One run owns one response. No reconnect, polling, background collector or UI-owned reader.
use super::super::streaming::{authority_lost, sink_error};
use super::*;
use wes_engine::streams::StreamSink;

fn line_limit() -> usize {
    wes_budgets::get("docker.log.line.bytes") as usize
}
pub(crate) fn capability() -> Capability {
    let mut cap = super::capability();
    cap.path = vec!["logs".into(), "follow".into()];
    cap.streaming = true;
    cap.result = item_shape();
    cap.summary = "Follow an exact container's logs into the ordinary bounded stream window; tail defaults to 200, 64-KiB line prefixes are marked, no reconnect; cancel closes the reader".into();
    cap
}
fn item_shape() -> Shape {
    record(
        "DockerLogEvent",
        vec![
            ("container", primitive(Primitive::Text)),
            ("sequence", primitive(Primitive::Int)),
            ("received_at_ns", primitive(Primitive::Int)),
            ("stream", primitive(Primitive::Text)),
            ("timestamp_ns", option(primitive(Primitive::Int))),
            ("text", primitive(Primitive::Text)),
            ("partial", primitive(Primitive::Bool)),
            ("lossy", primitive(Primitive::Bool)),
            ("line_truncated", primitive(Primitive::Bool)),
        ],
    )
}
impl Observation {
    pub(in crate::docker::observation) async fn follow_logs(
        &self,
        call: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
        lease: CancellationToken,
    ) -> Result<(), InvocationError> {
        let this = self;
        let selected = Selection::from_call(&call)?;
        let opening_timeout =
            Duration::from_millis(this.binding.import().timeout_ms().unwrap_or(15_000) as u64);
        let run = async {
            let (mut response, tty) =
                tokio::time::timeout(opening_timeout, this.open_logs(&selected, true))
                    .await
                    .map_err(|_| {
                        failure(
                            "DOCKER_TIMEOUT",
                            "Docker log follow did not open before its deadline",
                        )
                    })??;
            this.permitted()?;
            sink.opened().map_err(sink_error)?;
            let mut decoder = Decoder::new(tty, line_limit());
            let mut sequence = 0i64;
            let mut emit = |line: Line| {
                if cancellation.is_cancelled() {
                    return Err(InvocationError::Cancelled);
                }
                if lease.is_cancelled() {
                    return Err(authority_lost());
                }
                sequence = sequence.checked_add(1).ok_or_else(|| {
                    failure(
                        "DOCKER_LOG_SEQUENCE",
                        "Docker log sequence capacity exhausted; start a new run",
                    )
                })?;
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| malformed())?
                    .as_nanos()
                    .try_into()
                    .map_err(|_| malformed())?;
                let mut fields = line_data(&line);
                fields.extend([
                    (
                        "container".into(),
                        Data::Text(selected.container.as_str().into()),
                    ),
                    ("sequence".into(), Data::Int(sequence)),
                    ("received_at_ns".into(), Data::Int(now)),
                    ("line_truncated".into(), Data::Bool(line.truncated)),
                ]);
                let value = Value::new(
                    call.capability.result.clone(),
                    Data::Record(fields),
                    Provenance::default().with_fact("docker.api", "1.45"),
                )
                .map_err(|_| malformed())?;
                Ok::<_, InvocationError>(value)
            };
            loop {
                let chunk = response.chunk().await.map_err(|_| {
                    this.client.invalidate();
                    failure(
                        "DOCKER_TRANSPORT",
                        "Docker log follow was interrupted; no reconnect was attempted",
                    )
                })?;
                let Some(chunk) = chunk else {
                    let mut lines = Vec::new();
                    decoder.finish(false, &mut |line| {
                        lines.push(line);
                        Ok(())
                    })?;
                    for line in lines {
                        sink.send(emit(line)?).await.map_err(sink_error)?;
                    }
                    return Ok(());
                };
                // At most one decoded row per input byte, plus the bounded partial line.
                // Await delivery before decoding the next part; no detached parser or collector.
                for part in chunk.chunks(1024) {
                    let mut lines = Vec::new();
                    decoder.feed(part, &mut |line| {
                        lines.push(line);
                        Ok(())
                    })?;
                    for line in lines {
                        sink.send(emit(line)?).await.map_err(sink_error)?;
                    }
                    // Bounded processing turns let cancellation/authority changes wake even
                    // during a continuous burst. No timer or extra protocol call.
                    tokio::task::yield_now().await;
                }
            }
        };
        run.await
    }
}
