//! Single-owner source admission, finite execution and joined observation logging.
use crate::{
    calls::CallJournal,
    driver::{
        CancellationToken, DriverError, ExecutionIo, ExecutionNotice, Snapshot,
        waiting::{self, Waiter, Waiters},
    },
    graph::{NodeId, OutputRef},
    log::PreparedLog,
    runtime::{Effect, Observation},
    source::{
        self, ParsedSource, PreparedSource, SourceDiagnostics, SourceError, SourceInput,
        SourcePreparation,
    },
    tasks::{BoundTask, TaskExecutor},
    type_sources::{TypeSourceCapture, TypeSourceReader},
    workspace::{Preparation, Workspace, WorkspaceError},
};
use indexmap::IndexMap;
use std::{num::NonZeroUsize, sync::Arc};
use thiserror::Error;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore, broadcast, mpsc, oneshot},
    task::{JoinHandle, JoinSet},
};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};
mod access;
mod display;
mod environments;
mod import_plans;
mod observation;
mod observation_query;
mod view_observation;
mod view_queries;
pub use display::DisplaySample;
pub use observation_query::{ObservationQuery, PreparedObservationQuery};
mod queries;
pub mod sandbox;
pub use environments::{
    EnvironmentAuthentication, EnvironmentAuthorityCommand, EnvironmentPlan,
    EnvironmentPublication, ProviderAuthorityCommand,
};
pub use observation::{ObservedCell, SessionObservation};
mod checkpoint;
mod identity;
mod repeat;
pub mod retirement;
use checkpoint::Checkpoints;
pub use checkpoint::{SessionCheckpoint, SessionPause};
mod log;
mod restore;
pub(crate) mod values;
mod workspaces;
use identity::Cells;
mod requests;
pub use log::LogSnapshot;
use log::SessionLog;
pub use requests::{RequestedSubmission, SequentialState};
pub use restore::{
    RestoreError, RestoreProblem, RestoreProblemKind, RestoreReport, RestoredSession, restore,
};
use values::SessionValues;
pub use values::{
    PinBinding, PublishedValue, RecoveredValue, RetainedAcknowledgement, RetentionPolicy,
    SessionStorage, StorageCommand, StorageReceipt, StorageReply, ValuePublication, ValueSnapshot,
};
pub mod management;
pub use workspaces::{
    WorkspaceAction, WorkspaceActionError, WorkspaceActions, WorkspaceOperation, WorkspaceRequest,
};

#[derive(Clone)]
pub enum RecordingMode {
    Ephemeral,
    Required(CallJournal),
}
#[derive(Clone, Debug)]
pub struct SubmissionResult {
    /// Factual application receipts; absent on reconstruction, never proof of completed work.
    pub receipts: Vec<crate::workspace::OperationReceipt>,
    /// Transient object observation; never a node, cell or retained result.
    pub sandbox: Option<sandbox::Reply>,
    pub cell: String,
    pub nodes: Vec<NodeId>,
    /// Existing nodes explicitly requested by refresh; never declaration ownership.
    pub refreshed: Vec<NodeId>,
    pub accepted: Vec<Span>,
    pub removed: Vec<NodeId>,
    pub unbound: Vec<String>,
    pub diagnostics: SourceDiagnostics,
    /// Accepted source belongs in replay history; this flag alone is not a durability receipt.
    pub recorded: bool,
    /// Reconstructed identity, not the original transient reply or proof of execution.
    pub restored: bool,
    /// Exact fresh run accepted by a typed repeat, not the node's later current run.
    pub repeated_run: Option<crate::runtime::RunId>,
}
#[derive(Clone, Debug, Error)]
pub enum SessionError {
    #[error("CMD001: {0}")]
    CommandArgument(&'static str),
    #[error("{0}")]
    Management(String),
    #[error("{0}")]
    Sandbox(String),
    /// The producer has rejected this attempt before installing or starting its work.
    /// Other request failures make no claim about execution or external effects.
    #[error("{0}")]
    AdmissionRefused(String),
    #[error("{0}")]
    RepeatRefused(&'static str),
    #[error("another declaration admission is in progress; retry after it settles")]
    AdmissionBusy,
    #[error(transparent)]
    Environment(#[from] wes_core::environments::EnvironmentError),
    #[error("{0}")]
    Credential(#[from] crate::credentials::CredentialError),
    #[error("the session is no longer accepting work")]
    Stopped,
    #[error("this submission identity already belongs to different source text")]
    Conflict,
    #[error("the session submission or retained identity budget is full")]
    Capacity,
    #[error("this submission belongs to restored history; its original reply is unavailable")]
    HistoricalReplyUnavailable,
    #[error("this session has no configured value store")]
    NoStorage,
    #[error("this session has no configured history recorder")]
    NoHistory,
    #[error("a workspace checkpoint or switch is in progress")]
    CheckpointBusy,
    #[error("this node is not in the current session")]
    UnknownNode,
    #[error("this value handle is not a current session output")]
    UnknownValue,
    /// Payload-independent authority explanation from the shared access boundary.
    #[error("{0}")]
    AccessDenied(String),
    #[error(
        "Operation is outside this actor's authority. Ask the user to review the required scope through wes's host controls."
    )]
    Authority,
    #[error("submission preparation failed before installation")]
    Preparation,
    #[error("submission preparation was cancelled before installation")]
    Cancelled,
    #[error(
        "required history acknowledgement failed; the submission was not installed and recording may be incomplete"
    )]
    Recording,
    #[error("the prepared submission could not be installed")]
    Commit,
    #[error(transparent)]
    Driver(#[from] DriverError),
}
impl SessionError {
    /// Preserve only producer-approved public explanations, never diagnostic subjects.
    fn from_access(error: WorkspaceError) -> Self {
        if let WorkspaceError::Rejected { diagnostics, .. } = error
            && let Some(diagnostic) = diagnostics.iter().find(|d| d.code == "AUT001")
        {
            return Self::AccessDenied(diagnostic.public_summary().to_owned());
        }
        Self::Authority
    }
    pub fn authority_message(&self) -> Option<String> {
        match self {
            Self::Authority | Self::AccessDenied(_) => Some(self.to_string()),
            _ => None,
        }
    }
}
pub type SubmissionReply = Result<Arc<SubmissionResult>, SessionError>;
#[derive(Clone, Debug)]
pub struct SessionSnapshot {
    pub execution: Snapshot<BoundTask>,
    pub names: IndexMap<String, OutputRef>,
    /// Current run-bound conversation ports, without input or transient transcript data.
    pub conversations: IndexMap<NodeId, crate::runtime::RunId>,
    pub admission_pending: bool,
    pub checkpoint_pending: bool,
    pub recording_blocked: bool,
    /// Startup evidence, not a continuously recomputed claim about remote transactions.
    pub restoration: Option<Arc<RestoreReport>>,
}
struct Submission<T> {
    source: T,
    reply: oneshot::Sender<SubmissionReply>,
    credit: OwnedSemaphorePermit,
}
enum Control {
    ResolveManagement {
        input: SourceInput,
        reference: String,
        reply: oneshot::Sender<(bool, Result<String, SessionError>)>,
    },
    CompleteManagement {
        cell: String,
        result: SubmissionReply,
        reply: oneshot::Sender<()>,
    },
    StopWorkspace {
        environments: std::collections::BTreeMap<String, wes_core::environments::Revision>,
        cells: Vec<String>,
        stop: bool,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    SandboxRead {
        member: Option<String>,
        inspect: bool,
        cooperative: bool,
        reply: oneshot::Sender<Result<wes_core::Data, SessionError>>,
    },
    SandboxWorkspace {
        input: SourceInput,
        name: Option<String>,
        reply: oneshot::Sender<Result<Workspace, SessionError>>,
    },
    CheckRetirementAccess(oneshot::Sender<Result<(), SessionError>>),
    FenceRetirement(Arc<()>, oneshot::Sender<Result<(), SessionError>>),
    Retire(
        retirement::RetirementPlan,
        Arc<crate::history::HistoryImage>,
        Arc<()>,
        oneshot::Sender<Result<(), SessionError>>,
    ),
    Repeat(Box<Submission<SourceInput>>),
    Environments(Box<environments::Request>),
    Input {
        node: NodeId,
        run: crate::runtime::RunId,
        bytes: Option<Vec<u8>>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Cancel(NodeId, oneshot::Sender<Result<(), SessionError>>),
    ObserveActor {
        actor: String,
        inherit: String,
        environment: Option<String>,
        reply: oneshot::Sender<Result<SessionObservation, SessionError>>,
    },
    GrantWork {
        actor: String,
        cells: Vec<String>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    GrantSources {
        actor: String,
        cells: Vec<String>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    ReadSource {
        actor: String,
        cell: String,
        reply: oneshot::Sender<Result<Option<String>, SessionError>>,
    },
    CancelActor {
        actor: String,
        nodes: Vec<NodeId>,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    CancelWork(String, oneshot::Sender<Result<(), SessionError>>),
    CancelActorWork {
        actor: String,
        cell: String,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Observe(oneshot::Sender<SessionObservation>),
    DisplayValue(NodeId, oneshot::Sender<Result<DisplaySample, SessionError>>),
    ViewFrame(
        NodeId,
        Option<(String, String)>,
        oneshot::Sender<Result<crate::views::Frame, SessionError>>,
    ),
    ViewMount(
        NodeId,
        String,
        crate::views::MountAction,
        oneshot::Sender<Result<Option<String>, SessionError>>,
    ),
    ViewCatalogue(oneshot::Sender<Result<wes_views::Catalogue, SessionError>>),
    ViewInputs(
        NodeId,
        String,
        oneshot::Sender<Result<crate::views::InputPatches, SessionError>>,
    ),
    ViewInteraction {
        node: NodeId,
        identity: String,
        edit: Option<crate::views::InteractionEdit>,
        reply: oneshot::Sender<
            Result<(crate::views::Snapshot, crate::views::InteractionState), SessionError>,
        >,
    },
    ImportedSpecs(oneshot::Sender<Vec<crate::workspace::ImportedSpec>>),
    Checkpoint(oneshot::Sender<Result<SessionCheckpoint, SessionError>>),
    Storage(
        StorageCommand,
        crate::storage::ValueHandle,
        oneshot::Sender<StorageReply>,
    ),
    ReleaseAtCheckpoint(
        crate::storage::ValueHandle,
        Arc<()>,
        oneshot::Sender<StorageReply>,
    ),
    RetentionPolicy(RetentionPolicy, oneshot::Sender<Result<(), SessionError>>),
    Submit(Box<Submission<SourcePreparation<ParsedSource>>>),
    Snapshot(oneshot::Sender<SessionSnapshot>),
    Log(oneshot::Sender<LogSnapshot>),
    Values(oneshot::Sender<Option<ValueSnapshot>>),
    WaitIdle(oneshot::Sender<Result<(), DriverError>>),
    Shutdown(oneshot::Sender<()>),
}
impl Control {
    fn environment(request: environments::Request) -> Self {
        Self::Environments(Box::new(request))
    }
}
#[derive(Clone)]
pub struct SessionHandle {
    management: Arc<management::Requests>,
    workspace_management: Option<WorkspaceActions>,
    remote_uncertain: Arc<std::sync::atomic::AtomicBool>,
    sandboxes: Arc<sandbox::Sandboxes>,
    requests: Arc<requests::Requests>,
    sandbox_requests: Arc<requests::Requests>,
    environment_credit: Arc<Semaphore>,
    history_reader: Option<std::sync::Weak<crate::recording::Recorder>>,
    sources: mpsc::Sender<Submission<ParsedSource>>,
    controls: mpsc::Sender<Control>,
    parsers: Arc<Semaphore>,
    ingress: Arc<Semaphore>,
    source_credit: Arc<Semaphore>,
    control_credit: Arc<Semaphore>,
    events: broadcast::WeakSender<Observation>,
    notices: broadcast::WeakSender<ExecutionNotice>,
    conversations: broadcast::WeakSender<Arc<crate::driver::ConversationEvent>>,
    updates: broadcast::WeakSender<()>,
    log_updates: broadcast::WeakSender<()>,
    value_updates: Option<broadcast::WeakSender<()>>,
}
impl SessionHandle {
    pub fn remote_outcome_uncertain(&self) -> bool {
        self.remote_uncertain
            .load(std::sync::atomic::Ordering::Acquire)
    }
    /// Capacity-refusing variant for transports; a retained page never makes shutdown wait for admission.
    pub async fn try_history_page(
        &self,
        cursor: Option<crate::history::HistoryCursor>,
        limits: crate::history::HistoryPageLimits,
    ) -> Result<crate::recording::CapturedPage, crate::history::RecordError> {
        if self.sources.is_closed() {
            return Err(crate::history::RecordError::Closed);
        }
        let recorder = self
            .history_reader
            .as_ref()
            .ok_or(crate::history::RecordError::PageUnsupported)?
            .upgrade()
            .ok_or(crate::history::RecordError::Closed)?;
        recorder.try_page(cursor, limits).await
    }
    /// Read saved Log entries without source admission, runtime work or a session checkpoint pause.
    /// The recording owner joins admitted reads, including abandoned waits; this weak client port
    /// never prolongs a closed session's writer lifetime. A returned page owns its response credit.
    pub async fn history_page(
        &self,
        cursor: Option<crate::history::HistoryCursor>,
        limits: crate::history::HistoryPageLimits,
    ) -> Result<crate::recording::CapturedPage, crate::history::RecordError> {
        if self.sources.is_closed() {
            return Err(crate::history::RecordError::Closed);
        }
        let recorder = self
            .history_reader
            .as_ref()
            .ok_or(crate::history::RecordError::PageUnsupported)?
            .upgrade()
            .ok_or(crate::history::RecordError::Closed)?;
        recorder.page(cursor, limits).await
    }
    pub async fn input(
        &self,
        node: NodeId,
        run: crate::runtime::RunId,
        bytes: &[u8],
    ) -> Result<(), SessionError> {
        self.conversation_input(node, run, Some(bytes)).await
    }
    pub async fn eof(&self, node: NodeId, run: crate::runtime::RunId) -> Result<(), SessionError> {
        self.conversation_input(node, run, None).await
    }
    async fn conversation_input(
        &self,
        node: NodeId,
        run: crate::runtime::RunId,
        bytes: Option<&[u8]>,
    ) -> Result<(), SessionError> {
        if bytes.is_some_and(|bytes| bytes.len() > crate::conversations::input_bytes()) {
            return Err(DriverError::Conversation(
                crate::conversations::ConversationError::Capacity,
            )
            .into());
        }
        // Reserve the bounded control queue before copying private bytes. Input cannot become a
        // recorded submission, even while a workspace checkpoint pauses source admission.
        let permit = self
            .controls
            .reserve()
            .await
            .map_err(|_| SessionError::Stopped)?;
        let (reply, receive) = oneshot::channel();
        permit.send(Control::Input {
            node,
            run,
            bytes: bytes.map(<[u8]>::to_vec),
            reply,
        });
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub fn subscribe_conversations(
        &self,
    ) -> Result<broadcast::Receiver<Arc<crate::driver::ConversationEvent>>, SessionError> {
        self.conversations
            .upgrade()
            .map(|sender| sender.subscribe())
            .ok_or(SessionError::Stopped)
    }
    /// Typed client cancellation; node text is never reparsed as command source.
    pub async fn cancel(&self, node: NodeId) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Cancel(node, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn cancel_work(&self, origin: String) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::CancelWork(origin, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Subscribe before observing. Payload-free wakeups may coalesce; observe again after lag.
    pub fn subscribe_updates(&self) -> Result<broadcast::Receiver<()>, SessionError> {
        self.updates
            .upgrade()
            .map(|sender| sender.subscribe())
            .ok_or(SessionError::Stopped)
    }
    /// A single owner turn captures all client-visible state without running work or reading files.
    pub async fn observe(&self) -> Result<SessionObservation, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Observe(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    /// A demand-driven public display sample. No storage, history cloning, or execution.
    /// The sample is an observation, never a promise that its run remains the current run.
    pub async fn display_value(&self, node: NodeId) -> Result<DisplaySample, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::DisplayValue(node, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn view_catalogue(&self) -> Result<wes_views::Catalogue, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewCatalogue(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn view_input_patches(
        &self,
        node: NodeId,
        identity: String,
    ) -> Result<crate::views::InputPatches, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewInputs(node, identity, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn view_interaction(
        &self,
        node: NodeId,
        identity: String,
        edit: Option<crate::views::InteractionEdit>,
    ) -> Result<(crate::views::Snapshot, crate::views::InteractionState), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewInteraction {
                node,
                identity,
                edit,
                reply,
            })
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn view_mount(
        &self,
        node: NodeId,
        identity: String,
        action: crate::views::MountAction,
    ) -> Result<Option<String>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewMount(node, identity, action, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn observed_view_frame(
        &self,
        node: NodeId,
        identity: String,
        mount: String,
    ) -> Result<crate::views::Frame, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewFrame(node, Some((identity, mount)), reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn view_frame(&self, node: NodeId) -> Result<crate::views::Frame, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ViewFrame(node, None, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// User inspection of retained spec inputs; no execution, source reads or agent grant.
    pub async fn imported_specs(
        &self,
    ) -> Result<Vec<crate::workspace::ImportedSpec>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ImportedSpecs(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    /// Stop starting new submissions, settle admitted finite work and capture both history streams.
    /// Dropping the returned checkpoint resumes admission. While settling, cancel remains available;
    /// snapshots and shutdown always remain responsive. A disconnected request releases the pause
    /// after any entered capture has physically completed. The composition root still owns writers.
    pub async fn checkpoint(&self) -> Result<SessionCheckpoint, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Checkpoint(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }

    pub async fn keep(&self, handle: crate::storage::ValueHandle) -> StorageReply {
        self.storage_command(StorageCommand::Keep, handle).await
    }
    pub async fn release(&self, handle: crate::storage::ValueHandle) -> StorageReply {
        self.storage_command(StorageCommand::Release, handle).await
    }
    /// Only the owner holding this session's live pause may release after impact
    /// revalidation. Unforgeable in-process capability; never a transport token.
    pub async fn release_at_checkpoint(
        &self,
        handle: crate::storage::ValueHandle,
        checkpoint: &SessionCheckpoint,
    ) -> StorageReply {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::ReleaseAtCheckpoint(
                handle,
                checkpoint.authority(),
                reply,
            ))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    async fn storage_command(
        &self,
        command: StorageCommand,
        handle: crate::storage::ValueHandle,
    ) -> StorageReply {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Storage(command, handle, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Affects future Ready observations only. Never revisits already queued/historical outputs.
    pub async fn set_retention_policy(&self, policy: RetentionPolicy) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::RetentionPolicy(policy, reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// None means this session was explicitly composed without a value store.
    pub async fn values(&self) -> Result<Option<ValueSnapshot>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Values(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    pub fn subscribe_values(&self) -> Result<Option<broadcast::Receiver<()>>, SessionError> {
        self.value_updates
            .as_ref()
            .map(|updates| {
                updates
                    .upgrade()
                    .map(|sender| sender.subscribe())
                    .ok_or(SessionError::Stopped)
            })
            .transpose()
    }
    /// Subscribe before fetching the snapshot. Wakeups may coalesce; refetch after lag too.
    /// No log payload is retained in this notification channel.
    pub fn subscribe_log(&self) -> Result<broadcast::Receiver<()>, SessionError> {
        self.log_updates
            .upgrade()
            .map(|sender| sender.subscribe())
            .ok_or(SessionError::Stopped)
    }
    pub async fn log(&self) -> Result<LogSnapshot, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Log(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    /// Before enqueue the caller owns the input. After enqueue, disconnect does not retract work.
    /// Syntax classification has a separate bounded pool so immediate actions bypass disk admission.
    pub async fn submit(&self, input: SourceInput) -> SubmissionReply {
        if self.sources.is_closed() {
            return Err(SessionError::Stopped);
        }
        if input.repeat().is_some() {
            let cost = u32::try_from(
                input
                    .text()
                    .len()
                    .saturating_mul(6)
                    .saturating_add(input.context_charge())
                    .saturating_add(8192),
            )
            .map_err(|_| SessionError::Capacity)?;
            let credit = self
                .control_credit
                .clone()
                .try_acquire_many_owned(cost)
                .map_err(|_| SessionError::Capacity)?;
            let (reply, receive) = oneshot::channel();
            self.controls
                .send(Control::Repeat(Box::new(Submission {
                    source: input,
                    reply,
                    credit,
                })))
                .await
                .map_err(|_| SessionError::Stopped)?;
            return receive.await.map_err(|_| SessionError::Stopped)?;
        }
        // Account for source, tokens/AST, diagnostics and collection overhead conservatively.
        // Credit follows the physical parser and queued/active source even if the caller leaves.
        let cost = u32::try_from(
            input
                .text()
                .len()
                .saturating_mul(128)
                .saturating_add(input.context_charge())
                .saturating_add(4096),
        )
        .map_err(|_| SessionError::Capacity)?;
        let credit = self
            .ingress
            .clone()
            .acquire_many_owned(cost)
            .await
            .map_err(|_| SessionError::Stopped)?;
        let permit = self
            .parsers
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| SessionError::Stopped)?;
        let (source, credit) = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            (source::preflight(input), credit)
        })
        .await
        .map_err(|_| SessionError::Preparation)?;
        let source = source.map_err(|error| match error {
            SourceError::Capacity => SessionError::Capacity,
            _ => SessionError::Preparation,
        })?;
        let input = match &source {
            SourcePreparation::Declarations(parsed) => parsed.input(),
            SourcePreparation::Immediate { input, .. }
            | SourcePreparation::Rejected { input, .. } => input,
        };
        if let Some(reply) = self.management.intercept(self, input, &source).await? {
            return reply;
        }
        if let Some(reply) = self.sandboxes.intercept(input, &source).await? {
            return reply;
        }
        // Transfer from analysis credit to the selected mailbox without holding parser capacity
        // while waiting behind an external write. A saturated declaration mailbox cannot exhaust
        // the separate immediate-control budget. Refusal here precedes session acceptance.
        let mailbox = match &source {
            SourcePreparation::Declarations(_) => &self.source_credit,
            _ => &self.control_credit,
        };
        let queued_credit = mailbox
            .clone()
            .try_acquire_many_owned(cost)
            .map_err(|_| SessionError::Capacity)?;
        drop(credit);
        let (reply, receive) = oneshot::channel();
        match source {
            SourcePreparation::Declarations(source) => self
                .sources
                .send(Submission {
                    source,
                    reply,
                    credit: queued_credit,
                })
                .await
                .map_err(|_| SessionError::Stopped)?,
            source => self
                .controls
                .send(Control::Submit(Box::new(Submission {
                    source,
                    reply,
                    credit: queued_credit,
                })))
                .await
                .map_err(|_| SessionError::Stopped)?,
        }
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn snapshot(&self) -> Result<SessionSnapshot, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Snapshot(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    pub async fn wait_idle(&self) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::WaitIdle(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive
            .await
            .map_err(|_| SessionError::Stopped)?
            .map_err(SessionError::from)
    }
    /// Acknowledges cancellation request, not physical worker or recording termination. Join next.
    pub async fn shutdown(&self) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::Shutdown(reply))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    pub fn subscribe(&self) -> Result<broadcast::Receiver<Observation>, SessionError> {
        self.events
            .upgrade()
            .map(|sender| sender.subscribe())
            .ok_or(SessionError::Stopped)
    }
    pub fn subscribe_notices(&self) -> Result<broadcast::Receiver<ExecutionNotice>, SessionError> {
        self.notices
            .upgrade()
            .map(|sender| sender.subscribe())
            .ok_or(SessionError::Stopped)
    }
}
pub struct SessionTask(JoinHandle<()>);
impl SessionTask {
    pub async fn join(self) -> Result<(), tokio::task::JoinError> {
        self.0.await
    }
}

pub fn spawn(
    workspace: Workspace,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    max_concurrent: NonZeroUsize,
) -> Result<(SessionHandle, SessionTask), SessionError> {
    spawn_configured(
        workspace,
        recording,
        type_reader,
        max_concurrent,
        None,
        None,
    )
}
/// The composition root shuts down and joins the store/writer only AFTER joining the session.
/// This finite boundary cannot auto-archive a streaming window or interactive transcript.
pub fn spawn_with_storage(
    workspace: Workspace,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    max_concurrent: NonZeroUsize,
    storage: SessionStorage,
) -> Result<(SessionHandle, SessionTask), SessionError> {
    spawn_configured(
        workspace,
        recording,
        type_reader,
        max_concurrent,
        Some(storage),
        None,
    )
}
/// Application composition with concrete named-workspace dispatch owned outside the session.
pub fn spawn_with_actions(
    workspace: Workspace,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    max_concurrent: NonZeroUsize,
    storage: Option<SessionStorage>,
    actions: WorkspaceActions,
) -> Result<(SessionHandle, SessionTask), SessionError> {
    spawn_configured(
        workspace,
        recording,
        type_reader,
        max_concurrent,
        storage,
        Some(actions),
    )
}
fn spawn_configured(
    workspace: Workspace,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    max_concurrent: NonZeroUsize,
    storage: Option<SessionStorage>,
    actions: Option<WorkspaceActions>,
) -> Result<(SessionHandle, SessionTask), SessionError> {
    let log = SessionLog::new(&recording);
    let values = storage.map(|storage| SessionValues::new(storage, &recording));
    spawn_owned(
        workspace,
        recording,
        type_reader,
        max_concurrent,
        SessionSeed {
            capacity: None,
            cells: Cells::default(),
            log,
            values,
            restoration: None,
            actions,
        },
    )
}
struct SessionSeed {
    capacity: Option<crate::driver::ExecutionCapacity>,
    actions: Option<WorkspaceActions>,
    cells: Cells,
    log: SessionLog,
    values: Option<SessionValues>,
    restoration: Option<Arc<RestoreReport>>,
}
fn spawn_owned(
    workspace: Workspace,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    max_concurrent: NonZeroUsize,
    seed: SessionSeed,
) -> Result<(SessionHandle, SessionTask), SessionError> {
    let SessionSeed {
        capacity,
        actions,
        cells,
        log,
        values,
        restoration,
    } = seed;
    if !workspace.runtime().is_drained() {
        return Err(DriverError::ActiveRuntime.into());
    }
    if workspace.runtime().is_closed() {
        return Err(SessionError::Stopped);
    }
    let capacity =
        capacity.map_or_else(|| crate::driver::ExecutionCapacity::new(max_concurrent), Ok)?;
    let executor = Arc::new(
        (match &recording {
            RecordingMode::Ephemeral => TaskExecutor::ephemeral(),
            RecordingMode::Required(journal) => TaskExecutor::recorded(journal.clone()),
        })
        .with_scan_memory(capacity.scan_memory.clone()),
    );
    let io = ExecutionIo::with_capacity(executor, capacity.clone())?;
    let (sources, source_receiver) = mpsc::channel(32);
    let (controls, control_receiver) = mpsc::channel(256);
    let updates = broadcast::channel(1).0;
    let remote_uncertain = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let sandboxes = Arc::new(sandbox::Sandboxes::new(
        remote_uncertain.clone(),
        !workspace.sandbox_runtime,
        workspace.sandbox_store.clone(),
        controls.clone(),
        type_reader.clone(),
        capacity.clone(),
        max_concurrent,
        updates.clone(),
    ));
    let management = Arc::new(management::Requests::default());
    let handle = SessionHandle {
        management: management.clone(),
        workspace_management: actions.clone(),
        remote_uncertain: remote_uncertain.clone(),
        sandboxes: sandboxes.clone(),
        requests: Arc::new(requests::Requests::new(&recording)),
        sandbox_requests: Arc::new(requests::Requests::new(&RecordingMode::Ephemeral)),
        environment_credit: Arc::new(Semaphore::new(2)),
        history_reader: match &recording {
            RecordingMode::Ephemeral => None,
            RecordingMode::Required(journal) => Some(journal.weak_recorder()),
        },
        updates: updates.downgrade(),
        sources,
        controls,
        parsers: Arc::new(Semaphore::new(8)),
        ingress: Arc::new(Semaphore::new(256 * 1024 * 1024)),
        source_credit: Arc::new(Semaphore::new(256 * 1024 * 1024)),
        control_credit: Arc::new(Semaphore::new(256 * 1024 * 1024)),
        events: io.events(),
        notices: io.notices(),
        conversations: io.conversations(),
        log_updates: log.updates(),
        value_updates: values.as_ref().map(SessionValues::updates),
    };
    let actor = Actor {
        import_plans: Default::default(),
        active_import: None,
        view_queries: view_queries::Queries::default(),
        query_capacity: capacity,
        displays: Default::default(),
        remote_uncertain,
        sandboxes,
        environment_credit: handle.environment_credit.clone(),
        node_owners: Default::default(),
        execution_principals: Default::default(),
        name_owners: Default::default(),
        work_grants: Default::default(),
        environment_clients: Default::default(),
        environment_plans: Default::default(),
        environment_work: JoinSet::new(),
        updates,
        workspace,
        io,
        sources: source_receiver,
        controls: control_receiver,
        recording,
        type_reader,
        admission: JoinSet::new(),
        active: None,
        reserved_output_names: Default::default(),
        preparation_token: CancellationToken::new(),
        waiters: Waiters::default(),
        waiting: JoinSet::new(),
        workspace_waiting: JoinSet::new(),
        workspace_actions: actions,
        cells,
        controls_open: true,
        sources_open: true,
        recording_failed: false,
        checkpoints: Checkpoints::default(),
        deferred_windows: vec![],
        final_retention: false,
        log,
        values,
        restoration,
    };
    Ok((handle, SessionTask(tokio::spawn(actor.run()))))
}
enum Admission {
    Prepared(Result<PreparedSource, SourceError>),
    Recorded(Result<PreparedSource, SessionError>),
}
struct Actor {
    import_plans: import_plans::Plans,
    active_import: Option<import_plans::FrozenImport>,
    view_queries: view_queries::Queries,
    query_capacity: crate::driver::ExecutionCapacity,
    displays: display::Displays,
    remote_uncertain: Arc<std::sync::atomic::AtomicBool>,
    sandboxes: Arc<sandbox::Sandboxes>,
    node_owners: std::collections::BTreeMap<NodeId, String>,
    execution_principals:
        std::collections::BTreeMap<NodeId, (crate::environments::InvocationAuthority, String)>,
    name_owners: std::collections::BTreeMap<String, (String, String)>,
    work_grants: std::collections::BTreeMap<String, access::WorkGrant>,
    environment_credit: Arc<Semaphore>,
    environment_clients:
        std::collections::BTreeMap<String, wes_core::environments::EnvironmentContext>,
    environment_plans: std::collections::BTreeMap<(String, String), EnvironmentPlan>,
    environment_work: JoinSet<environments::Completed>,
    updates: broadcast::Sender<()>,
    workspace: Workspace,
    io: ExecutionIo<BoundTask>,
    sources: mpsc::Receiver<Submission<ParsedSource>>,
    controls: mpsc::Receiver<Control>,
    recording: RecordingMode,
    type_reader: Arc<dyn TypeSourceReader>,
    admission: JoinSet<Admission>,
    active: Option<String>,
    reserved_output_names: std::collections::BTreeSet<String>,
    preparation_token: CancellationToken,
    waiters: Waiters,
    waiting: JoinSet<(String, SubmissionReply)>,
    workspace_waiting: JoinSet<(String, SubmissionReply)>,
    workspace_actions: Option<WorkspaceActions>,
    cells: Cells,
    controls_open: bool,
    sources_open: bool,
    recording_failed: bool,
    checkpoints: Checkpoints,
    deferred_windows: Vec<crate::driver::RuntimeInput<BoundTask>>,
    final_retention: bool,
    log: SessionLog,
    values: Option<SessionValues>,
    restoration: Option<Arc<RestoreReport>>,
}
impl Actor {
    async fn run(mut self) {
        if self.initialize_default_namespace().await.is_err() {
            self.recording_failed = true;
        }
        loop {
            // Cancellation/removal can revoke a deferred run while a checkpoint is settling. Ack
            // that update promptly so its worker can join; otherwise an idle barrier could deadlock.
            let deferred = std::mem::take(&mut self.deferred_windows);
            for input in deferred {
                if self.checkpoints.busy()
                    && !self.workspace.runtime().is_closed()
                    && input.deferrable(&self.workspace)
                {
                    self.deferred_windows.push(input);
                } else {
                    self.runtime_input(input);
                }
            }
            let settled = self.active.is_none()
                && self.environment_work.is_empty()
                && self.io.is_idle()
                && self.waiting.is_empty()
                && self.log.is_idle()
                && self.values.as_ref().is_none_or(SessionValues::is_idle);
            if settled
                && self.workspace.runtime().is_closed()
                && let Some(values) = self.values.as_mut()
            {
                values.retire_private_outputs();
            }
            let retain = if settled
                && !self.recording_failed
                && (self.checkpoints.pending()
                    || (self.workspace.runtime().is_closed() && !self.final_retention))
            {
                self.values.as_mut().map_or(
                    values::StreamRetention::Ready,
                    SessionValues::retain_stream_windows,
                )
            } else {
                values::StreamRetention::Ready
            };
            if settled
                && retain == values::StreamRetention::Ready
                && (self.checkpoints.pending()
                    || (self.workspace.runtime().is_closed() && !self.final_retention))
            {
                self.log.checkpoint_views(&self.workspace);
                self.log
                    .checkpoint_live(&self.workspace, self.values.as_ref());
                self.check_log();
            }
            if self.workspace.runtime().is_closed()
                && settled
                && (!self.log.live_pending()
                    || self.recording_failed
                    || retain == values::StreamRetention::Failed)
                && retain != values::StreamRetention::Pending
            {
                self.final_retention = true;
            }
            self.checkpoints.start_if_settled(
                settled
                    && self.log.is_idle()
                    && !self.log.live_pending()
                    && retain == values::StreamRetention::Ready,
                self.recording_failed || retain == values::StreamRetention::Failed,
                &self.recording,
            );
            self.waiters.poll(
                self.workspace.runtime(),
                self.io.is_idle()
                    && self.active.is_none()
                    && self.environment_work.is_empty()
                    && !self.checkpoints.busy()
                    && self.workspace_waiting.is_empty()
                    && self.sources.is_empty()
                    && self.waiting.is_empty()
                    && self.values.as_ref().is_none_or(SessionValues::is_idle)
                    && self.log.is_idle(),
            );
            if self.workspace.runtime().is_closed()
                && self.workspace.runtime().is_drained()
                && self.io.is_drained()
                && self.final_retention
                && self.admission.is_empty()
                && self.view_queries.is_empty()
                && self.environment_work.is_empty()
                && !self.checkpoints.busy()
                && self.workspace_waiting.is_empty()
                && self.waiting.is_empty()
                && self.log.is_idle()
                && self.values.as_ref().is_none_or(SessionValues::is_idle)
            {
                break;
            }
            let query_deadline = self.view_queries.deadline(&self.workspace);
            let display_deadline = self
                .displays
                .deadline()
                .into_iter()
                .chain(self.workspace.views.mount_deadline())
                .min();
            let mut changed = true;
            tokio::select! {
                control = self.controls.recv(), if self.controls_open => match control {
                    Some(control) => {
                        changed = !matches!(&control, Control::SandboxRead { .. } | Control::SandboxWorkspace { .. } | Control::Observe(_) | Control::ViewCatalogue(_) | Control::ViewFrame(..) | Control::ViewInteraction { .. } | Control::ViewInputs(..) | Control::ViewMount(..) | Control::DisplayValue(..) | Control::ImportedSpecs(_) | Control::ObserveActor { .. } | Control::Snapshot(_) | Control::Log(_) | Control::Values(_) | Control::WaitIdle(_) | Control::Input { .. } | Control::CheckRetirementAccess(_)) && !matches!(&control, Control::Environments(request) if matches!(request.as_ref(), environments::Request::Observe(_) | environments::Request::Authentication(_) | environments::Request::Documents(_) | environments::Request::PrepareTarget { .. }));
                        self.control(control);
                    },
                    None => {self.controls_open = false; self.close();}
                },
                submission = self.sources.recv(), if self.sources_open && self.active.is_none() && self.environment_work.is_empty() && (!self.checkpoints.busy() || self.checkpoints.rejects_sources()) => match submission {
                    Some(submission) if self.checkpoints.rejects_sources() => { let _ = submission.reply.send(Err(SessionError::CheckpointBusy)); },
                    Some(submission) => self.submit(submission),
                    None => {self.sources_open = false; if !self.controls_open {self.close();}}
                },
                completed = self.view_queries.tasks.join_next(), if !self.view_queries.is_empty() => {
                    if let Some(completed) = completed { self.view_queries.completed(completed, &mut self.workspace); }
                    changed = false;
                },
                () = async { match query_deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => {
                    self.view_queries.tick(&mut self.workspace, self.type_reader.clone(), self.query_capacity.clone());
                    // Active view readers sample their own revisions; no whole-workspace update per tick.
                    changed = false;
                },
                input = self.io.next() => {
                    changed = input.changes_snapshot();
                    if self.checkpoints.busy() && !self.workspace.runtime().is_closed() && input.deferrable(&self.workspace) {
                        self.deferred_windows.push(input);
                    } else { self.runtime_input(input); }
                },
                Some(result) = self.admission.join_next(), if !self.admission.is_empty() => match result {
                    Ok(result) => self.admitted(result),
                    Err(_) => {
                        self.recording_failed = matches!(self.recording, RecordingMode::Required(_));
                        self.finish_active(Err(SessionError::Preparation));
                    },
                },
                Some(result) = self.environment_work.join_next(), if !self.environment_work.is_empty() => {
                    match result { Ok(result) => self.environment_completed(result), Err(_) => { self.recording_failed = matches!(self.recording, RecordingMode::Required(_)); } }
                },
                Some(result) = self.waiting.join_next(), if !self.waiting.is_empty() => {
                    if let Ok((cell, reply)) = result {self.finish_cell(&cell, reply);}
                },
                () = self.log.completed(), if !self.log.is_idle() => self.check_log(),
                completed = async { self.values.as_mut().expect("configured storage").completed(&mut self.workspace).await },
                    if self.values.as_ref().is_some_and(SessionValues::has_completion) => {
                    self.recording_failed |= self.values.as_ref().is_some_and(SessionValues::recording_failed);
                    for pin in completed.pins {
                        let result = match pin.intent.principal.as_ref() {
                            Some((authority,actor)) => {
                                let scope=if authority.is_local_user() {Default::default()} else {self.actor_scope(actor)};
                                self.workspace.bind_kept_pin(&pin,&scope)
                            },
                            None=>Err("Pin execution authority is no longer available".into()),
                        };
                        if let Err(problem) = &result {
                            self.storage_notice(values::StorageNotice {
                                context:crate::history::NoticeContext::Publication {node:pin.node.clone(),run:pin.run.clone(),handle:Some(pin.handle.clone())},
                                error:crate::runtime::RuntimeCode::ExecutionFailed.error(format!("Input was kept, but the view was not pinned: {problem}"),None),
                            });
                        }
                        self.values.as_mut().expect("configured storage").finish_pin(&pin,result);
                    }
                    for notice in completed.notices { self.storage_notice(notice); }
                    self.effects(completed.effects);
                },
                Some(result) = self.workspace_waiting.join_next(), if !self.workspace_waiting.is_empty() => {
                    if let Ok((cell, reply)) = result { self.finish_cell(&cell, reply); }
                },
                resumed = self.checkpoints.completed(), if self.checkpoints.busy() => {
                    // Retirement refuses sources received while fenced instead of
                    // resuming them against a graph with deleted identities.
                    if self.checkpoints.retirement_fenced() {
                        let queued = self.sources.len();
                        for _ in 0..queued {
                            let Ok(submission) = self.sources.try_recv() else { break };
                            let _ = submission.reply.send(Err(SessionError::CheckpointBusy));
                        }
                    }
                    if let Some(resumed) = resumed { let _ = resumed.send(()); }
                },
                () = waiting::until(self.waiters.next_deadline()) => {},
                () = async { match display_deadline { Some(at) => tokio::time::sleep_until(at).await, None => std::future::pending().await } } => { self.displays.expire(); self.workspace.views.expire_mounts(); changed = false; },
            }
            if self.view_queries.uncertain {
                self.remote_uncertain
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            for (origin, message) in std::mem::take(&mut self.view_queries.notices) {
                self.log.capture(log_time().and_then(|at| {
                    PreparedLog::notice(
                        at,
                        crate::history::NoticeContext::Execution {
                            node: origin.node().clone(),
                            run: origin.id().clone(),
                        },
                        crate::runtime::RuntimeCode::ExecutionFailed
                            .error(format!("View query stopped: {message}"), None),
                    )
                }));
            }
            if changed {
                let _ = self.updates.send(());
            }
        }
        self.sources.close();
        self.controls.close();
        while let Ok(submission) = self.sources.try_recv() {
            let _ = submission.reply.send(Err(SessionError::Stopped));
        }
        while let Ok(control) = self.controls.try_recv() {
            self.control(control);
        }
        self.cells.close();
        self.sandboxes.shutdown().await;
    }
    fn submit(&mut self, mut submission: Submission<ParsedSource>) {
        if self.workspace.runtime().is_closed() {
            let _ = submission.reply.send(Err(SessionError::Stopped));
            return;
        }
        let input = submission.source.input().clone();
        if !self.cells.begin(input.clone(), submission.reply) {
            return;
        }
        if self.recording_failed {
            self.finish_cell(input.cell(), Err(SessionError::Recording));
            return;
        }
        if input.is_cooperative()
            && let Some(origin) = input.revision_of()
            && let Err(error) = self.check_cell_change(input.client(), origin)
        {
            self.finish_cell(input.cell(), Err(error));
            return;
        }
        if let Some(origin) = input.revision_of() {
            if self.cells.definition(origin).is_none() || self.cells.has_revision(origin) {
                self.finish_cell(input.cell(), Err(SessionError::RepeatRefused(
                    "Edit the latest successful definition of this work; the selected definition is missing or has already been revised.")));
                return;
            }
        }
        let context = input
            .environments()
            .cloned()
            .or_else(|| self.environment_clients.get(input.client()).cloned())
            .or_else(|| self.workspace.default_environment_context());
        let default_selected = self.workspace.default_environment.is_some()
            && context.as_ref().and_then(|c| c.selected.as_ref())
                == self.workspace.default_environment.as_ref();
        if self.workspace.environment_managed && submission.source.statements().iter().any(|s| matches!(&s.expression, wes_language::Expression::Call(c) if c.marker.is_some() && c.path.first().is_some_and(|n| n.text == "import") && !c.path.get(1).is_some_and(|n|matches!(n.text.as_str(),"plan"|"apply")) && (!default_selected || c.arguments.iter().any(|a| matches!(a.key.text.as_str(), "env" | "target"))))) {
            if input.is_cooperative() {
                self.finish_cell(input.cell(), Err(SessionError::AccessDenied(
                    "This import changes shared environment definitions. Ask the user to review and apply the definition change through wes's user controls.".into())));
                return;
            }
            self.environment_import(input, submission.source.statements());
            return;
        }
        fn binds_plan(
            statement: &wes_language::Statement,
            names: &std::collections::BTreeSet<&str>,
        ) -> bool {
            statement
                .binding
                .iter()
                .chain(statement.error_binding.iter())
                .any(|b| names.contains(b.name.text.as_str()))
                || matches!(&statement.expression,wes_language::Expression::Pipeline(stages) if stages.iter().any(|s|binds_plan(s,names)))
                || matches!(&statement.expression,wes_language::Expression::Fork(branches) if branches.iter().any(|b|binds_plan(&b.body,names)))
        }
        let plan_names = self
            .environment_plans
            .keys()
            .map(|(_, name)| name.as_str())
            .collect();
        if submission
            .source
            .statements()
            .iter()
            .any(|s| binds_plan(s, &plan_names))
        {
            self.finish_cell(
                input.cell(),
                Err(SessionError::RepeatRefused(
                    "This name is reserved by a live environment plan. Use another output name.",
                )),
            );
            return;
        }
        self.active = Some(input.cell().to_owned());
        let draft = match self.workspace.draft() {
            Ok(draft) => draft
                .with_environment_context(context, false)
                .with_reserved_names(
                    self.environment_plans
                        .keys()
                        .map(|(_, name)| name.clone())
                        .chain(self.sandboxes.names()),
                )
                .with_access(self.source_scope(&input)),
            Err(_) => {
                self.finish_active(Err(SessionError::Preparation));
                return;
            }
        };
        self.active_import = match self.admit_import_apply(&mut submission.source) {
            Ok(guard) => guard,
            Err(error) => {
                self.finish_active(Err(error));
                return;
            }
        };
        self.preparation_token = CancellationToken::new();
        let token = self.preparation_token.clone();
        let types = TypeSourceCapture::live(self.type_reader.clone());
        let credit = submission.credit;
        self.admission.spawn(async move {
            let _credit = credit;
            Admission::Prepared(
                source::prepare_parsed(submission.source, draft, types, token).await,
            )
        });
    }
    fn admitted(&mut self, result: Admission) {
        if self.workspace.runtime().is_closed() {
            self.finish_active(Err(SessionError::Stopped));
            return;
        }
        match result {
            Admission::Prepared(Err(error)) => self.finish_active(Err(match error {
                SourceError::Cancelled => SessionError::Cancelled,
                SourceError::Capacity => SessionError::Capacity,
                _ => SessionError::Preparation,
            })),
            Admission::Prepared(Ok(prepared)) => {
                if self.preparation_token.is_cancelled() {
                    self.finish_active(Err(SessionError::Cancelled));
                    return;
                }
                if self.recording_failed {
                    self.finish_active(Err(SessionError::Recording));
                    return;
                }
                if let Some(guard) = &self.active_import
                    && let Err(error) = guard.validate(self, prepared.input())
                {
                    self.finish_active(Err(error));
                    return;
                }
                let names: std::collections::BTreeSet<String> = prepared
                    .written_names()
                    .into_iter()
                    .chain(prepared.nodes().map(|n| n.as_str().to_owned()))
                    .collect();
                if self
                    .environment_plans
                    .keys()
                    .any(|(_, name)| names.contains(name))
                {
                    self.finish_active(Err(SessionError::RepeatRefused("An environment plan claimed an output name while this declaration was preparing. Retry with a fresh name.")));
                    return;
                }
                self.reserved_output_names = names;
                // Charge the prepared success notices before recording or committing effects.
                let reserve = reply_reservation(&prepared);
                if !self.cells.reserve(prepared.input().cell(), reserve) {
                    self.finish_active(Err(SessionError::Capacity));
                    return;
                }
                if let RecordingMode::Required(journal) = &self.recording
                    && let Some(command) = prepared.record()
                {
                    let command = command.clone();
                    let journal = journal.clone();
                    self.admission.spawn(async move {
                        let result = match journal.admit(command).await {
                            Ok(receipt) => prepared
                                .with_admission(receipt)
                                .map_err(|_| SessionError::Commit),
                            Err(_) => Err(SessionError::Recording),
                        };
                        Admission::Recorded(result)
                    });
                } else {
                    self.install(prepared)
                }
            }
            Admission::Recorded(Ok(prepared)) => self.install(prepared),
            Admission::Recorded(Err(error)) => {
                // Even failure of the accepted-cell append can follow a written command record.
                // Do not reuse that draft's uninstalled identities in another recorded admission.
                self.recording_failed = true;
                self.finish_active(Err(error));
            }
        }
    }
    fn install(&mut self, prepared: PreparedSource) {
        if self.preparation_token.is_cancelled() {
            self.finish_active(Err(SessionError::Cancelled));
            return;
        }
        if self.recording_failed {
            self.finish_active(Err(SessionError::Recording));
            return;
        }
        if let Some(guard) = &self.active_import
            && let Err(error) = guard.validate(self, prepared.input())
        {
            self.finish_active(Err(error));
            return;
        }
        let mut result = report(&prepared);
        let client = prepared.input().client().to_owned();
        let owner_cell = prepared.input().cell().to_owned();
        let written_names = prepared.written_names();
        let cooperative = prepared.input().is_cooperative();
        let execution_authority =
            crate::environments::InvocationAuthority::from_source(prepared.input());
        let changed_nodes = prepared.changed_nodes();
        let created_nodes: Vec<_> = prepared.nodes().cloned().collect();
        let default_before = self
            .workspace
            .default_environment
            .as_ref()
            .and_then(|name| {
                self.workspace
                    .environments()
                    .inspect(name)
                    .map(|environment| (name.clone(), environment.revision()))
            });
        match prepared.commit(&mut self.workspace, self.io.now()) {
            Ok(applied) => {
                result.receipts = applied.receipts;
                for node in created_nodes.iter().chain(&changed_nodes) {
                    self.execution_principals
                        .insert(node.clone(), (execution_authority.clone(), client.clone()));
                }
                self.execution_principals
                    .retain(|node, _| self.workspace.runtime().graph().node(node).is_some());
                if !cooperative {
                    for node in changed_nodes {
                        self.node_owners.insert(node, client.clone());
                    }
                }
                for node in created_nodes {
                    self.node_owners.insert(node, client.clone());
                }
                self.node_owners
                    .retain(|node, _| self.workspace.runtime().graph().node(node).is_some());
                for name in written_names {
                    if cooperative {
                        self.name_owners
                            .entry(name)
                            .or_insert_with(|| (client.clone(), owner_cell.clone()));
                    } else {
                        self.name_owners
                            .insert(name, (client.clone(), owner_cell.clone()));
                    }
                }
                self.name_owners
                    .retain(|name, _| self.workspace.bindings().names().contains_key(name));
                // An import explicitly approves this client's next default image. Do not rebase
                // another client's acknowledgement or a draft already frozen in the UI.
                if let Some((name, before)) = default_before
                    && let Some(context) = self.environment_clients.get_mut(&client)
                    && context.selected.as_ref() == Some(&name)
                    && context.revisions.get(&name) == Some(&before)
                    && let Some(environment) = self.workspace.environments().inspect(&name)
                {
                    context.revisions.insert(name, environment.revision());
                }
                self.log.retain_nodes(&self.workspace);
                if let Some(values) = &mut self.values {
                    values.retain_nodes(&self.workspace);
                }
                // Analysis warnings already appear in the report; commit adds only success notices.
                for applied in applied.changes {
                    result.diagnostics.diagnostics.extend(
                        applied
                            .diagnostics
                            .into_iter()
                            .filter(|d| d.severity == wes_language::Severity::Info),
                    );
                }
                result
                    .diagnostics
                    .diagnostics
                    .sort_by_key(|d| d.span.start());
                self.capture_repeat_definition(&result);
                self.finish_active(Ok(Arc::new(result)));
                self.effects(applied.effects);
                let effects = self.workspace.start(self.io.now());
                self.effects(effects);
            }
            Err(error @ WorkspaceError::UnsafeObservation) => {
                self.finish_active(Err(SessionError::AccessDenied(error.to_string())));
            }
            Err(_) => {
                self.recording_failed = matches!(self.recording, RecordingMode::Required(_));
                self.finish_active(Err(SessionError::Commit));
            }
        }
    }
    fn finish_active(&mut self, reply: SubmissionReply) {
        self.active_import = None;
        self.reserved_output_names.clear();
        if let Some(cell) = self.active.take() {
            self.finish_cell(&cell, reply)
        }
    }
    fn control(&mut self, control: Control) {
        match control {
            Control::StopWorkspace {
                cells,
                environments,
                stop,
                reply,
            } => {
                let mut actual = self
                    .cells
                    .observed()
                    .iter()
                    .map(|c| c.input.cell().to_owned())
                    .collect::<Vec<_>>();
                actual.sort();
                let busy = self.active.is_some()
                    || !self.environment_work.is_empty()
                    || self.checkpoints.busy();
                if busy
                    || actual != cells
                    || environments != self.workspace.environments().revisions()
                    || (!stop && !self.io.is_idle())
                {
                    let _ = reply.send(Err(SessionError::CheckpointBusy));
                } else {
                    self.close();
                    let _ = reply.send(Ok(()));
                }
            }

            Control::ResolveManagement {
                input,
                reference,
                reply,
            } => {
                let mut admitted = false;
                let result = if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else if input.is_cooperative() {
                    Err(SessionError::Authority)
                } else if self.cells.contains(input.cell()) {
                    Err(SessionError::Conflict)
                } else if self.checkpoints.busy()
                    || self.active.is_some()
                    || !self.environment_work.is_empty()
                    || !self.workspace_waiting.is_empty()
                {
                    Err(SessionError::AdmissionBusy)
                } else {
                    let (waiter, _receive) = oneshot::channel();
                    if !self.cells.begin(input.clone(), waiter) {
                        Err(SessionError::Capacity)
                    } else {
                        admitted = true;
                        if !self.cells.reserve(input.cell(), 4096) {
                            let _ = reply.send((admitted, Err(SessionError::Capacity)));
                            return;
                        }
                        self.workspace.resolve(&reference).and_then(|output| match self.workspace.runtime().output(&output) {
                            crate::runtime::OutputState::Available(value) if value.shape() == &wes_core::Shape::Meta(wes_core::MetaType::WorkspaceDeletePlan) => value.management_authority().map(str::to_owned),
                            _ => None,
                        }).ok_or_else(|| SessionError::Management("Expected a live WorkspaceDeletePlan; use a plan node reference and replan if its authority has expired or was restored.".into()))
                    }
                };
                let _ = reply.send((admitted, result));
            }
            Control::CompleteManagement {
                cell,
                result,
                reply,
            } => {
                if self.cells.contains(&cell) {
                    self.finish_cell(&cell, result);
                }
                let _ = reply.send(());
            }
            Control::SandboxRead {
                member,
                inspect,
                cooperative,
                reply,
            } => {
                let _ = reply.send(sandbox::observe(
                    &self.workspace,
                    member.as_deref(),
                    inspect,
                    cooperative,
                    None,
                ));
            }
            Control::SandboxWorkspace { input, name, reply } => {
                let result = if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else if self.checkpoints.busy() || self.active.is_some() {
                    Err(SessionError::CheckpointBusy)
                } else if name.as_ref().is_some_and(|name| {
                    self.workspace.resolve(name).is_some()
                        || self.environment_plans.keys().any(|(_, n)| n == name)
                }) {
                    Err(SessionError::AccessDenied(
                        "Sandbox name is already used in this workspace.".into(),
                    ))
                } else {
                    let workspace = self.workspace.sandbox_workspace();
                    let context = input
                        .environments()
                        .cloned()
                        .or_else(|| self.environment_clients.get(input.client()).cloned());
                    workspace
                        .map(|mut workspace| {
                            if let Some(context) = context {
                                workspace.default_environment = context.selected;
                            }
                            workspace
                        })
                        .map_err(|_| SessionError::Preparation)
                };
                let _ = reply.send(result);
            }
            Control::ObserveActor {
                actor,
                inherit,
                environment,
                reply,
            } => {
                let result = self
                    .attach_actor(&actor, &inherit, environment)
                    .map(|()| self.observation());
                let _ = reply.send(result);
            }
            Control::GrantWork {
                actor,
                cells,
                reply,
            } => {
                let result = self.grant_work(actor, cells);
                let _ = reply.send(result);
            }
            Control::GrantSources {
                actor,
                cells,
                reply,
            } => {
                let _ = reply.send(self.grant_sources(&actor, cells));
            }
            Control::ReadSource { actor, cell, reply } => {
                let _ = reply.send(self.read_source(&actor, &cell));
            }
            Control::CancelActor {
                actor,
                nodes,
                reply,
            } => {
                let result = self.cancel_actor(&actor, &nodes);
                let _ = reply.send(result);
            }
            Control::CancelActorWork { actor, cell, reply } => {
                let result = self
                    .cells
                    .nodes(&cell)
                    .and_then(|nodes| self.cancel_actor(&actor, &nodes));
                let _ = reply.send(result);
            }
            Control::Repeat(submission) => self.repeat(*submission),
            Control::Environments(request) => self.environment_request(*request),
            Control::Input {
                node,
                run,
                bytes,
                reply,
            } => {
                let result = self
                    .io
                    .input(self.workspace.runtime(), &node, &run, bytes.as_deref())
                    .map_err(Into::into);
                let _ = reply.send(result);
            }
            Control::Cancel(node, reply) => {
                let result = if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else if self.checkpoints.busy() && !self.checkpoints.pending() {
                    Err(SessionError::CheckpointBusy)
                } else if self.workspace.runtime().graph().node(&node).is_none() {
                    Err(SessionError::UnknownNode)
                } else {
                    let effects = self.workspace.cancel(&node, self.io.now());
                    self.effects(effects);
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Control::CancelWork(origin, reply) => {
                let result = if self.checkpoints.busy() && !self.checkpoints.pending() {
                    Err(SessionError::CheckpointBusy)
                } else if self.active.as_deref() == Some(&origin) {
                    self.preparation_token.cancel();
                    Ok(())
                } else if let Ok(nodes) = self.cells.nodes(&origin) {
                    match self.workspace.cancel_nodes(&nodes, self.io.now()) {
                        Ok(effects) => {
                            self.effects(effects);
                            Ok(())
                        }
                        Err(_) => Err(SessionError::Commit),
                    }
                } else {
                    Err(SessionError::UnknownNode)
                };
                let _ = reply.send(result);
            }
            Control::ImportedSpecs(reply) => {
                let _ = reply.send(self.workspace.imported_specs());
            }
            Control::ViewCatalogue(reply) => {
                let result = if self.checkpoints.rejects_sources() {
                    Err(SessionError::CheckpointBusy)
                } else if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else {
                    Ok(self.workspace.view_catalogue().clone())
                };
                let _ = reply.send(result);
            }
            Control::ViewInputs(node, identity, reply) => {
                let result = if self.checkpoints.rejects_sources() {
                    Err(SessionError::CheckpointBusy)
                } else {
                    self.workspace
                        .view_input_patches(&node, &identity)
                        .map_err(SessionError::AccessDenied)
                };
                let _ = reply.send(result);
            }
            Control::ViewInteraction {
                node,
                identity,
                edit,
                reply,
            } => {
                let result = if self.checkpoints.rejects_sources()
                    || (edit.is_some() && self.checkpoints.busy())
                {
                    Err(SessionError::CheckpointBusy)
                } else {
                    let result = edit.map_or(Ok(()), |edit| {
                        self.workspace
                            .commit_view_interaction(&node, &identity, edit)
                            .map(|_| ())
                    });
                    result
                        .and_then(|()| self.workspace.view_interaction(&node, &identity))
                        .map_err(SessionError::AccessDenied)
                };
                let _ = reply.send(result);
            }
            Control::ViewMount(node, identity, action, reply) => {
                let result = if self.checkpoints.rejects_sources()
                    && !matches!(action, crate::views::MountAction::Close(_))
                {
                    Err(SessionError::CheckpointBusy)
                } else {
                    self.workspace
                        .view_mount(&node, &identity, action)
                        .map_err(SessionError::AccessDenied)
                };
                let _ = reply.send(result);
            }
            Control::ViewFrame(node, mount, reply) => {
                let result = if self.checkpoints.rejects_sources() {
                    Err(SessionError::CheckpointBusy)
                } else {
                    mount
                        .map_or(Ok(()), |(identity, token)| {
                            self.sample_view(&node, &identity, &token)
                        })
                        .and_then(|()| {
                            self.workspace
                                .view_frame(&node)
                                .map_err(SessionError::AccessDenied)
                        })
                };
                let _ = reply.send(result);
            }
            Control::DisplayValue(node, reply) => {
                let result = if self.checkpoints.rejects_sources() {
                    Err(SessionError::CheckpointBusy)
                } else {
                    self.displays.read(&self.workspace, &node)
                };
                let _ = reply.send(result);
            }
            Control::Observe(reply) => {
                let _ = reply.send(self.observation());
            }
            Control::CheckRetirementAccess(reply) => {
                let result = if self.checkpoints.rejects_sources() {
                    Err(SessionError::CheckpointBusy)
                } else if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else {
                    Ok(())
                };
                let _ = reply.send(result);
            }
            Control::FenceRetirement(authority, reply) => {
                let result = if self.checkpoints.fence_retirement(&authority) {
                    Ok(())
                } else {
                    Err(SessionError::CheckpointBusy)
                };
                let _ = reply.send(result);
            }
            Control::Retire(plan, image, authority, reply) => {
                let result = if !self.checkpoints.permits(&authority) {
                    Err(SessionError::CheckpointBusy)
                } else {
                    self.retire_work(plan, image)
                };
                let _ = reply.send(result);
            }
            Control::Checkpoint(reply) => {
                if self.workspace.runtime().is_closed() {
                    let _ = reply.send(Err(SessionError::Stopped));
                } else {
                    self.checkpoints.request(reply, &self.recording);
                }
            }
            Control::Storage(command, handle, reply) => {
                if self.workspace.runtime().is_closed() {
                    let _ = reply.send(Err(SessionError::Stopped));
                } else if self.checkpoints.busy() {
                    let _ = reply.send(Err(SessionError::CheckpointBusy));
                } else if command == StorageCommand::Release
                    && self
                        .workspace
                        .views
                        .retained_inputs()
                        .iter()
                        .any(|(_, saved)| saved == &handle)
                {
                    let _ = reply.send(Err(SessionError::Management(
                        "A pinned view still uses this result; rebind the view before release"
                            .into(),
                    )));
                } else if command != StorageCommand::Release && self.recording_failed {
                    let _ = reply.send(Err(SessionError::Recording));
                } else if let Some(values) = &mut self.values {
                    values.request(command, handle, reply);
                } else {
                    let _ = reply.send(Err(SessionError::NoStorage));
                }
            }
            Control::ReleaseAtCheckpoint(handle, authority, reply) => {
                if self.workspace.runtime().is_closed() {
                    let _ = reply.send(Err(SessionError::Stopped));
                } else if self
                    .workspace
                    .views
                    .retained_inputs()
                    .iter()
                    .any(|(_, saved)| saved == &handle)
                {
                    let _ = reply.send(Err(SessionError::Management(
                        "A pinned view still uses this result; rebind the view before release"
                            .into(),
                    )));
                } else if !self.checkpoints.permits(&authority) {
                    let _ = reply.send(Err(SessionError::CheckpointBusy));
                } else if let Some(values) = &mut self.values {
                    values.request(StorageCommand::Release, handle, reply);
                } else {
                    let _ = reply.send(Err(SessionError::NoStorage));
                }
            }
            Control::RetentionPolicy(policy, reply) => {
                let result = if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else if self.checkpoints.busy() {
                    Err(SessionError::CheckpointBusy)
                } else if let Some(values) = &mut self.values {
                    values.set_policy(policy);
                    Ok(())
                } else {
                    Err(SessionError::NoStorage)
                };
                let _ = reply.send(result);
            }
            Control::Values(reply) => {
                let _ = reply.send(self.values.as_ref().map(SessionValues::snapshot));
            }
            Control::Log(reply) => {
                let _ = reply.send(self.log.snapshot());
            }
            Control::Snapshot(reply) => {
                let _ = reply.send(SessionSnapshot {
                    execution: Snapshot::capture(self.workspace.runtime(), self.io.is_idle()),
                    names: self.workspace.bindings().names().clone(),
                    conversations: self.io.active_conversations(self.workspace.runtime()),
                    admission_pending: self.active.is_some() || !self.environment_work.is_empty(),
                    checkpoint_pending: self.checkpoints.busy(),
                    recording_blocked: self.recording_failed,
                    restoration: self.restoration.clone(),
                });
            }
            Control::WaitIdle(reply) => {
                if self.workspace.runtime().is_closed()
                    && self.io.is_idle()
                    && self.active.is_none()
                    && self.environment_work.is_empty()
                    && self.waiting.is_empty()
                    && self.log.is_idle()
                    && self.values.as_ref().is_none_or(SessionValues::is_idle)
                {
                    let _ = reply.send(Ok(()));
                } else {
                    self.waiters.insert(Waiter::Idle(reply));
                }
            }
            Control::Shutdown(reply) => {
                self.close();
                let _ = reply.send(());
            }
            Control::Submit(submission) => self.immediate(*submission),
        }
    }
    fn close(&mut self) {
        self.import_plans.clear();
        self.workspace.views.stop_queries();
        self.view_queries.stop();
        self.sandboxes.close();
        if !self.workspace.sandbox_runtime
            && let Some(authority) = self
                .workspace
                .environment_loader
                .as_ref()
                .and_then(|l| l.authority())
        {
            authority.close();
        }
        self.sources.close();
        self.checkpoints.close();
        self.preparation_token.cancel();
        let effects = self.workspace.close();
        self.effects(effects);
        while let Ok(submission) = self.sources.try_recv() {
            let _ = submission.reply.send(Err(SessionError::Stopped));
        }
    }
    fn immediate(&mut self, submission: Submission<SourcePreparation<ParsedSource>>) {
        if self.workspace.runtime().is_closed() {
            let _ = submission.reply.send(Err(SessionError::Stopped));
            return;
        }
        let input = match &submission.source {
            SourcePreparation::Immediate { input, .. }
            | SourcePreparation::Rejected { input, .. } => input,
            SourcePreparation::Declarations(_) => unreachable!("declaration mailbox"),
        };
        if self.cells.contains(input.cell()) {
            self.cells.begin(input.clone(), submission.reply);
            return;
        }
        if self.checkpoints.busy() && !self.checkpoints.allows_cancel(&submission.source) {
            let _ = submission.reply.send(Err(SessionError::CheckpointBusy));
            return;
        }
        let (input, statement, mut diagnostics) = match submission.source {
            SourcePreparation::Immediate { input, statement } => {
                (input, Some(statement), SourceDiagnostics::default())
            }
            SourcePreparation::Rejected { input, diagnostics } => (input, None, diagnostics),
            SourcePreparation::Declarations(_) => {
                unreachable!("declarations use the serialized queue")
            }
        };
        if !self.cells.begin(input.clone(), submission.reply) {
            return;
        }
        let cell = input.cell().to_owned();
        let mut result = SubmissionResult {
            receipts: vec![],
            sandbox: None,
            cell: cell.clone(),
            nodes: vec![],
            accepted: vec![],
            removed: vec![],
            unbound: vec![],
            diagnostics: SourceDiagnostics::default(),
            recorded: false,
            restored: false,
            refreshed: vec![],
            repeated_run: None,
        };
        let Some(statement) = statement else {
            result.diagnostics = diagnostics;
            self.finish_immediate(result);
            return;
        };
        if matches!(&statement.expression, wes_language::Expression::Call(call) if call.marker.is_some() && wes_language::vocabulary::commands::invocation(call).is_ok_and(|i|i.spec.command == MetaCommand::Env))
        {
            self.environment_source(input, *statement, result);
            return;
        }
        let prepared = self.workspace.prepare(&statement);
        match prepared {
            Ok(Preparation::Meta(meta)) if meta.command() == MetaCommand::Env => {
                self.environment_source(input, *statement, result);
                return;
            }
            Ok(Preparation::Meta(meta))
                if matches!(meta.command(), MetaCommand::Save | MetaCommand::Load) =>
            {
                self.workspace_action(meta, result, statement.span);
                return;
            }
            Ok(Preparation::Meta(meta)) if meta.command() == MetaCommand::Wait => {
                match self.workspace.prepare_wait(meta) {
                    Ok(wait) => {
                        result.accepted.push(statement.span);
                        result.diagnostics.diagnostics = wait.diagnostics().to_vec();
                        if !self.cells.reserve(&cell, identity::charge(&result) + 4096) {
                            self.finish_cell(&cell, Err(SessionError::Capacity));
                            return;
                        }
                        let (reply, receive) = oneshot::channel();
                        self.waiters.insert(Waiter::Outputs {
                            selected: wait.selected().clone(),
                            until: tokio::time::Instant::now() + wait.budget(),
                            reply,
                        });
                        self.waiting.spawn(async move {
                            let reply = match receive.await {
                                Ok(Ok(available)) => {
                                    if !available {
                                        result.diagnostics.diagnostics.push(wait.unavailable());
                                    }
                                    Ok(Arc::new(result))
                                }
                                Ok(Err(error)) => Err(SessionError::Driver(error)),
                                Err(_) => Err(SessionError::Stopped),
                            };
                            (cell, reply)
                        });
                        return;
                    }
                    Err(error) => diagnostic_error(&mut diagnostics, error, statement.span),
                }
            }
            Ok(Preparation::Meta(meta))
                if matches!(meta.command(), MetaCommand::Cancel | MetaCommand::Refresh) =>
            {
                match self.workspace.prepare_control(meta) {
                    Ok(control) => {
                        if let Err(error) = self
                            .workspace
                            .check_control_access(&control, &self.source_scope(&input))
                        {
                            self.finish_cell(&cell, Err(SessionError::from_access(error)));
                            return;
                        }
                        if control.refreshes_downstream() {
                            if self.recording_failed {
                                self.finish_cell(&cell, Err(SessionError::Recording));
                                return;
                            }
                            if self.active.is_some() || !self.environment_work.is_empty() {
                                self.finish_cell(&cell, Err(SessionError::AdmissionBusy));
                                return;
                            }
                        }
                        result.diagnostics.diagnostics = control.diagnostics().to_vec();
                        if !self.cells.reserve(&cell, identity::charge(&result) + 4096) {
                            self.finish_cell(&cell, Err(SessionError::Capacity));
                            return;
                        }
                        let targets = control.execution_targets(self.workspace.runtime().graph());
                        result.refreshed = targets.clone();
                        if !self.cells.reserve(&cell, identity::charge(&result) + 4096) {
                            self.finish_cell(&cell, Err(SessionError::Capacity));
                            return;
                        }
                        match self.workspace.apply_control(control, self.io.now()) {
                            Ok(applied) => {
                                result.receipts.push(applied.receipt);
                                for node in targets {
                                    self.execution_principals.insert(
                                        node,
                                        (
                                            crate::environments::InvocationAuthority::from_source(
                                                &input,
                                            ),
                                            input.client().into(),
                                        ),
                                    );
                                }
                                self.effects(applied.effects);
                                result.accepted.push(statement.span);
                            }
                            Err(error) => {
                                result.refreshed.clear();
                                diagnostic_error(&mut diagnostics, error, statement.span);
                            }
                        }
                    }
                    Err(error) => diagnostic_error(&mut diagnostics, error, statement.span),
                }
            }
            Ok(_) => diagnostics.diagnostics.push(Diagnostic::error(
                "ENG007",
                statement.span,
                "this command is not available through this execution entry point",
            )),
            Err(error) => diagnostic_error(&mut diagnostics, error, statement.span),
        }
        result
            .diagnostics
            .diagnostics
            .extend(diagnostics.diagnostics);
        result.diagnostics.issues.extend(diagnostics.issues);
        self.finish_immediate(result);
    }
    fn finish_immediate(&mut self, result: SubmissionResult) {
        let cell = result.cell.clone();
        if result.diagnostics.check_limit().is_err() {
            self.finish_cell(&cell, Err(SessionError::Capacity));
            return;
        }
        if self.cells.reserve(&cell, identity::charge(&result)) {
            self.finish_cell(&cell, Ok(Arc::new(result)));
        } else {
            self.finish_cell(&cell, Err(SessionError::Capacity));
        }
    }
    fn finish_cell(&mut self, cell: &str, reply: SubmissionReply) {
        // Immediate controls and structural refusals can bypass declaration diagnostics.
        // Use the same source advisory at the common reply boundary, once per cell.
        let reply = match reply {
            Ok(mut result)
                if !result
                    .diagnostics
                    .diagnostics
                    .iter()
                    .any(|d| d.code == "SEC001") =>
            {
                if let Some(warning) = self
                    .cells
                    .input(cell)
                    .and_then(|input| crate::source::credential_literal_warning(input.text()))
                {
                    Arc::make_mut(&mut result)
                        .diagnostics
                        .diagnostics
                        .push(warning);
                    if !self.cells.reserve(cell, identity::charge(&result)) {
                        Err(SessionError::Capacity)
                    } else {
                        Ok(result)
                    }
                } else {
                    Ok(result)
                }
            }
            other => other,
        };
        let input = self
            .cells
            .input(cell)
            .expect("completion follows admission");
        self.log
            .capture(PreparedLog::submission(crate::history::SubmissionRecord {
                source_name: input.source_name().to_owned(),
                source_start: input.source_start(),
                document: input.document().map(str::to_owned),
                revision_of: input.revision_of().map(str::to_owned),
                id: uuid::Uuid::new_v4().to_string(),
                cell: cell.to_owned(),
                text: input.text().to_owned(),
                client: input.client().to_owned(),
                context: input.environments().cloned(),
                order: self.cells.order(cell),
                nodes: reply.as_ref().map_or_else(|_| vec![], |r| r.nodes.clone()),
                refreshed: reply
                    .as_ref()
                    .map_or_else(|_| vec![], |r| r.refreshed.clone()),
                repeat: input.repeat().cloned(),
                run: reply.as_ref().ok().and_then(|r| r.repeated_run.clone()),
            }));
        let source: Arc<str> = self
            .cells
            .input(cell)
            .expect("completion follows admission")
            .text()
            .into();
        let failure;
        let diagnostics = match &reply {
            Ok(result) => &result.diagnostics.diagnostics[..],
            Err(error) => {
                let (code, message) = match error {
                    SessionError::CommandArgument(message) => ("CMD001", (*message).to_owned()),
                    SessionError::Environment(error) => (error.code, error.message.clone()),
                    SessionError::Recording => ("RUN005", error.to_string()),
                    _ => ("ENG007", error.to_string()),
                };
                failure = [Diagnostic::error(code, Span::at(0), message)];
                &failure[..]
            }
        };
        for diagnostic in diagnostics {
            let prepared = log_time().and_then(|at| {
                PreparedLog::diagnostic_shared(
                    at,
                    cell.to_owned(),
                    source.clone(),
                    diagnostic.clone(),
                )
            });
            self.log.capture(prepared);
        }
        self.check_log();
        self.cells.finish(cell, reply);
    }
    fn runtime_input(&mut self, input: crate::driver::RuntimeInput<BoundTask>) {
        for (run, error) in input.notices() {
            if error.code() == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN {
                self.remote_uncertain
                    .store(true, std::sync::atomic::Ordering::Release);
            }
            self.log.capture(log_time().and_then(|at| {
                PreparedLog::notice(
                    at,
                    crate::history::NoticeContext::Execution {
                        node: run.node().clone(),
                        run: run.id().clone(),
                    },
                    error.clone(),
                )
            }));
        }
        self.io.announce_notices(&input);
        if let crate::driver::RuntimeInput::Conversation(update) = input {
            self.io
                .conversation_update(update, self.workspace.runtime());
            return;
        }
        if let crate::driver::RuntimeInput::Enter(mut enter) = input {
            if let BoundTask::View(view) = &mut enter.ticket.payload {
                view.origin = Some(enter.ticket.run.clone());
                if let Some((authority, client)) =
                    self.execution_principals.get(enter.ticket.run.node())
                {
                    let caller =
                        SourceInput::new(uuid::Uuid::new_v4().to_string(), ":view apply".into())
                            .and_then(|input| input.with_client(client.clone()))
                            .ok();
                    view.caller = caller.map(|input| {
                        if authority.is_local_user() {
                            input
                        } else {
                            input.cooperative()
                        }
                    });
                }
                view.scope = self
                    .execution_principals
                    .get(enter.ticket.run.node())
                    .filter(|(authority, _)| !authority.is_local_user())
                    .map(|(_, actor)| self.actor_scope(actor))
                    .unwrap_or_default();
            }
            let mut ticket = self.workspace.enter_ticket(enter.ticket);
            if let Ok(Some(ticket)) = &mut ticket {
                ticket.payload.set_authority(
                    self.execution_principals
                        .get(ticket.run.node())
                        .map(|(authority, _)| authority.clone())
                        .unwrap_or_default(),
                );
                if let BoundTask::Management(task) = &mut ticket.payload {
                    let user = self
                        .execution_principals
                        .get(ticket.run.node())
                        .filter(|(authority, _)| authority.is_local_user())
                        .map(|(_, client)| client.clone());
                    task.context = self.workspace_actions.clone().zip(user);
                }
                if let BoundTask::View(view) = &mut ticket.payload {
                    if let Some(mut intent) = view.pin.take() {
                        intent.principal =
                            self.execution_principals.get(ticket.run.node()).cloned();
                        let accepted = if self.recording_failed {
                            Err("Pin requires healthy durable workspace recording")
                        } else if let Some(values) = &mut self.values {
                            values.prepare_pin(
                                ticket.run.node().clone(),
                                ticket.run.id().clone(),
                                intent,
                            )
                        } else {
                            Err("Pin requires configured value storage")
                        };
                        if let Err(message) = accepted {
                            view.reject_pin(message);
                        }
                    }
                }
                if let BoundTask::ImportPlan(plan) = &ticket.payload {
                    let plan = plan.clone();
                    let outcome = self.freeze_import(&plan, &ticket.run, &ticket.inputs);
                    if let BoundTask::ImportPlan(plan) = &mut ticket.payload {
                        plan.captured = Some(outcome);
                    }
                }
                self.capture_session_query(ticket);
            }
            let _ = enter.reply.send(ticket);
            return;
        }
        let effects = input.apply_workspace(&mut self.workspace, self.io.now());
        self.effects(effects);
    }
    fn effects(&mut self, effects: Vec<Effect<BoundTask>>) {
        for effect in &effects {
            if let Effect::Observe(observation) = effect {
                self.displays.observe(&self.workspace, observation);
                let call = self
                    .workspace
                    .runtime()
                    .graph()
                    .node(&observation.node)
                    .and_then(|node| node.payload().call());
                let streaming = call.is_some_and(crate::providers::BoundCall::streaming);
                let event_stage = self.workspace.runtime().is_event_stage(&observation.node);
                let interactive = call.is_some_and(crate::providers::BoundCall::interactive);
                let opening = effects.iter().any(|effect| matches!(effect, Effect::StreamReady { run, .. }
                    if run.node() == &observation.node && Some(run.id()) == observation.run.as_ref()));
                // An event invocation owns a live lease, not a durable finite execution.
                // Retention captures its source epoch and output together; calls retain their
                // independent mandatory write-ahead/recovery receipts.
                let live = if event_stage {
                    self.workspace
                        .runtime()
                        .ordered_root(&observation.node)
                        .and_then(|source| {
                            let (source, epoch, delivery) = match observation.delivery.clone() {
                                Some((source, epoch, sequence)) => (source, epoch, Some(sequence)),
                                None => {
                                    let epoch = self.workspace.runtime().run_of(&source)?.clone();
                                    (source, epoch, None)
                                }
                            };
                            let record = log_time()
                                .and_then(|at| {
                                    crate::history::ExecutionRecord::capture(
                                        uuid::Uuid::new_v4().to_string(),
                                        at,
                                        observation,
                                    )
                                })
                                .ok()?;
                            Some(crate::history::LiveSnapshot {
                                source,
                                epoch,
                                delivery,
                                observation: record,
                                result: None,
                            })
                        })
                } else {
                    None
                };
                if let Some(snapshot) = &live {
                    self.log.observe_live(snapshot.clone());
                } else if !streaming
                    || !self.workspace.runtime().is_streaming(&observation.node)
                    || opening
                {
                    self.log.capture(
                        log_time().and_then(|at| PreparedLog::observation(at, observation)),
                    );
                }
                if let Some(values) = &mut self.values
                    && let Some(notice) = if interactive {
                        values.observe_interactive(observation)
                    } else {
                        values.observe_live(observation, streaming || event_stage, live.clone())
                    }
                {
                    self.storage_notice(notice);
                }
            }
        }
        self.check_log();
        self.io.effects(effects);
    }
    fn check_log(&mut self) {
        if matches!(self.recording, RecordingMode::Required(_)) && self.log.failed() {
            self.recording_failed = true;
        }
    }
    fn storage_notice(&mut self, notice: values::StorageNotice) {
        self.log.capture(
            log_time().and_then(|at| PreparedLog::notice(at, notice.context, notice.error)),
        );
    }
}
fn log_time() -> Result<wes_core::Timestamp, crate::history::InvalidRecord> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let invalid =
        || crate::history::InvalidRecord("wall clock is outside the supported timestamp range");
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| invalid())?;
    wes_core::Timestamp::new(
        i64::try_from(duration.as_secs()).map_err(|_| invalid())?,
        duration.subsec_nanos(),
    )
    .map_err(|_| invalid())
}
fn report(prepared: &PreparedSource) -> SubmissionResult {
    SubmissionResult {
        receipts: vec![],
        sandbox: None,
        cell: prepared.input().cell().to_owned(),
        nodes: prepared.nodes().cloned().collect(),
        accepted: prepared.accepted().to_vec(),
        removed: prepared.removed().to_vec(),
        unbound: prepared.unbound().to_vec(),
        diagnostics: prepared.diagnostics().clone(),
        recorded: prepared.record().is_some(),
        restored: false,
        refreshed: vec![],
        repeated_run: None,
    }
}
fn reply_reservation(prepared: &PreparedSource) -> usize {
    let mut result = report(prepared);
    result
        .diagnostics
        .diagnostics
        .extend(prepared.success_diagnostics().cloned());
    identity::charge(&result) + prepared.receipt_charge()
}
fn diagnostic_error(diagnostics: &mut SourceDiagnostics, error: WorkspaceError, span: Span) {
    match error {
        WorkspaceError::Rejected {
            diagnostics: errors,
            issues,
        } => {
            diagnostics.diagnostics.extend(errors);
            if !issues.is_empty() {
                diagnostics
                    .issues
                    .push(source::StatementIssues { span, issues });
            }
        }
        _ => diagnostics.diagnostics.push(Diagnostic::error(
            "ENG007",
            span,
            "the immediate command could not be applied",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn authority_error_preserves_only_public_diagnostic_text() {
        let error = WorkspaceError::from(
            Diagnostic::error("AUT001", Span::at(0), "private name and value")
                .with_public_message("Protected work. Ask the user for change scope."),
        );
        let message = SessionError::from_access(error)
            .authority_message()
            .unwrap();
        assert_eq!(message, "Protected work. Ask the user for change scope.");
        let error = WorkspaceError::from(Diagnostic::error(
            "AUT001",
            Span::at(0),
            "private name and value",
        ));
        let message = SessionError::from_access(error)
            .authority_message()
            .unwrap();
        assert!(!message.contains("private name"));
        assert!(SessionError::Stopped.authority_message().is_none());
    }
    struct NoFiles;
    impl TypeSourceReader for NoFiles {
        fn read(&self, _: &str, _: usize) -> Result<String, crate::type_sources::TypeSourceError> {
            panic!("no file reads")
        }
    }
    #[tokio::test]
    async fn full_declaration_credit_cannot_starve_immediate_control_or_snapshot_requests() {
        let (handle, task) = spawn(
            Workspace::local(crate::providers::LocalScope::new("fixture").unwrap()),
            RecordingMode::Ephemeral,
            Arc::new(NoFiles),
            NonZeroUsize::new(1).unwrap(),
        )
        .unwrap();
        let held = handle
            .source_credit
            .clone()
            .acquire_many_owned(256 * 1024 * 1024)
            .await
            .unwrap();
        let input = |cell: &str, text: &str| SourceInput::new(cell.into(), text.into()).unwrap();
        assert!(matches!(
            handle
                .submit(input("normal", ":type check \"ok\" as:Text"))
                .await,
            Err(SessionError::Capacity)
        ));
        let response = handle
            .submit(input("cancel", ":cancel $missing"))
            .await
            .unwrap();
        assert!(!response.diagnostics.diagnostics.is_empty());
        assert!(handle.snapshot().await.unwrap().execution.graph.is_empty());
        drop(held);
        assert!(
            handle
                .submit(input("normal", ":type check \"ok\" as:Text"))
                .await
                .is_ok()
        );
        handle.shutdown().await.unwrap();
        task.join().await.unwrap();
    }
}
