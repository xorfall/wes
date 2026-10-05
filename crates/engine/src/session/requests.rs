//! Shared, transport-independent request identity. Disk waits stay off the session actor.
use super::{RecordingMode, SessionHandle, SubmissionReply};
use crate::{
    history::{Persistence, RecordError, RequestClaim, RequestRecord, RequiredPersistence},
    recording::Recorder,
    source::SourceInput,
};
use indexmap::IndexMap;
use sha2::{Digest, Sha256};
use std::sync::{Arc, Mutex, Weak};
use tokio::sync::Semaphore;
mod workflows;
pub use workflows::SequentialState;
use workflows::Workflow;

pub(super) struct Requests {
    backend: Backend,
    credit: Arc<Semaphore>,
    /// Verified immutable identities, not execution grants or successful-submission receipts.
    /// Keep control/read requests for recent work independent of the recording queue.
    known: Mutex<IndexMap<(String, String), RequestRecord>>,
    workflows: Mutex<IndexMap<String, Workflow>>,
    workflow_credit: Arc<Semaphore>,
}
const KNOWN_REQUESTS: usize = 10_000;
enum Backend {
    Recorded(Weak<Recorder>, RequiredPersistence),
    Ephemeral(Mutex<IndexMap<(String, String), RequestRecord>>),
}
#[derive(Debug)]
pub struct RequestedSubmission {
    pub claim: RequestClaim,
    /// Present for a new admission only. Its failure never forgets the reservation.
    pub submission: Option<SubmissionReply>,
}
impl Requests {
    pub(super) fn new(mode: &RecordingMode) -> Self {
        Self {
            backend: match mode {
                RecordingMode::Ephemeral => Backend::Ephemeral(Default::default()),
                RecordingMode::Required(journal) => {
                    Backend::Recorded(journal.weak_recorder(), journal.recording().1)
                }
            },
            credit: Arc::new(Semaphore::new(16)),
            known: Default::default(),
            workflows: Default::default(),
            workflow_credit: Arc::new(Semaphore::new(4)),
        }
    }
    fn remember(&self, record: &RequestRecord) {
        let mut known = self.known.lock().expect("verified request identities");
        let key = (record.namespace.clone(), record.request.clone());
        known.shift_remove(&key);
        if known.len() == KNOWN_REQUESTS {
            known.shift_remove_index(0);
        }
        known.insert(key, record.clone());
    }
    fn known(&self, namespace: &str, request: &str) -> Option<RequestRecord> {
        let mut known = self.known.lock().expect("verified request identities");
        let key = (namespace.to_owned(), request.to_owned());
        let record = known.shift_remove(&key)?;
        known.insert(key, record.clone());
        Some(record)
    }
    async fn claim(
        &self,
        record: RequestRecord,
        current: bool,
    ) -> Result<RequestClaim, RecordError> {
        match &self.backend {
            Backend::Recorded(recorder, required) => {
                let claim = recorder
                    .upgrade()
                    .ok_or(RecordError::Closed)?
                    .claim_request(record, current)
                    .await?;
                if !required.accepts(claim.persistence) {
                    return Err(RecordError::Poisoned);
                }
                Ok(claim)
            }
            Backend::Ephemeral(entries) => {
                let mut entries = entries.lock().expect("request identities");
                let key = (record.namespace.clone(), record.request.clone());
                if let Some(previous) = entries.get(&key) {
                    if !previous.matches(&record) {
                        return Err(RecordError::RequestConflict);
                    }
                    return Ok(RequestClaim {
                        record: previous.clone(),
                        fresh: false,
                        persistence: Persistence::Volatile,
                    });
                }
                if !current {
                    return Err(RecordError::RequestContext);
                }
                if entries.len() >= 10_000 {
                    return Err(RecordError::Limit("ephemeral request identities"));
                }
                entries.insert(key, record.clone());
                Ok(RequestClaim {
                    record,
                    fresh: true,
                    persistence: Persistence::Volatile,
                })
            }
        }
    }
}
impl SessionHandle {
    /// The trusted caller binds the opaque context token to its current workspace context.
    /// On an existing key, that context is compared with the original fingerprint instead.
    pub async fn submit_request(
        &self,
        namespace: String,
        request: String,
        context: String,
        current: bool,
        input: SourceInput,
    ) -> Result<RequestedSubmission, RecordError> {
        self.submit_request_in(namespace, request, context, current, input, None)
            .await
    }
    /// An explicit finite workflow shares durable identity and admission with ordinary requests.
    pub async fn submit_sequential_request(
        &self,
        namespace: String,
        request: String,
        context: String,
        current: bool,
        input: SourceInput,
        stop: crate::driver::CancellationToken,
    ) -> Result<RequestedSubmission, RecordError> {
        self.submit_request_in(namespace, request, context, current, input, Some(stop))
            .await
    }
    async fn submit_request_in(
        &self,
        namespace: String,
        request: String,
        context: String,
        current: bool,
        input: SourceInput,
        sequential: Option<crate::driver::CancellationToken>,
    ) -> Result<RequestedSubmission, RecordError> {
        if context.len() > 256 || context.chars().any(char::is_control) {
            return Err(RecordError::Limit("request context"));
        }
        let spans = if sequential.is_some() {
            let spans = input
                .statement_spans()
                .map_err(|_| RecordError::Limit("invalid sequential source"))?;
            if spans.is_empty()
                || spans.len() > 64
                || input.repeat().is_some()
                || input.revision_of().is_some()
            {
                return Err(RecordError::Limit("sequential workflow steps"));
            }
            Some(spans)
        } else {
            None
        };
        let steps: Vec<_> = spans
            .as_ref()
            .map(|spans| {
                spans
                    .iter()
                    .enumerate()
                    .map(|(i, _)| {
                        if i == 0 {
                            input.cell().to_owned()
                        } else {
                            format!("{}/step/{i}", input.cell())
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        let record = RequestRecord {
            namespace,
            request,
            cell: input.cell().into(),
            fingerprint: fingerprint(&input, &context, sequential.is_some()),
            steps,
        };
        record
            .validate()
            .map_err(|_| RecordError::Limit("request identity"))?;
        let credit = self
            .requests
            .credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| RecordError::ReadBusy)?;
        if self.sources.is_closed() {
            return Err(RecordError::Closed);
        }
        let workflow_credit = if sequential.is_some() {
            Some(
                self.requests
                    .workflow_credit
                    .clone()
                    .try_acquire_owned()
                    .map_err(|_| RecordError::ReadBusy)?,
            )
        } else {
            None
        };
        let session = self.clone();
        // Ownership transfers before the first await: a cancelled transport cannot strand a
        // successful reservation merely by dropping its wait. Process crashes still fail closed.
        tokio::spawn(async move {
            let _credit = credit;
            let sandbox = session
                .sandboxes
                .routes(&input)
                .await
                .map_err(|_| RecordError::ReadBusy)?;
            if sandbox && sequential.is_some() {
                return Err(RecordError::Limit(
                    "sequential workflows cannot contain sandbox work",
                ));
            }
            let requests = if sandbox {
                &session.sandbox_requests
            } else {
                &session.requests
            };
            let claim = requests.claim(record, current).await?;
            requests.remember(&claim.record);
            let submission = if claim.fresh
                && let Some(spans) = spans
            {
                let stop = sequential.expect("sequential spans");
                let inputs = spans
                    .into_iter()
                    .zip(&claim.record.steps)
                    .map(|(span, cell)| input.workflow_step(cell.clone(), span))
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(|_| RecordError::Limit("sequential workflow source"))?;
                session.start_workflow(
                    &claim.record,
                    inputs,
                    stop,
                    workflow_credit.expect("workflow credit"),
                )?;
                None
            } else if claim.fresh {
                Some(session.submit(input).await)
            } else {
                None
            };
            Ok(RequestedSubmission { claim, submission })
        })
        .await
        .map_err(|_| RecordError::Closed)?
    }
    pub async fn find_request(
        &self,
        actor: &str,
        request: &str,
    ) -> Result<Option<RequestRecord>, RecordError> {
        let probe = RequestRecord {
            namespace: actor.into(),
            request: request.into(),
            cell: "lookup".into(),
            fingerprint: "0".repeat(64),
            steps: Vec::new(),
        };
        probe
            .validate()
            .map_err(|_| RecordError::Limit("request identity"))?;
        let _credit = self
            .requests
            .credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| RecordError::ReadBusy)?;
        if self.sources.is_closed() {
            return Err(RecordError::Closed);
        }
        if let Some(record) = self
            .sandbox_requests
            .known(actor, request)
            .or_else(|| self.requests.known(actor, request))
        {
            return Ok(Some(record));
        }
        if let Backend::Ephemeral(entries) = &self.sandbox_requests.backend {
            if let Some(record) = entries
                .lock()
                .expect("sandbox request identities")
                .get(&(actor.into(), request.into()))
                .cloned()
            {
                return Ok(Some(record));
            }
        }
        let found = match &self.requests.backend {
            Backend::Recorded(recorder, _) => {
                recorder
                    .upgrade()
                    .ok_or(RecordError::Closed)?
                    .find_request(actor, request)
                    .await
            }
            Backend::Ephemeral(entries) => Ok(entries
                .lock()
                .expect("request identities")
                .get(&(actor.into(), request.into()))
                .cloned()),
        }?;
        if let Some(record) = &found {
            self.requests.remember(record);
        }
        Ok(found)
    }
}
fn fingerprint(input: &SourceInput, context: &str, sequential: bool) -> String {
    let mut hash = Sha256::new();
    hash.update(b"wes.request.v1");
    let mut field = |value: Option<&str>| {
        hash.update([u8::from(value.is_some())]);
        if let Some(value) = value {
            hash.update((value.len() as u64).to_le_bytes());
            hash.update(value);
        }
    };
    field(Some(context));
    field(Some(input.text()));
    field(input.document());
    field(input.revision_of());
    field(input.repeat().map(|r| r.origin.as_str()));
    field(
        input
            .repeat()
            .and_then(|r| r.from.as_ref())
            .map(|n| n.as_str()),
    );
    hash.update([
        u8::from(input.is_cooperative()),
        u8::from(input.is_reactive()),
        u8::from(input.repeat().is_some_and(|r| r.acknowledge_effects)),
    ]);
    if sequential {
        hash.update(b"sequential.v1");
    }
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn verified_identities_are_bounded_namespaced_and_do_not_change_claim_authority() {
        let requests = Requests::new(&RecordingMode::Ephemeral);
        for n in 0..KNOWN_REQUESTS {
            requests.remember(&RequestRecord {
                namespace: "pane".into(),
                request: n.to_string(),
                cell: n.to_string(),
                fingerprint: "0".repeat(64),
                steps: Vec::new(),
            });
        }
        assert_eq!(requests.known("pane", "0").unwrap().cell, "0");
        let other = RequestRecord {
            namespace: "other".into(),
            request: "0".into(),
            cell: "other-cell".into(),
            fingerprint: "1".repeat(64),
            steps: Vec::new(),
        };
        requests.remember(&other);
        assert_eq!(requests.known.lock().unwrap().len(), KNOWN_REQUESTS);
        assert!(requests.known("pane", "1").is_none());
        assert_eq!(requests.known("pane", "0").unwrap().cell, "0");
        assert_eq!(requests.known("other", "0").unwrap(), other);
        assert!(requests.known("unknown", "0").is_none());
    }
}
