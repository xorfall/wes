//! Process-local admission constraints. A reservation grants no invocation or read authority.
use super::StreamError;
use std::{
    collections::BTreeSet,
    sync::{Mutex, OnceLock},
};

pub const MAX_EXCLUSIVE_RESOURCES: usize = 4;
pub const MAX_RESOURCE_PARTS: usize = 8;
pub const MAX_RESOURCE_PART_BYTES: usize = 4096;
const MAX_RESOURCE_KEY_BYTES: usize = 8192;
const MAX_ACTIVE_KEYS: usize = 1024;

/// Literal physical-resource namespace plus an immutable validated argument key.
/// Namespaces must be shared by aliases that address the same physical resource.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ExclusiveResource {
    namespace: String,
    parts: Vec<String>,
}
impl std::fmt::Debug for ExclusiveResource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExclusiveResource")
            .field("parts", &self.parts.len())
            .finish_non_exhaustive()
    }
}
impl ExclusiveResource {
    pub fn valid_namespace(namespace: &str) -> bool {
        !namespace.is_empty()
            && namespace.len() <= 128
            && namespace
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
    }
    pub fn new(namespace: &str, parts: Vec<String>) -> Result<Self, StreamError> {
        if !Self::valid_namespace(namespace)
            || parts.is_empty()
            || parts.len() > MAX_RESOURCE_PARTS
            || parts.iter().any(|s| {
                s.trim().is_empty()
                    || s.len() > MAX_RESOURCE_PART_BYTES
                    || s.chars().any(char::is_control)
            })
            || parts.iter().map(String::len).sum::<usize>() > MAX_RESOURCE_KEY_BYTES
        {
            return Err(StreamError::InvalidResource);
        }
        Ok(Self {
            namespace: namespace.into(),
            parts,
        })
    }
}

fn active() -> &'static Mutex<BTreeSet<ExclusiveResource>> {
    static ACTIVE: OnceLock<Mutex<BTreeSet<ExclusiveResource>>> = OnceLock::new();
    ACTIVE.get_or_init(Mutex::default)
}

/// Kept by the native lifetime task, including joined producer and controller cleanup.
pub(super) struct Reservation(Vec<ExclusiveResource>);
impl Reservation {
    pub(super) fn acquire(keys: Vec<ExclusiveResource>) -> Result<Self, StreamError> {
        if keys.len() > MAX_EXCLUSIVE_RESOURCES
            || keys.iter().collect::<BTreeSet<_>>().len() != keys.len()
        {
            return Err(StreamError::InvalidResource);
        }
        let mut active = active()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if keys.iter().any(|key| active.contains(key)) {
            return Err(StreamError::ResourceBusy);
        }
        if active.len().saturating_add(keys.len()) > MAX_ACTIVE_KEYS {
            return Err(StreamError::Capacity);
        }
        active.extend(keys.iter().cloned());
        Ok(Self(keys))
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        let mut active = active()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for key in &self.0 {
            active.remove(key);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn key(namespace: &str, part: &str) -> ExclusiveResource {
        ExclusiveResource::new(namespace, vec![part.into()]).unwrap()
    }
    #[test]
    fn conflict_is_atomic_and_lexical_keys_do_not_collide() {
        let namespace = uuid::Uuid::new_v4().to_string();
        let held = Reservation::acquire(vec![key(&namespace, "a")]).unwrap();
        assert!(matches!(
            Reservation::acquire(vec![key(&namespace, "b"), key(&namespace, "a")]),
            Err(StreamError::ResourceBusy)
        ));
        let independent = Reservation::acquire(vec![key(&namespace, "b")]).unwrap();
        assert!(matches!(
            Reservation::acquire(vec![key(&namespace, "c"), key(&namespace, "c")]),
            Err(StreamError::InvalidResource)
        ));
        drop((held, independent));
        assert!(Reservation::acquire(vec![key(&namespace, "a"), key(&namespace, "b")]).is_ok());
        let a = ExclusiveResource::new(&namespace, vec!["a/b".into(), "c".into()]).unwrap();
        let b = ExclusiveResource::new(&namespace, vec!["a".into(), "b/c".into()]).unwrap();
        assert!(Reservation::acquire(vec![a, b]).is_ok());
    }
    #[test]
    fn key_bounds_and_namespace_are_checked_without_retaining_payloads() {
        for namespace in ["", "a/b", "a:b", "a\n", "ä"] {
            assert_eq!(
                ExclusiveResource::new(namespace, vec!["a".into()]),
                Err(StreamError::InvalidResource)
            );
        }
        for parts in [
            vec![],
            vec![" ".into()],
            vec!["a\0b".into()],
            vec!["a".repeat(MAX_RESOURCE_PART_BYTES + 1)],
            vec!["x".into(); 9],
            vec!["a".repeat(MAX_RESOURCE_PART_BYTES); 3],
        ] {
            assert_eq!(
                ExclusiveResource::new("resource", parts),
                Err(StreamError::InvalidResource)
            );
        }
        assert!(
            ExclusiveResource::new("resource", vec!["a".repeat(MAX_RESOURCE_PART_BYTES); 2])
                .is_ok()
        );
    }

    #[tokio::test]
    async fn producer_exit_does_not_release_a_resource_before_inflight_control_joins() {
        use super::super::{self as streams, *};
        use crate::{
            environments::InvocationAuthority,
            providers::Call,
            runtime::{Effect, ExecutionTraits, Runtime},
            source::SourceInput,
        };
        use tokio::sync::{mpsc, oneshot};
        use wes_core::{
            Data, Primitive, Shape,
            capability::{Capability, Safety},
            contracts::ContractRegistry,
        };
        struct Source {
            resource: ExclusiveResource,
            entered: mpsc::UnboundedSender<(StreamSink, oneshot::Sender<()>)>,
        }
        impl StreamingInvoker for Source {
            fn exclusive_resources(&self, _: &Call) -> Result<Vec<ExclusiveResource>, StreamError> {
                Ok(vec![self.resource.clone()])
            }
            fn subscribe(&self, _: Call, sink: StreamSink, _: CancellationToken) -> StreamFuture {
                sink.opened().unwrap();
                let (finish, receive) = oneshot::channel();
                self.entered.send((sink, finish)).unwrap();
                Box::pin(async move {
                    receive.await.unwrap();
                    Ok(())
                })
            }
        }
        struct Controller(mpsc::UnboundedSender<oneshot::Sender<()>>);
        impl LiveController for Controller {
            fn authorize(&self, _: &InvocationAuthority, _: &Value) -> bool {
                true
            }
            fn invoke(&self, _: Value, _: CancellationToken) -> ControlFuture {
                let (finish, receive) = oneshot::channel();
                self.0.send(finish).unwrap();
                Box::pin(async move {
                    receive.await.unwrap();
                    ControlOutcome::Unknown
                })
            }
        }
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
        let run = runtime
            .start(Duration::ZERO)
            .into_iter()
            .find_map(|effect| match effect {
                Effect::Spawn(ticket) => Some(ticket.run),
                _ => None,
            })
            .unwrap();
        let principal = InvocationAuthority::from_source(
            &SourceInput::new("fixture".into(), "source".into()).unwrap(),
        );
        let mut capability =
            Capability::new(["watch"], Shape::Primitive(Primitive::Int), Safety::Unsafe);
        capability.streaming = true;
        let call = Call {
            authority: principal.clone(),
            run: run.clone(),
            capability: Arc::new(capability),
            arguments: Default::default(),
        };
        let (entered, mut received) = mpsc::unbounded_channel();
        let provider = Arc::new(Source {
            resource: key(&uuid::Uuid::new_v4().to_string(), "interface"),
            entered,
        });
        let start = || {
            streams::spawn(
                call.clone(),
                provider.clone(),
                Provenance::default(),
                Limits::default(),
                CancellationToken::new(),
            )
        };
        let (handle, task) = start().unwrap();
        let (sink, finish) = received.recv().await.unwrap();
        let contract = ContractRegistry::new().resolve("Int").unwrap();
        let (control_entered, mut controls_received) = mpsc::unbounded_channel();
        sink.install_controls(
            contract.clone(),
            contract,
            ControlLimits::default(),
            Arc::new(Controller(control_entered)),
        )
        .unwrap();
        let controls = handle.live_controls(&run, principal).unwrap();
        let action = Value::new(
            Shape::Primitive(Primitive::Int),
            Data::Int(1),
            Provenance::default(),
        )
        .unwrap();
        let action = tokio::spawn(async move {
            controls
                .invoke(action, Duration::from_secs(5), CancellationToken::new())
                .await
        });
        let control_end = controls_received.recv().await.unwrap();
        finish.send(()).unwrap();
        let mut changes = handle.subscribe();
        tokio::time::timeout(Duration::from_secs(5), async {
            while changes.borrow_and_update().phase != Phase::Closing {
                changes.changed().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert!(matches!(start(), Err(StreamError::ResourceBusy)));
        assert!(received.try_recv().is_err());
        control_end.send(()).unwrap();
        assert!(action.await.unwrap().is_err());
        task.join().await.unwrap();
        let (_, next_task) = start().unwrap();
        received.recv().await.unwrap().1.send(()).unwrap();
        next_task.join().await.unwrap();
    }
}
