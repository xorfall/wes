//! Transient, generation/run-bound output shares the SSE connection with coherent metadata.
use super::projection::Projection;
use serde_json::json;
use std::{
    collections::VecDeque,
    io::{self, Write},
    sync::Arc,
};
use tokio::sync::{OwnedSemaphorePermit, broadcast, watch};
use wes_engine::{conversations::Output, runtime::Run};

fn max_frame() -> usize {
    wes_budgets::get("transport.conversation.bytes") as usize
}
pub(super) struct Frame {
    generation: String,
    run: Option<Run>,
    encoded: Arc<str>,
}
impl Frame {
    pub fn output(generation: String, run: Run, batch: &Output) -> io::Result<Self> {
        let value = json!({"event":"output", "node":run.node().as_str(), "run":run.id().as_str(), "text":batch.text, "omittedBytes":batch.omitted_bytes.to_string()});
        let mut writer = Limited(Vec::new());
        serde_json::to_writer(&mut writer, &value)?;
        let encoded = String::from_utf8(writer.0)
            .map_err(io::Error::other)?
            .into();
        Ok(Self {
            generation,
            run: Some(run),
            encoded,
        })
    }
    pub fn gap(generation: String) -> Self {
        Self {
            generation,
            run: None,
            encoded: Arc::from(r#"{"event":"output-gap"}"#),
        }
    }
    fn current(&self, projection: &Projection) -> bool {
        self.generation == projection.generation
            && self
                .run
                .as_ref()
                .is_none_or(|run| projection.runs.get(run.node()) == Some(run.id()))
    }
}
struct Limited(Vec<u8>);
impl Write for Limited {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > max_frame().saturating_sub(self.0.len()) {
            return Err(io::Error::other(
                "conversation frame exceeds its byte budget",
            ));
        }
        self.0.extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
pub(super) struct Client {
    updates: watch::Receiver<Option<Arc<Projection>>>,
    previous: Option<Arc<Projection>>,
    pending: VecDeque<Arc<str>>,
    live: broadcast::Receiver<Arc<Frame>>,
    held: Option<Arc<Frame>>,
    live_open: bool,
    _permit: OwnedSemaphorePermit,
}
impl Client {
    pub fn new(
        updates: watch::Receiver<Option<Arc<Projection>>>,
        live: broadcast::Receiver<Arc<Frame>>,
        permit: OwnedSemaphorePermit,
    ) -> Self {
        Self {
            updates,
            previous: None,
            pending: VecDeque::new(),
            live,
            held: None,
            live_open: true,
            _permit: permit,
        }
    }
    /// A coherent metadata delta travels as one frame, so clients do not render half a
    /// projection. Only one such batch is allowed in flight by the socket transport.
    pub async fn batch(&mut self, sequence: u64) -> Option<String> {
        let first = self.next().await?;
        let mut frames = vec![first];
        frames.extend(self.pending.drain(..));
        Some(format!(
            r#"{{"sequence":{},"events":[{}]}}"#,
            sequence,
            frames
                .iter()
                .map(|frame| frame.as_ref())
                .collect::<Vec<_>>()
                .join(",")
        ))
    }
    pub async fn next(&mut self) -> Option<Arc<str>> {
        loop {
            if let Some(frame) = self.pending.pop_front() {
                return Some(frame);
            }
            if let Some(current) = self.updates.borrow_and_update().clone() {
                let connecting_during_conversation =
                    self.previous.is_none() && current.has_conversations;
                self.pending = current.changes(self.previous.as_deref()).into();
                if connecting_during_conversation {
                    self.pending
                        .push_back(Frame::gap(current.generation.clone()).encoded);
                }
                self.previous = Some(current);
                if !self.pending.is_empty() {
                    continue;
                }
            }
            // Always drain new metadata first. A delayed old run or old workspace frame can never
            // append to a replacement dialogue, even if its node ID is reused in restored history.
            if let Some(frame) = self.held.take()
                && self
                    .previous
                    .as_ref()
                    .is_some_and(|projection| frame.current(projection))
            {
                return Some(frame.encoded.clone());
            }
            tokio::select! {
                biased;
                changed = self.updates.changed() => if changed.is_err() { return None; },
                frame = self.live.recv(), if self.live_open => match frame {
                    Ok(frame) => self.held = Some(frame),
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        if let Some(previous) = &self.previous { self.held = Some(Arc::new(Frame::gap(previous.generation.clone()))); }
                    },
                    Err(broadcast::error::RecvError::Closed) => self.live_open = false,
                },
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use wes_engine::runtime::{Effect, ExecutionTraits, Runtime};
    fn run() -> Run {
        let mut runtime = Runtime::new();
        runtime
            .add(
                (),
                [],
                ExecutionTraits {
                    pure: false,
                    repeatable: false,
                    bounded: false,
                },
            )
            .unwrap();
        runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap()
    }
    fn output(generation: &str, run: Run, text: &str) -> Arc<Frame> {
        Arc::new(
            Frame::output(
                generation.into(),
                run,
                &Output {
                    text: text.into(),
                    omitted_bytes: 0,
                },
            )
            .unwrap(),
        )
    }
    async fn json(client: &mut Client) -> serde_json::Value {
        serde_json::from_str(
            &tokio::time::timeout(Duration::from_secs(2), client.next())
                .await
                .unwrap()
                .unwrap(),
        )
        .unwrap()
    }
    async fn client(
        projection: Projection,
        capacity: usize,
    ) -> (
        Client,
        watch::Sender<Option<Arc<Projection>>>,
        broadcast::Sender<Arc<Frame>>,
    ) {
        let (updates, receive) = watch::channel(Some(Arc::new(projection)));
        let (live, events) = broadcast::channel(capacity);
        let permit = Arc::new(tokio::sync::Semaphore::new(1))
            .acquire_owned()
            .await
            .unwrap();
        (Client::new(receive, events, permit), updates, live)
    }
    #[test]
    fn frame_encoding_has_a_strict_bound_and_loss_count_is_exact_text() {
        let run = run();
        let frame = Frame::output(
            "g".into(),
            run.clone(),
            &Output {
                text: "\0".repeat(256 * 1024),
                omitted_bytes: u64::MAX,
            },
        )
        .unwrap();
        assert!(frame.encoded.len() <= max_frame());
        let value: serde_json::Value = serde_json::from_str(&frame.encoded).unwrap();
        assert_eq!(value["omittedBytes"], u64::MAX.to_string());
        let error = Frame::output(
            "g".into(),
            run,
            &Output {
                text: "private".repeat(max_frame()),
                omitted_bytes: 0,
            },
        )
        .err()
        .unwrap();
        assert!(!error.to_string().contains("private"));
    }
    #[tokio::test]
    async fn client_sends_metadata_first_and_discards_delayed_old_run_or_workspace_frames() {
        let old_run = run();
        let new_run = run();
        let mut projection = Projection::empty("old");
        projection
            .runs
            .insert(old_run.node().clone(), old_run.id().clone());
        let (mut client, updates, live) = client(projection, 8).await;
        assert_eq!(json(&mut client).await["event"], "session");
        assert_eq!(json(&mut client).await["event"], "log-delta");
        live.send(output("old", old_run.clone(), "stale run"))
            .ok()
            .unwrap();
        let mut projection = Projection::empty("new");
        projection
            .runs
            .insert(new_run.node().clone(), new_run.id().clone());
        updates.send_replace(Some(Arc::new(projection)));
        live.send(output("new", old_run, "wrong run")).ok().unwrap();
        live.send(output("new", new_run, "current")).ok().unwrap();
        let session = json(&mut client).await;
        assert_eq!(session["event"], "session");
        assert_eq!(session["generation"], "new");
        assert_eq!(json(&mut client).await["event"], "log-delta");
        assert_eq!(json(&mut client).await["text"], "current");
        drop(updates);
        assert!(client.next().await.is_none());
    }
    #[tokio::test]
    async fn client_reports_lag_and_connecting_during_a_live_dialogue_without_replaying_output() {
        let run = run();
        let mut projection = Projection::empty("g");
        projection.has_conversations = true;
        projection.runs.insert(run.node().clone(), run.id().clone());
        let (mut client, updates, live) = client(projection, 2).await;
        assert_eq!(json(&mut client).await["event"], "session");
        assert_eq!(json(&mut client).await["event"], "log-delta");
        assert_eq!(json(&mut client).await["event"], "output-gap");
        for n in 0..4 {
            live.send(output("g", run.clone(), &n.to_string()))
                .ok()
                .unwrap();
        }
        assert_eq!(json(&mut client).await["event"], "output-gap");
        assert_eq!(json(&mut client).await["text"], "2");
        assert_eq!(json(&mut client).await["text"], "3");
        drop(live);
        drop(updates);
        assert!(client.next().await.is_none());
    }
}
