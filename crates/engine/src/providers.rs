//! Provider ownership and validated invocation boundaries. Concrete HTTP/process adapters live elsewhere.
use crate::{
    calls::{AdmittedCommand, CallJournal},
    driver::{
        CancellationToken, ExecutionFuture, ExecutionReport, Executor, InteractiveExecution,
        InteractiveExecutionFuture, StreamExecutionFuture,
    },
    graph::NodeId,
    plan::{Input, Invocation},
    runtime::{ExecutionTraits, Outcome, Run, RunTicket, RuntimeCode},
};
use indexmap::IndexMap;
use std::{fmt, future::Future, pin::Pin, sync::Arc};
use thiserror::Error;
use uuid::Uuid;
use wes_core::{
    ErrorId, ErrorValue, Provenance, Shape, Value,
    capability::{Capability, Catalogue, ProviderDescription, Safety},
    contracts::boundary::{self, BoundaryError},
};
mod interactive;
mod lifecycle;
mod streaming;

pub type InvocationFuture =
    Pin<Box<dyn Future<Output = Result<Value, InvocationError>> + Send + 'static>>;
#[derive(Clone, Debug, Error)]
pub enum InvocationError {
    #[error("{}",.0.message())]
    Failed(ErrorValue),
    #[error("the local operation was cancelled")]
    Cancelled,
}

/// Arguments have crossed all attached contracts. Implementations return their own provenance.
/// Cancellation is cooperative; implementations obey the Executor lease contract.
pub trait Invoker: Send + Sync + 'static {
    fn invoke(&self, call: Call, cancellation: CancellationToken) -> InvocationFuture;
    fn supports_trace(&self, _profile: &str) -> bool {
        false
    }
    fn invoke_observed(
        &self,
        call: Call,
        cancellation: CancellationToken,
        _trace: crate::trace::TraceSink,
    ) -> InvocationFuture {
        self.invoke(call, cancellation)
    }
}
#[derive(Clone, Debug)]
pub struct Call {
    pub authority: crate::environments::InvocationAuthority,
    pub run: Run,
    pub capability: Arc<Capability>,
    pub arguments: IndexMap<String, Value>,
}

/// A trusted embedding owner's local execution domain. Reuse the identity on restore;
/// share it only between providers permitted to consume each other's values.
#[derive(Clone, Debug)]
pub struct LocalScope(String);

#[derive(Debug, Error)]
#[error(
    "local execution scope must be nonblank, at most 240 bytes and contain no control characters"
)]
pub struct InvalidLocalScope;

impl LocalScope {
    pub fn new(identity: &str) -> Result<Self, InvalidLocalScope> {
        if identity.trim().is_empty()
            || identity.len() > 240
            || identity.chars().any(char::is_control)
        {
            return Err(InvalidLocalScope);
        }
        Ok(Self(format!("local:{identity}")))
    }
    pub fn origin(&self) -> &str {
        &self.0
    }
}

#[derive(Clone)]
enum ExecutionScope {
    Unbound,
    Local(LocalScope),
    Environment {
        binding: wes_core::environments::Binding,
        authority: Option<crate::environments::Authority>,
    },
}

#[derive(Clone)]
pub struct Provider {
    scope: ExecutionScope,
    description: Arc<ProviderDescription>,
    invoker: Arc<dyn Invoker>,
    streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
    conversations: Option<Arc<dyn crate::conversations::InteractiveInvoker>>,
}
impl Provider {
    pub fn description(&self) -> &Arc<ProviderDescription> {
        &self.description
    }
}
impl fmt::Debug for Provider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Provider")
            .field("name", &self.description.name())
            .finish_non_exhaustive()
    }
}

/// One owner updates metadata and callable handles together. Analysers only borrow catalogue().
#[derive(Clone, Default)]
pub struct Providers {
    local_scope: Option<LocalScope>,
    catalogue: Catalogue,
    providers: IndexMap<String, Arc<Provider>>,
}
impl Providers {
    pub(crate) fn register_environment(
        &mut self,
        product: &crate::imports::ImportProduct,
        binding: wes_core::environments::Binding,
        authority: Option<crate::environments::Authority>,
    ) -> Result<(), crate::imports::ImportError> {
        let bound = product.bind_environment(&binding, authority.clone())?;
        let product = bound.as_ref().unwrap_or(product);
        self.register_scoped(
            ExecutionScope::Environment { binding, authority },
            product.description().clone(),
            product.invoker().clone(),
            product.streams().cloned(),
            product.conversations().cloned(),
        );
        Ok(())
    }
    /// Metadata/port registry without implicit execution permission. Managed providers
    /// receive their scope when registered against an environment.
    pub fn new() -> Self {
        Self::default()
    }
    /// Explicitly authorize registered ports within the embedding owner's local domain.
    pub fn local(scope: LocalScope) -> Self {
        Self {
            local_scope: Some(scope),
            ..Self::default()
        }
    }
    pub fn catalogue(&self) -> &Catalogue {
        &self.catalogue
    }
    pub fn register(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
    ) -> Option<Arc<Provider>> {
        self.register_ports(description, invoker, None)
    }
    /// Both callable ports are captured with the same immutable metadata revision. Registration
    /// performs no invocation; a streaming declaration alone does not supply a stream implementation.
    pub fn register_ports(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
    ) -> Option<Arc<Provider>> {
        self.register_all_ports(description, invoker, streams, None)
    }
    /// Capture all callable ports in one immutable metadata revision. No provider is entered here.
    pub fn register_all_ports(
        &mut self,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
        conversations: Option<Arc<dyn crate::conversations::InteractiveInvoker>>,
    ) -> Option<Arc<Provider>> {
        self.register_scoped(
            self.local_scope
                .clone()
                .map_or(ExecutionScope::Unbound, ExecutionScope::Local),
            description,
            invoker,
            streams,
            conversations,
        )
    }
    fn register_scoped(
        &mut self,
        scope: ExecutionScope,
        description: ProviderDescription,
        invoker: Arc<dyn Invoker>,
        streams: Option<Arc<dyn crate::streams::StreamingInvoker>>,
        conversations: Option<Arc<dyn crate::conversations::InteractiveInvoker>>,
    ) -> Option<Arc<Provider>> {
        let name = description.name().to_owned();
        self.catalogue.register(description);
        let description = self
            .catalogue
            .provider(&name)
            .expect("just registered")
            .clone();
        self.providers.insert(
            name,
            Arc::new(Provider {
                scope,
                description,
                invoker,
                streams,
                conversations,
            }),
        )
    }
    pub fn unregister(&mut self, name: &str) -> Option<Arc<Provider>> {
        self.catalogue.unregister(name);
        self.providers.shift_remove(name)
    }
    /// Resolving and binding must happen against the same registry revision. A concurrent replacement
    /// is rejected rather than accidentally pointing a previously analysed call at another invoker.
    pub fn interactive_names(&self) -> Vec<String> {
        self.providers
            .iter()
            .filter(|(_, p)| p.conversations.is_some())
            .map(|(name, _)| name.clone())
            .collect()
    }
    pub fn bind_finite(&self, invocation: Invocation) -> Result<BoundCall, BindError> {
        if invocation.interactive || invocation.capability.streaming {
            return Err(BindError::NotFinite);
        }
        self.bind(invocation)
    }
    pub fn bind_stream(&self, invocation: Invocation) -> Result<BoundCall, BindError> {
        if invocation.interactive || !invocation.capability.streaming {
            return Err(BindError::NotStream);
        }
        let bound = self.bind(invocation)?;
        if bound.provider.streams.is_none() {
            return Err(BindError::MissingStreamPort);
        }
        Ok(bound)
    }
    pub fn bind_interactive(&self, invocation: Invocation) -> Result<BoundCall, BindError> {
        if !invocation.interactive || invocation.capability.streaming {
            return Err(BindError::NotInteractive);
        }
        let bound = self.bind(invocation)?;
        if bound.provider.conversations.is_none() {
            return Err(BindError::MissingInteractivePort);
        }
        Ok(bound)
    }
    fn bind(&self, invocation: Invocation) -> Result<BoundCall, BindError> {
        let provider = self
            .providers
            .get(invocation.provider.name())
            .ok_or(BindError::ProviderChanged)?;
        if !Arc::ptr_eq(&provider.description, &invocation.provider)
            || !provider
                .description
                .capability(&invocation.capability.path)
                .is_some_and(|capability| Arc::ptr_eq(capability, &invocation.capability))
        {
            return Err(BindError::ProviderChanged);
        }
        if let Some(profile) = invocation.trace_profile.as_deref()
            && (profile.is_empty()
                || profile.len() > 64
                || !provider.invoker.supports_trace(profile)
                || invocation.interactive
                || invocation.capability.streaming)
        {
            return Err(BindError::UnsupportedTrace);
        }
        Ok(BoundCall {
            definition_changed: false,
            invocation,
            provider: provider.clone(),
            context: Box::default(),
        })
    }
}
#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
pub enum BindError {
    #[error("this provider does not support the requested finite trace profile")]
    UnsupportedTrace,
    #[error("the provider changed after analysis; resolve this call again")]
    ProviderChanged,
    #[error("streaming and interactive calls require their own execution ports")]
    NotFinite,
    #[error("the call does not select a noninteractive stream")]
    NotStream,
    #[error("the provider does not implement its declared stream")]
    MissingStreamPort,
    #[error("the call does not select a nonstreaming conversation")]
    NotInteractive,
    #[error("the provider does not implement interactive execution")]
    MissingInteractivePort,
}

/// The provider handle is captured once. Re-importing or unregistering cannot retarget old nodes.
#[derive(Clone)]
pub struct BoundCall {
    definition_changed: bool,
    invocation: Invocation,
    provider: Arc<Provider>,
    context: Box<CallContext>,
}
#[derive(Clone, Default)]
struct CallContext {
    source_definition: Option<Arc<()>>,
    stream_budget: Option<crate::streams::delivery::Budget>,
    recording_schema: Option<wes_core::contracts::ResolvedContractBundle>,
    authority: crate::environments::InvocationAuthority,
    pipe_input: Option<crate::graph::OutputRef>,
    admission: Option<AdmittedCommand>,
    traces: Option<crate::trace::Traces>,
}
impl BoundCall {
    pub(crate) fn set_source_definition(&mut self, definition: Arc<()>) {
        self.context.source_definition = Some(definition);
    }
    pub(crate) fn with_recording_schema(
        mut self,
        registry: &wes_core::contracts::ContractRegistry,
    ) -> Self {
        if self.streaming() {
            let shape = &self.invocation.capability.result;
            self.context.recording_schema = registry
                .resolve(&shape.to_string())
                .ok()
                .filter(|contract| contract.shape() == *shape)
                .and_then(|contract| {
                    wes_core::contracts::ResolvedContractBundle::capture(
                        contract,
                        Default::default(),
                    )
                    .ok()
                });
        }
        self
    }
    pub(crate) fn recording_schema(&self) -> Option<&wes_core::contracts::ResolvedContractBundle> {
        self.context.recording_schema.as_ref()
    }

    pub(crate) fn set_stream_budget(&mut self, budget: Option<crate::streams::delivery::Budget>) {
        self.context.stream_budget = budget;
    }
    pub(crate) fn set_authority(&mut self, authority: crate::environments::InvocationAuthority) {
        self.context.authority = authority;
    }
    pub(crate) fn with_pipe_input(mut self, input: Option<crate::graph::OutputRef>) -> Self {
        self.context.pipe_input = input;
        self
    }
    pub(crate) fn set_traces(&mut self, traces: crate::trace::Traces) {
        self.context.traces = Some(traces);
    }
    fn failure_policy(&self, inputs: &IndexMap<NodeId, Value>) -> wes_core::flow::FlowPolicy {
        let carried = inputs
            .values()
            .chain(
                self.invocation
                    .inputs
                    .values()
                    .flat_map(Input::literal_values),
            )
            .fold(Provenance::default(), |p, value| {
                p.with_policy(value.provenance().policy())
            });
        self.output_provenance(&carried).policy().clone()
    }
    fn origin(&self) -> Option<String> {
        match &self.provider.scope {
            ExecutionScope::Environment {
                binding,
                authority: Some(authority),
            } => authority.origin(binding.environment().identity()),
            ExecutionScope::Local(scope) => Some(scope.origin().to_owned()),
            _ => None,
        }
    }
    fn require_dispatch(
        &self,
        carried: &Provenance,
    ) -> Result<(), crate::environments::DispatchDenied> {
        use crate::environments::DispatchDenied;
        match &self.provider.scope {
            ExecutionScope::Environment {
                binding,
                authority: Some(authority),
            } => authority.require_dispatch(
                binding.environment().identity(),
                carried.policy(),
                binding.environment().revision(),
            ),
            ExecutionScope::Local(scope)
                if !carried.policy().is_unknown()
                    && carried
                        .policy()
                        .origins()
                        .iter()
                        .all(|origin| origin == scope.origin()) =>
            {
                Ok(())
            }
            ExecutionScope::Local(_) => Err(DispatchDenied::Transfer),
            _ => Err(DispatchDenied::Unavailable),
        }
    }
    fn output_provenance(&self, carried: &Provenance) -> Provenance {
        let mut policy = match self.origin() {
            Some(origin) => carried.policy().clone().from_origin(origin),
            None => carried.policy().clone().unknown(),
        };
        if let Some(binding) = self.environment() {
            policy = policy.join(&binding.import().declaration().output_policy.policy());
        }
        carried.clone().with_policy(&policy)
    }
    pub(crate) fn environment_available(&self) -> bool {
        match &self.provider.scope {
            ExecutionScope::Local(_) => true,
            ExecutionScope::Environment {
                binding,
                authority: Some(authority),
            } => authority.available(binding.environment().identity()),
            _ => false,
        }
    }
    pub fn environment(&self) -> Option<&wes_core::environments::Binding> {
        match &self.provider.scope {
            ExecutionScope::Environment { binding, .. } => Some(binding),
            _ => None,
        }
    }
    pub(crate) fn output_policy(&self) -> wes_core::flow::FlowPolicy {
        self.environment().map_or_else(Default::default, |b| {
            b.import().declaration().output_policy.policy()
        })
    }
    pub(crate) fn with_inputs(mut self, inputs: IndexMap<String, Input>) -> Self {
        self.definition_changed = true;
        self.invocation.inputs = inputs;
        self
    }
    /// The executor still checks journal and node identity. A receipt for a different command
    /// cannot authorize this run; attaching metadata alone does not invoke anything.
    pub fn with_admission(mut self, admission: AdmittedCommand) -> Self {
        self.context.admission = Some(admission);
        self
    }
    pub fn streaming(&self) -> bool {
        self.invocation.capability.streaming
    }
    pub fn interactive(&self) -> bool {
        self.invocation.interactive
    }
    pub fn definition_changed(&self) -> bool {
        self.definition_changed
    }
    pub fn invocation(&self) -> &Invocation {
        &self.invocation
    }
    pub fn traits(&self) -> ExecutionTraits {
        ExecutionTraits {
            pure: false,
            repeatable: self.invocation.capability.safety == Safety::Safe && !self.interactive(),
            bounded: !self.interactive(),
        }
    }
    pub fn dependencies(&self) -> impl Iterator<Item = crate::graph::OutputRef> + '_ {
        self.invocation
            .inputs
            .values()
            .flat_map(|input| input.dependencies().into_iter().cloned())
            .chain(self.context.pipe_input.iter().cloned())
    }
}
impl fmt::Debug for BoundCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("BoundCall")
            .field("provider", &self.provider.description.name())
            .field("capability", &self.invocation.capability.path)
            .finish_non_exhaustive()
    }
}

/// Validated invocation executor. Persistent mode requires matching acknowledged admission.
/// Finite calls, streams and conversations use separate execution ports.
#[derive(Clone)]
pub struct CallExecutor {
    journal: Option<CallJournal>,
}
impl CallExecutor {
    pub(crate) fn prepare_stream(
        &self,
        ticket: RunTicket<BoundCall>,
        cancellation: CancellationToken,
    ) -> streaming::PreparationFuture {
        streaming::prepare_source(self.journal.clone(), ticket, cancellation)
    }
    pub fn ephemeral() -> Self {
        Self { journal: None }
    }
    pub fn recorded(journal: CallJournal) -> Self {
        Self {
            journal: Some(journal),
        }
    }
}
impl Executor<BoundCall> for CallExecutor {
    fn interactive(&self, payload: &BoundCall) -> bool {
        payload.interactive()
    }
    fn execute_interactive(
        &self,
        ticket: RunTicket<BoundCall>,
        cancellation: CancellationToken,
    ) -> InteractiveExecutionFuture {
        interactive::execute(self.journal.clone(), ticket, cancellation)
    }
    fn streaming(&self, payload: &BoundCall) -> bool {
        payload.streaming()
    }
    fn execute_stream(
        &self,
        ticket: RunTicket<BoundCall>,
        cancellation: CancellationToken,
    ) -> StreamExecutionFuture {
        streaming::execute(self.journal.clone(), ticket, cancellation)
    }

    fn execute(
        &self,
        ticket: RunTicket<BoundCall>,
        cancellation: CancellationToken,
    ) -> ExecutionFuture {
        let journal = self.journal.clone();
        let policy = ticket.payload.failure_policy(&ticket.inputs);
        let work = async move {
            if ticket.payload.streaming() || ticket.payload.interactive() {
                return Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "This call requires its stream or conversation execution port.",
                    None,
                ))
                .into();
            }
            // Validation can walk large values. Keep it off asynchronous scheduler threads, and
            // join its physical exit even when cancelled, just like any other blocking work.
            let validation_cancel = cancellation.clone();
            let prepared = tokio::task::spawn_blocking(move || {
                prepare(
                    &ticket.payload.invocation,
                    &ticket.inputs,
                    &validation_cancel,
                )
                .map(|(arguments, mut carried)| {
                    // A pipe remains a control/data dependency even when the stage does not
                    // project any fields. Do not shed its private/transfer restrictions.
                    if let Some(input) = &ticket.payload.context.pipe_input
                        && let Some(value) = ticket.inputs.get(&input.node)
                    {
                        carried = carried.with_policy(value.provenance().policy());
                    }
                    (ticket.payload, ticket.run, arguments, carried)
                })
            })
            .await;
            let (bound, run, arguments, carried) = match prepared {
                Ok(Ok(prepared)) => prepared,
                Ok(Err(error)) => return error.outcome().into(),
                Err(_) => {
                    return Outcome::Failed(
                        RuntimeCode::ExecutionFailed
                            .error("Input validation terminated unexpectedly.", None),
                    )
                    .into();
                }
            };
            if cancellation.is_cancelled() {
                return InvocationError::Cancelled.outcome().into();
            }
            let calling = match lifecycle::begin(&journal, &bound, &run, &cancellation).await {
                Ok(calling) => calling,
                Err(report) => return *report,
            };
            // An accepted writer request is joined even when cancellation happens while waiting.
            // In that case close this local attempt without ever entering the provider.
            let protected = bound.output_provenance(&carried);
            let trace = if let Some(profile) = bound.invocation.trace_profile.as_deref() {
                bound
                    .context
                    .traces
                    .as_ref()
                    .and_then(|traces| traces.begin(&run, profile, protected.clone()))
            } else {
                None
            };
            let observation = trace.clone();
            let outcome = if bound.invocation.trace_profile.is_some() && trace.is_none() {
                Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Trace capacity is unavailable; the provider was not called.",
                    None,
                ))
            } else if let Err(reason) = bound.require_dispatch(&carried) {
                environment_denied(reason)
            } else if cancellation.is_cancelled() {
                InvocationError::Cancelled.outcome()
            } else {
                let provider = bound.provider;
                let call = Call {
                    authority: bound.context.authority.clone(),
                    run,
                    capability: bound.invocation.capability.clone(),
                    arguments,
                };
                // Join the actual provider future, including synchronous invoke panics. A panic
                // payload may contain secrets and is never copied into a value or notice.
                let result = tokio::spawn(async move {
                    if cancellation.is_cancelled() {
                        return Err(InvocationError::Cancelled);
                    }
                    match observation {
                        Some(trace) => {
                            provider
                                .invoker
                                .invoke_observed(call, cancellation, trace)
                                .await
                        }
                        None => provider.invoker.invoke(call, cancellation).await,
                    }
                })
                .await;
                match result {
                    Ok(Ok(value)) => {
                        let provenance = value
                            .provenance()
                            .inheriting(&protected)
                            .cautioned(bound.invocation.cautions.iter().cloned());
                        Outcome::Produced(if &provenance == value.provenance() {
                            value
                        } else {
                            value.with_provenance(provenance)
                        })
                    }
                    Ok(Err(error)) => error.outcome(),
                    Err(_) => Outcome::Failed(
                        RuntimeCode::ExecutionFailed
                            .error("The provider terminated unexpectedly.", None),
                    ),
                }
            };
            let mut report: ExecutionReport = outcome.with_policy(protected.policy()).into();
            if let Outcome::Failed(error) = &report.outcome
                && error.code() == wes_core::ErrorValue::REMOTE_OUTCOME_UNKNOWN
            {
                // Cancellation can revoke node publication before physical exec returns. Keep
                // remote uncertainty as an operational notice even for an obsolete run.
                report.notices.push(error.clone());
            }
            if let Some(trace) = trace {
                let (state, code, provenance) = match &report.outcome {
                    Outcome::Produced(value) => ("completed", "", value.provenance().clone()),
                    Outcome::Failed(error) | Outcome::Incomplete { error, .. } => (
                        "failed",
                        error.code(),
                        wes_core::Provenance::default().with_policy(error.policy()),
                    ),
                    _ => ("cancelled", "", protected.clone()),
                };
                trace.emit(
                    "execution.finished",
                    crate::trace::record(
                        "ExecutionCompletion",
                        [
                            ("state".into(), crate::trace::text(state)),
                            ("code".into(), crate::trace::text(code)),
                        ],
                        provenance,
                    ),
                );
                trace.finish(match &report.outcome {
                    Outcome::Produced(_) => "completed",
                    Outcome::Failed(_) | Outcome::Incomplete { .. } => "failed",
                    _ => "cancelled",
                });
                if let Some(record) = trace.persistent_record() {
                    if let Some(journal) = &journal {
                        if journal.record_trace(record).await.is_ok() {
                            trace.persistence("recorded");
                        } else {
                            trace.persistence("failed");
                            report.notices.push(RuntimeCode::RecordingFailed.error(
                                "Inspection completed, but its trace could not be recorded.",
                                None,
                            ));
                        }
                    }
                } else {
                    trace.persistence("private");
                }
            }
            lifecycle::finish(journal, calling, report).await
        };
        Box::pin(async move {
            let mut report: ExecutionReport = work.await;
            report.outcome = report.outcome.with_policy(&policy);
            report.notices = report
                .notices
                .into_iter()
                .map(|e| e.with_policy(&policy))
                .collect();
            report
        })
    }
}

fn recording_denied(cancellation: &CancellationToken) -> ExecutionReport {
    let error = RuntimeCode::RecordingFailed.error(
        "Required call admission or recovery recording could not be acknowledged; the provider was not invoked.", None);
    if cancellation.is_cancelled() {
        ExecutionReport {
            outcome: InvocationError::Cancelled.outcome(),
            notices: vec![error],
            stream_start: None,
            holds: vec![],
            progress: None,
        }
    } else {
        Outcome::Failed(error).into()
    }
}
fn environment_denied(reason: crate::environments::DispatchDenied) -> Outcome {
    Outcome::Failed(
        ErrorValue::new(
            ErrorId::new(Uuid::new_v4().to_string()).expect("UUID"),
            reason.code(),
            reason.message(),
            vec![],
            None,
        )
        .expect("bounded environment refusal"),
    )
}

#[cfg(test)]
#[test]
fn environment_refusals_preserve_their_code_without_a_runtime_wrapper() {
    use crate::environments::DispatchDenied;
    for reason in [
        DispatchDenied::Closed,
        DispatchDenied::Disabled,
        DispatchDenied::Restored,
        DispatchDenied::Unavailable,
        DispatchDenied::Transfer,
    ] {
        let Outcome::Failed(error) = environment_denied(reason) else {
            panic!()
        };
        assert_eq!(error.code(), "ENV020");
        assert_eq!(error.message(), reason.message());
        assert!(!error.message().contains("ENV020:"));
    }
}

fn wall_time() -> Option<wes_core::Timestamp> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let duration = SystemTime::now().duration_since(UNIX_EPOCH).ok()?;
    wes_core::Timestamp::new(
        i64::try_from(duration.as_secs()).ok()?,
        duration.subsec_nanos(),
    )
    .ok()
}

impl InvocationError {
    fn outcome(self) -> Outcome {
        match self {
            Self::Failed(error) => Outcome::Failed(error),
            Self::Cancelled => Outcome::Cancelled(
                RuntimeCode::Cancelled.error("The local operation was cancelled.", None),
            ),
        }
    }
}

fn prepare(
    invocation: &Invocation,
    inputs: &IndexMap<NodeId, Value>,
    cancellation: &CancellationToken,
) -> Result<(IndexMap<String, Value>, Provenance), InvocationError> {
    if cancellation.is_cancelled() {
        return Err(InvocationError::Cancelled);
    }
    let mut arguments =
        crate::plan::resolve_arguments(&invocation.inputs, inputs).map_err(|(name, error)| {
            InvocationError::Failed(error.error(RuntimeCode::InputFailed, &name))
        })?;
    let mut origins = vec![];
    let mut policy = wes_core::flow::FlowPolicy::default();
    for (name, input) in &invocation.inputs {
        let value = &arguments[name];
        for output in input.dependencies() {
            origins.push(inputs[&output.node].provenance());
        }
        // Calc subcalls deliver captured values as literals. Literal is a planning mechanism,
        // not proof that the data was written by the user or is free of flow restrictions.
        policy = policy.join(value.provenance().policy());
        if !value.shape().is_assignable_to(&Shape::Unknown) {
            return Err(InvocationError::Failed(RuntimeCode::InputFailed.error(
                "Provider arguments require data values; management values are not data.",
                None,
            )));
        }
        if !value.data().is_inline() {
            return Err(InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                "Provider arguments require materialized data; collect Iter explicitly.",
                None,
            )));
        }
    }
    for parameter in &invocation.capability.parameters {
        if parameter.required && !arguments.contains_key(&parameter.name) {
            return Err(missing(&parameter.name));
        }
    }
    for (name, contracts) in &invocation.guards {
        let value = arguments.get(name).ok_or_else(|| missing(name))?;
        let expected = invocation
            .capability
            .parameter(name)
            .map_or(&Shape::Unknown, |p| &p.shape);
        let validated = boundary::require(name, contracts, expected, value, &|| {
            cancellation.is_cancelled()
        })
        .map_err(|error| {
            if value.provenance().policy().is_confidential() {
                InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                    "Private argument failed validation; details withheld.",
                    None,
                ))
            } else {
                boundary_error(error)
            }
        })?;
        arguments.insert(name.clone(), validated);
    }
    // References select from the accepted runtime snapshot. Replay has no hydrated input shapes,
    // and projections may differ from preparation metadata. Validate every referenced value after
    // explicit contract guards refine it, before lifecycle admission or provider execution.
    for parameter in &invocation.capability.parameters {
        if matches!(
            invocation.inputs.get(&parameter.name),
            Some(
                Input::FromNode(_)
                    | Input::FieldPath { .. }
                    | Input::Record(_)
                    | Input::List { .. }
            )
        ) && let Some(value) = arguments.get(&parameter.name)
            && !value.shape().is_assignable_to(&parameter.shape)
        {
            return Err(InvocationError::Failed(RuntimeCode::ExecutionFailed.error(
                format!("Parameter '{}:' requires {}. Use :type check $value as:\"{}\" before passing a loosely typed result.", parameter.name, parameter.shape, parameter.shape),
                None,
            )));
        }
    }
    Ok((
        arguments,
        Provenance::agreed_by(origins).with_policy(&policy),
    ))
}
fn missing(argument: &str) -> InvocationError {
    InvocationError::Failed(
        RuntimeCode::ExecutionFailed
            .error(format!("argument '{argument}' has no captured value"), None),
    )
}
fn boundary_error(error: BoundaryError) -> InvocationError {
    let message = error.to_string();
    let (code, issues) = match error {
        BoundaryError::Cancelled(_) => return InvocationError::Cancelled,
        BoundaryError::Invalid { issues, .. } => ("TYP005", issues),
        BoundaryError::Declaration(error) => (error.code, vec![]),
    };
    InvocationError::Failed(
        ErrorValue::new(
            ErrorId::new(Uuid::new_v4().to_string()).expect("UUID is nonblank"),
            code,
            message,
            issues,
            None,
        )
        .expect("validated boundary issue format"),
    )
}
