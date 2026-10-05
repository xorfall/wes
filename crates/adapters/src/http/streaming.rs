//! One owned HTTP SSE response. No reconnect, hidden retry or detached reader.
use super::*;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use wes_engine::streams::{StreamError, StreamFuture, StreamSink, StreamingInvoker};
mod parser;
use parser::Parser;

impl StreamingInvoker for HttpInvoker {
    fn subscribe(
        &self,
        call: Call,
        sink: StreamSink,
        cancellation: CancellationToken,
    ) -> StreamFuture {
        let inner = self.inner.clone();
        Box::pin(async move {
            if !matches!(inner.transport, transport::Transport::Internal) {
                return Err(Failure::Request("curl transport supports finite HTTP only; live streams require internal on a local target").error());
            }
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            if !call.capability.streaming {
                return Err(
                    Failure::Request("stream invocation requires streaming metadata").error(),
                );
            }
            let prepare = inner.clone();
            let request = tokio::task::spawn_blocking(move || prepare.prepare(&call))
                .await
                .map_err(|_| Failure::Internal.error())?;
            if cancellation.is_cancelled() {
                return Err(InvocationError::Cancelled);
            }
            let mut prepared = request.map_err(Failure::error)?;
            // Preserve configured method/body and authentication, including an explicitly bound
            // Accept header. Never rebuild a second unauthenticated GET from only its URL.
            prepared
                .request
                .headers_mut()
                .entry(ACCEPT)
                .or_insert(HeaderValue::from_static("text/event-stream"));
            let until = tokio::time::Instant::now() + inner.config.request_timeout;
            let mut response = tokio::select! {
                biased;
                _ = cancellation.cancelled() => return Err(InvocationError::Cancelled),
                response = tokio::time::timeout_at(until, inner.client.execute(prepared.request)) => {
                    response.map_err(|_| Failure::Timeout.error())?.map_err(|error| Failure::transport(error).error())?
                }
            };
            let status = response.status();
            if !status.is_success() {
                let (_, body) = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(InvocationError::Cancelled),
                    response = tokio::time::timeout_at(until, inner.read_body(response)) => {
                        response.map_err(|_| Failure::Timeout.error())?.map_err(Failure::error)?
                    }
                };
                let error = tokio::task::spawn_blocking(move || {
                    Failure::Status {
                        status: status.as_u16(),
                        excerpt: prepared.redactor.excerpt(&body, 500),
                    }
                    .error()
                })
                .await
                .map_err(|_| Failure::Internal.error())?;
                return if cancellation.is_cancelled() {
                    Err(InvocationError::Cancelled)
                } else {
                    Err(error)
                };
            }
            let event_response = prepared
                .responses
                .get(&status.as_u16())
                .cloned()
                .ok_or(Failure::Response)
                .map_err(Failure::error)?;
            // 204 explicitly closes without data. Other successful responses must identify SSE.
            if status.as_u16() != 204
                && !response
                    .headers()
                    .get(CONTENT_TYPE)
                    .and_then(|v| v.to_str().ok())
                    .is_some_and(|v| {
                        v.split(';').next().is_some_and(|mime| {
                            mime.trim().eq_ignore_ascii_case("text/event-stream")
                        })
                    })
            {
                return Err(Failure::Response.error());
            }
            drop(prepared.redactor);
            sink.opened().map_err(sink_error)?;
            if status.as_u16() == 204 {
                return Ok(());
            }
            let limits = inner.config.response;
            let mut parser = Parser::new(
                limits.bytes.min(1024 * 1024),
                limits.bytes.min(8 * 1024 * 1024),
            );
            // Header/opening timeout is complete. An idle successful stream has no implicit finite
            // body deadline; its run owner may still impose an explicit lifetime timeout.
            loop {
                let chunk = tokio::select! {
                    biased;
                    _ = cancellation.cancelled() => return Err(InvocationError::Cancelled),
                    chunk = response.chunk() => chunk.map_err(|error| Failure::transport(error).error())?,
                };
                let Some(chunk) = chunk else {
                    return Ok(());
                };
                for part in chunk.chunks(16 * 1024) {
                    if cancellation.is_cancelled() {
                        return Err(InvocationError::Cancelled);
                    }
                    let bytes = part.to_vec();
                    let token = cancellation.clone();
                    let target = sink.clone();
                    let event_response = event_response.clone();
                    let processed = tokio::task::spawn_blocking(move || {
                        let result = parser.feed(&bytes, |event| {
                            if token.is_cancelled() {
                                return Err(Failure::Internal);
                            }
                            // Decode after framing, so arbitrary transport splits never corrupt a
                            // Unicode scalar. Lossy replacement follows SSE UTF-8 decoding semantics.
                            let text = String::from_utf8_lossy(event);
                            let decoded = event_response.decode(text.as_bytes(), limits);
                            match decoded {
                                Ok(value) => {
                                    target.blocking_send(value).map_err(|_| Failure::Internal)
                                }
                                // Tolerate malformed individual events while exposing the skipped item count.
                                Err(_) => target.reject_item().map_err(|_| Failure::Internal),
                            }
                        });
                        (parser, result)
                    })
                    .await
                    .map_err(|_| Failure::Internal.error())?;
                    parser = processed.0;
                    if cancellation.is_cancelled() {
                        return Err(InvocationError::Cancelled);
                    }
                    processed.1.map_err(Failure::error)?;
                }
            }
        })
    }
}
fn sink_error(error: StreamError) -> InvocationError {
    match error {
        StreamError::Closed => InvocationError::Cancelled,
        _ => Failure::Internal.error(),
    }
}
