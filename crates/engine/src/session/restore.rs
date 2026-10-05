//! Joined startup reconstruction; no executor, append, live type lookup or automatic retry.
use super::{
    Cells, RecordingMode, RecoveredValue, SessionError, SessionHandle, SessionLog, SessionStorage,
    SessionTask, SessionValues, SourceDiagnostics, SourceInput, SubmissionResult,
};
use crate::{
    driver::CancellationToken,
    graph::{NodeId, NodeState},
    history::{
        CallRecord, HistoryCaptureLimits, HistoryImage, JournalEntry, RecoveryEntry, RestoreIndex,
        unresolved_calls,
    },
    source::SourceError,
    storage::ValueHandle,
    type_sources::TypeSourceReader,
    value_size::value_charge,
    workspace::{ReplayWorkspace, Workspace, WorkspaceError},
};
use indexmap::{IndexMap, IndexSet};
use std::{num::NonZeroUsize, sync::Arc};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum RestoreError {
    #[error("view definitions could not be restored: {0}")]
    Views(String),
    #[error("history contains invalid live snapshot evidence")]
    LiveSnapshot,
    #[error("history contains invalid or conflicting trace evidence")]
    Trace,
    #[error("historical environment change {index} could not be reconstructed: {source}")]
    Environment {
        index: usize,
        #[source]
        source: wes_core::environments::EnvironmentError,
    },
    #[error("history contains conflicting environment records for one identity")]
    ConflictingEnvironment,
    #[error("session reconstruction exceeds its {0} budget")]
    Capacity(&'static str),
    #[error("session reconstruction was cancelled; no session was started")]
    Cancelled,
    #[error("history contains conflicting command records for one submission identity")]
    ConflictingCommand,
    #[error("history contains conflicting log records for one event identity")]
    ConflictingLog,
    #[error("historical command {index} could not be reconstructed: {source}")]
    Source {
        index: usize,
        #[source]
        source: SourceError,
    },
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
    #[error(transparent)]
    Identity(#[from] SessionError),
    #[error(transparent)]
    Recording(#[from] crate::calls::CallJournalError),
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RestoreProblemKind {
    MissingValue,
    UnreadableValue,
    NoValueStore,
    ValueCapacity,
}
#[derive(Clone, Debug)]
pub struct RestoreProblem {
    pub node: NodeId,
    pub handle: ValueHandle,
    pub kind: RestoreProblemKind,
}
#[derive(Clone, Debug, Default)]
pub struct RestoreReport {
    pub environments: usize,
    pub duplicate_environments: usize,
    pub commands: usize,
    pub duplicate_commands: usize,
    pub duplicate_logs: usize,
    /// Recovery identities without recoverable source remain tombstones, never retry authority.
    pub unidentified_cells: usize,
    pub values: usize,
    /// Changed declarations lacking evidence of a subsequent run remain stale.
    pub unconfirmed_changes: Vec<NodeId>,
    /// Subset able to call providers; pure stale declarations are not external-outcome warnings.
    pub unconfirmed_external_changes: Vec<NodeId>,
    pub problems: Vec<RestoreProblem>,
    /// Local attempts lacking their exact completion record. No remote-outcome inference.
    pub interrupted: Vec<CallRecord>,
}
/// A reconstructed session stays inert until explicitly spawned. Its origin receipts, recording
/// mode and projections cannot accidentally be paired with another workspace or writer.
pub struct RestoredSession {
    workspace: Workspace,
    recording: RecordingMode,
    cells: Cells,
    log: SessionLog,
    values: Option<SessionValues>,
    report: RestoreReport,
}
impl RestoredSession {
    pub fn report(&self) -> &RestoreReport {
        &self.report
    }
    pub fn workspace(&self) -> &Workspace {
        &self.workspace
    }
    /// Starting the actor does not refresh held nodes. The session snapshot retains the startup
    /// report; the underlying recovery journal remains the persistent uncertainty source.
    pub fn spawn(
        self,
        reader: Arc<dyn TypeSourceReader>,
        concurrency: NonZeroUsize,
    ) -> Result<(SessionHandle, SessionTask), SessionError> {
        self.spawn_configured(reader, concurrency, None, None)
    }
    pub fn spawn_with_actions(
        self,
        reader: Arc<dyn TypeSourceReader>,
        concurrency: NonZeroUsize,
        actions: super::WorkspaceActions,
    ) -> Result<(SessionHandle, SessionTask), SessionError> {
        self.spawn_configured(reader, concurrency, Some(actions), None)
    }
    /// Multiple application sessions share the same bounded physical execution pool.
    pub fn spawn_with_capacity(
        self,
        reader: Arc<dyn TypeSourceReader>,
        concurrency: NonZeroUsize,
        actions: super::WorkspaceActions,
        capacity: crate::driver::ExecutionCapacity,
    ) -> Result<(SessionHandle, SessionTask), SessionError> {
        self.spawn_configured(reader, concurrency, Some(actions), Some(capacity))
    }
    fn spawn_configured(
        self,
        reader: Arc<dyn TypeSourceReader>,
        concurrency: NonZeroUsize,
        actions: Option<super::WorkspaceActions>,
        capacity: Option<crate::driver::ExecutionCapacity>,
    ) -> Result<(SessionHandle, SessionTask), SessionError> {
        super::spawn_owned(
            self.workspace,
            self.recording,
            reader,
            concurrency,
            super::SessionSeed {
                capacity,
                actions,
                cells: self.cells,
                log: self.log,
                values: self.values,
                restoration: Some(Arc::new(self.report)),
            },
        )
    }
}

/// The composition root must capture `history` from the same owned writer used by `recording` and
/// join startup before accepting requests. Cancellation is checked between joined preparation/read
/// operations. On any structural/identity failure no partially restored session is returned.
pub async fn restore(
    workspace: Workspace,
    recording: RecordingMode,
    storage: Option<SessionStorage>,
    history: HistoryImage,
    cancellation: CancellationToken,
) -> Result<RestoredSession, RestoreError> {
    check_cancelled(&cancellation)?;
    let limits = HistoryCaptureLimits::default();
    if history
        .journal()
        .len()
        .saturating_add(history.recovery().len())
        > limits.records
        || history.charged_bytes() > limits.bytes
    {
        return Err(RestoreError::Capacity("history"));
    }
    let checkpoint = history.checkpoint();
    if let RecordingMode::Required(journal) = &recording {
        journal.checkpoint(checkpoint)?;
    }
    let mut request_ids = std::collections::BTreeMap::new();
    for entry in history.journal() {
        if let JournalEntry::Requested(record) = entry {
            record
                .validate()
                .map_err(|_| RestoreError::ConflictingCommand)?;
            if request_ids
                .insert((&record.namespace, &record.request), record)
                .is_some_and(|old| old != record)
            {
                return Err(RestoreError::ConflictingCommand);
            }
        }
    }
    let mut replay = ReplayWorkspace::new(workspace)?;
    let accepted: IndexSet<_> = history
        .recovery()
        .iter()
        .filter_map(|entry| match entry {
            RecoveryEntry::Accepted { cell } => Some(cell.as_str()),
            _ => None,
        })
        .collect();
    let mut report = RestoreReport::default();
    let mut cells = Cells::default();
    let mut submissions = IndexMap::new();
    let mut submission_order = std::collections::BTreeMap::new();
    for entry in history.journal() {
        if let JournalEntry::Submitted(record) = entry {
            record
                .input()
                .map_err(|_| RestoreError::ConflictingCommand)?;
            if let Some(previous) = submissions.insert(record.cell.as_str(), record) {
                if previous != record {
                    return Err(RestoreError::ConflictingCommand);
                }
            } else if submission_order
                .insert(record.order, record.cell.as_str())
                .is_some()
            {
                return Err(RestoreError::ConflictingCommand);
            }
        }
    }
    let mut logs = IndexMap::new();
    let mut duplicate_logs = IndexSet::new();
    for (position, entry) in history.journal().iter().enumerate() {
        let identity = match entry {
            JournalEntry::Observed(record) => record.id(),
            JournalEntry::Diagnosed(record) => record.id(),
            JournalEntry::Noticed(record) => record.id(),
            _ => continue,
        };
        if let Some(previous) = logs.get(identity) {
            if *previous != entry {
                return Err(RestoreError::ConflictingLog);
            }
            duplicate_logs.insert(position);
            report.duplicate_logs += 1;
        } else {
            logs.insert(identity, entry);
        }
    }
    let mut commands: IndexMap<&str, &crate::history::CommandRecord> = IndexMap::new();
    let mut repeat_definitions = IndexMap::new();
    let mut created_at = IndexMap::new();
    let mut changed_at = IndexMap::new();
    let mut definitions_changed_at = IndexMap::new();
    let mut remaining_changes = 1_000_000usize;
    let mut environment_records = IndexMap::new();
    let mut trace_records = IndexMap::new();
    let traced_attempts: IndexSet<_> = history
        .recovery()
        .iter()
        .filter_map(|entry| match entry {
            RecoveryEntry::Calling(call) => Some((&call.node, &call.run)),
            _ => None,
        })
        .collect();
    for (position, entry) in history.journal().iter().enumerate() {
        if let JournalEntry::Retired(record) = entry {
            replay.reserve_retired(record)?;
            continue;
        }
        if let JournalEntry::Environments(record) = entry {
            check_cancelled(&cancellation)?;
            if let Some(previous) = environment_records.get(record.id()) {
                if *previous != record {
                    return Err(RestoreError::ConflictingEnvironment);
                }
                report.duplicate_environments += 1;
                continue;
            }
            replay
                .restore_environments(record)
                .await
                .map_err(|source| RestoreError::Environment {
                    index: position,
                    source,
                })?;
            check_cancelled(&cancellation)?;
            environment_records.insert(record.id(), record);
            report.environments += 1;
            continue;
        }
        if let JournalEntry::Trace(record) = entry {
            if let Some(previous) = trace_records.insert(record.run.clone(), record) {
                if previous != record {
                    return Err(RestoreError::Trace);
                }
                continue;
            }
            if !traced_attempts.contains(&(&record.node, &record.run)) {
                return Err(RestoreError::Trace);
            }
            if !created_at.contains_key(&record.node) || !replay.workspace().traces.restore(record)
            {
                return Err(RestoreError::Trace);
            }
            continue;
        }
        let JournalEntry::Command(command) = entry else {
            continue;
        };
        check_cancelled(&cancellation)?;
        if let Some(previous) = commands.get(command.cell.as_str()) {
            if *previous != command {
                return Err(RestoreError::ConflictingCommand);
            }
            report.duplicate_commands += 1;
            continue;
        }
        if let Some(origin) = &command.revision_of {
            if !commands.contains_key(origin.as_str())
                || commands
                    .values()
                    .any(|c| c.revision_of.as_ref() == Some(origin))
            {
                return Err(RestoreError::ConflictingCommand);
            }
        }
        commands.insert(command.cell.as_str(), command);
        let mut prepared = replay
            .prepare(command, cancellation.clone())
            .await
            .map_err(|source| RestoreError::Source {
                index: position,
                source,
            })?;
        check_cancelled(&cancellation)?;
        if let RecordingMode::Required(journal) = &recording {
            let admission = journal.recover(
                command,
                checkpoint,
                accepted.contains(command.cell.as_str()),
            )?;
            prepared.batch = prepared.batch.with_admission(admission)?;
        }
        let changed: IndexSet<_> = prepared.batch.changed_nodes().cloned().collect();
        for node in &changed {
            definitions_changed_at.insert(node.clone(), position);
        }
        let applied = replay.apply(prepared)?;
        if replay.workspace().runtime().graph().nodes().len() > 10_000 {
            return Err(RestoreError::Capacity("nodes"));
        }
        for node in &command.nodes {
            created_at.insert(node.clone(), position);
        }
        // Capture the declaration's identity before any later :change replaces it.
        // A restored definition grants no execution authority; rerun still rechecks context,
        // authority, inputs, leases and acknowledgement through the normal admission path.
        if command.text == command.replay && !command.nodes.is_empty() {
            let parsed = wes_language::parse_with_calculation(
                &wes_language::SourceText::new("<repeat-definition>", &command.replay),
                command
                    .calculation_package
                    .as_ref()
                    .map(|s| wes_language::calc::Package::load(s).map(Arc::new))
                    .transpose()
                    .map_err(|_| RestoreError::ConflictingCommand)?
                    .unwrap_or_else(wes_language::calc::Package::standard),
            );
            if parsed.script.statements.len() == 1 && parsed.diagnostics.is_empty() {
                if let Some(definition) = super::repeat::Definition::capture(
                    replay.workspace(),
                    &command.nodes,
                    command
                        .environments
                        .clone()
                        .or_else(|| replay.workspace().default_environment_context()),
                ) {
                    repeat_definitions.insert(command.cell.clone(), definition);
                }
            }
        }
        let graph = replay.workspace().runtime().graph();
        for node in changed {
            if graph.node(&node).is_none() {
                continue;
            }
            let affected = graph
                .downstream(&node)
                .expect("checked node in exclusively owned graph");
            remaining_changes = remaining_changes
                .checked_sub(affected.len())
                .ok_or(RestoreError::Capacity("changed dependency traversal"))?;
            for node in affected {
                changed_at.insert(node, position);
            }
        }
        report.commands += 1;
        if !submissions.contains_key(command.cell.as_str()) {
            let input = SourceInput::new(command.cell.clone(), command.text.clone())
                .and_then(|i| i.with_source_name(command.source_name.clone()))
                .and_then(|i| i.with_source_start(command.source_start))
                .and_then(|i| i.with_document(command.document.clone()))
                .and_then(|input| match &command.revision_of {
                    Some(origin) => input.with_revision(origin.clone()),
                    None => Ok(input),
                })
                .map_err(|source| RestoreError::Source {
                    index: position,
                    source,
                })?;
            cells.restore(
                input,
                Ok(Arc::new(SubmissionResult {
                    receipts: vec![],
                    sandbox: None,
                    cell: command.cell.clone(),
                    nodes: command.nodes.clone(),
                    accepted: vec![],
                    removed: applied.removed,
                    unbound: applied.unbound,
                    diagnostics: SourceDiagnostics::default(),
                    recorded: true,
                    restored: true,
                    refreshed: vec![],
                    repeated_run: None,
                })),
            )?;
        }
    }
    // A changed payload must not receive an earlier run's value, even when that run's delayed
    // completion was journaled after the command. Require a new run start after the change.
    let mut starts = IndexMap::new();
    for (position, entry) in history.journal().iter().enumerate() {
        if let JournalEntry::Observed(record) = entry
            && record.state() == NodeState::Running
            && let Some(run) = record.run()
        {
            starts.entry((record.node(), run)).or_insert(position);
        }
    }
    // Validate once before indexing. Removed work is historical evidence only; stale
    // source epochs cannot authorize hydration after a definition change.
    let mut live_candidates = IndexSet::new();
    for (position, entry) in history.journal().iter().enumerate() {
        let JournalEntry::Snapshot(snapshot) = entry else {
            continue;
        };
        snapshot
            .validate()
            .map_err(|_| RestoreError::LiveSnapshot)?;
        let node = snapshot.observation.node();
        if replay.workspace().runtime().graph().node(node).is_none() {
            continue;
        }
        let started = starts
            .get(&(&snapshot.source, &snapshot.epoch))
            .ok_or(RestoreError::LiveSnapshot)?;
        if *started >= position
            || !created_at
                .get(node)
                .is_some_and(|created| position > *created)
        {
            return Err(RestoreError::LiveSnapshot);
        }
        if changed_at
            .get(node)
            .is_some_and(|changed| started <= changed)
        {
            continue;
        }
        if replay.workspace().runtime().ordered_root(node).as_ref() != Some(&snapshot.source) {
            return Err(RestoreError::LiveSnapshot);
        }
        live_candidates.insert(position);
    }
    let index = RestoreIndex::from_entries(history.journal().iter().enumerate().filter_map(
        |(position, entry)| {
            if duplicate_logs.contains(&position) {
                return None;
            }
            if matches!(entry, JournalEntry::Snapshot(_)) {
                return live_candidates.contains(&position).then_some(entry);
            }
            let (node, run) = match entry {
                JournalEntry::Observed(record) => (record.node(), record.run()),
                JournalEntry::Result(result) => (&result.node, Some(&result.run)),
                _ => return Some(entry),
            };
            let after_creation = created_at
                .get(node)
                .is_some_and(|created| position > *created);
            let after_change = changed_at.get(node).is_none_or(|changed| {
                position > *changed
                    && run
                        .and_then(|run| starts.get(&(node, run)))
                        .is_some_and(|started| started > changed)
            });
            (after_creation && after_change).then_some(entry)
        },
    ));
    report.unconfirmed_changes = changed_at
        .keys()
        .filter(|node| {
            replay.workspace().runtime().graph().node(node).is_some()
                && !index.observations.contains_key(*node)
        })
        .cloned()
        .collect();
    report.unconfirmed_external_changes = report
        .unconfirmed_changes
        .iter()
        .filter(|id| {
            replay
                .workspace()
                .runtime()
                .graph()
                .node(id)
                .is_some_and(|node| match node.payload() {
                    crate::tasks::BoundTask::Call(_) => true,
                    crate::tasks::BoundTask::Calculation(calc) => calc.compiled.effectful(),
                    _ => false,
                })
        })
        .cloned()
        .collect();
    for cell in submission_order.values() {
        let record = submissions[cell];
        if let Some(command) = commands.get(cell)
            && (command.text != record.text
                || command.document != record.document
                || command.nodes != record.nodes
                || command.revision_of != record.revision_of)
        {
            return Err(RestoreError::ConflictingCommand);
        }
        if record.nodes.iter().any(|n| !created_at.contains_key(n)) {
            return Err(RestoreError::ConflictingCommand);
        }
        cells.restore(
            record
                .input()
                .map_err(|_| RestoreError::ConflictingCommand)?,
            Ok(Arc::new(SubmissionResult {
                receipts: vec![],
                sandbox: None,
                cell: record.cell.clone(),
                nodes: record.nodes.clone(),
                accepted: vec![],
                removed: vec![],
                unbound: vec![],
                diagnostics: SourceDiagnostics::default(),
                recorded: commands.contains_key(cell),
                restored: true,
                refreshed: record.refreshed.clone(),
                repeated_run: record.run.clone(),
            })),
        )?;
    }
    for (cell, definition) in repeat_definitions {
        if cells.input(&cell).is_some() {
            cells.set_definition(&cell, definition);
        }
    }
    let mut log = SessionLog::new(&recording);
    for (position, entry) in history.journal().iter().enumerate() {
        check_cancelled(&cancellation)?;
        if duplicate_logs.contains(&position) {
            continue;
        }
        if let JournalEntry::Diagnosed(diagnostic) = entry
            && !diagnostic.cell().is_empty()
        {
            let input =
                SourceInput::new(diagnostic.cell().to_owned(), diagnostic.source().to_owned())
                    .map_err(|source| RestoreError::Source {
                        index: report.commands,
                        source,
                    })?;
            if !cells.contains(input.cell()) {
                cells.restore(
                    input,
                    Ok(Arc::new(SubmissionResult {
                        receipts: vec![],
                        sandbox: None,
                        cell: diagnostic.cell().to_owned(),
                        nodes: vec![],
                        accepted: vec![],
                        removed: vec![],
                        unbound: vec![],
                        diagnostics: SourceDiagnostics::default(),
                        recorded: false,
                        restored: true,
                        refreshed: vec![],
                        repeated_run: None,
                    })),
                )?;
            }
            cells.restore_diagnostic(diagnostic)?;
        }
        if matches!(
            entry,
            JournalEntry::Observed(_)
                | JournalEntry::Snapshot(_)
                | JournalEntry::Diagnosed(_)
                | JournalEntry::Noticed(_)
        ) {
            log.restore(entry.clone(), checkpoint.journal);
        }
    }
    // Declaration replay and diagnostic recovery are different passes, not reading order.
    // Without this join, old diagnostic-only cells appear after the newest commands on
    // every restart, hiding recent work below an apparently frozen end of scrollback.
    let mut first_evidence = std::collections::BTreeMap::new();
    for (position, entry) in history.journal().iter().enumerate() {
        let cell = match entry {
            JournalEntry::Command(c) => &c.cell,
            JournalEntry::Diagnosed(d) => d.cell(),
            _ => continue,
        };
        first_evidence.entry(cell.to_owned()).or_insert(position);
    }
    cells.restore_order(
        &first_evidence,
        &submissions
            .values()
            .map(|s| (s.cell.clone(), s.order))
            .collect(),
    );
    for entry in history.recovery() {
        let cell = match entry {
            RecoveryEntry::Accepted { cell } => cell,
            RecoveryEntry::Calling(call) => &call.cell,
            RecoveryEntry::Called { .. } => continue,
        };
        if !cell.is_empty() {
            // Validate the same client identity bound without inventing missing source text.
            SourceInput::new(cell.clone(), String::new()).map_err(|source| {
                RestoreError::Source {
                    index: report.commands,
                    source,
                }
            })?;
            report.unidentified_cells += usize::from(cells.restore_unknown(cell)?);
        }
    }
    let mut values = storage
        .clone()
        .map(|storage| SessionValues::new(storage, &recording));
    let nodes: Vec<_> = replay
        .workspace()
        .runtime()
        .graph()
        .nodes()
        .map(|node| node.id().clone())
        .collect();
    let mut remaining = 512 * 1024 * 1024;
    for node in nodes {
        check_cancelled(&cancellation)?;
        let mut value = None;
        if let Some(result) = index.retained.get(&node) {
            let loaded = match &storage {
                None => Err(RestoreProblemKind::NoValueStore),
                Some(storage) => match storage.worker.recover(result.handle.clone()).await {
                    Ok(Some(loaded)) => Ok(loaded),
                    Ok(None) => Err(RestoreProblemKind::MissingValue),
                    Err(_) => Err(RestoreProblemKind::UnreadableValue),
                },
            };
            check_cancelled(&cancellation)?;
            match loaded {
                Ok(loaded) => {
                    if let Some(charge) = value_charge(&loaded.loaded.value, remaining) {
                        remaining -= charge;
                        values
                            .as_mut()
                            .expect("recovered through configured storage")
                            .restore(
                                node.clone(),
                                RecoveredValue {
                                    run: result.run.clone(),
                                    handle: result.handle.clone(),
                                    bytes: loaded.bytes,
                                    retention: loaded.retention,
                                    journal_checkpoint: checkpoint.journal,
                                },
                            )?;
                        report.values += 1;
                        value = Some(loaded.loaded.value);
                    } else {
                        report.problems.push(RestoreProblem {
                            node: node.clone(),
                            handle: result.handle.clone(),
                            kind: RestoreProblemKind::ValueCapacity,
                        });
                    }
                }
                Err(kind) => report.problems.push(RestoreProblem {
                    node: node.clone(),
                    handle: result.handle.clone(),
                    kind,
                }),
            }
        }
        let run = index
            .observations
            .get(&node)
            .and_then(|observed| observed.run().cloned())
            .or_else(|| index.retained.get(&node).map(|result| result.run.clone()));
        let state = if changed_at.contains_key(&node) && !index.observations.contains_key(&node) {
            crate::runtime::RestoredState::StaleBecause(crate::runtime::StaleReason::RestoreChanged)
        } else {
            index.state(&node, value)
        };
        replay.hydrate(&node, state, run)?;
    }
    check_cancelled(&cancellation)?;
    report.interrupted = unresolved_calls(history.recovery())
        .into_iter()
        .cloned()
        .collect();
    let mut workspace = replay.finish();
    if let Some((position, record)) =
        history
            .journal()
            .iter()
            .enumerate()
            .rev()
            .find_map(|(p, e)| match e {
                JournalEntry::Views(r) => Some((p, r)),
                _ => None,
            })
    {
        workspace
            .restore_views(
                record,
                position,
                &history,
                storage.as_ref(),
                &definitions_changed_at,
            )
            .await
            .map_err(|e| RestoreError::Views(e.to_string()))?;
    }
    check_cancelled(&cancellation)?;
    Ok(RestoredSession {
        workspace,
        recording,
        cells,
        log,
        values,
        report,
    })
}
fn check_cancelled(cancellation: &CancellationToken) -> Result<(), RestoreError> {
    if cancellation.is_cancelled() {
        Err(RestoreError::Cancelled)
    } else {
        Ok(())
    }
}
