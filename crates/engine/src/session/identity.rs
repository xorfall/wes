//! Bounded in-memory deduplication. Never evict an identity and thereby authorize a hidden retry.
use super::{SessionError, SourceInput, SubmissionReply, SubmissionResult};
use indexmap::{IndexMap, IndexSet};
use tokio::sync::oneshot;
fn max_cells() -> usize {
    wes_budgets::get("history.identities") as usize
}
fn max_bytes() -> usize {
    wes_budgets::get("history.identity.bytes") as usize
}
fn max_waiters() -> usize {
    wes_budgets::get("history.identity.waiters") as usize
}
struct Cell {
    input: SourceInput,
    extra: usize,
    reply: Option<SubmissionReply>,
    waiting: Vec<oneshot::Sender<SubmissionReply>>,
    definition: Option<super::repeat::Definition>,
}
#[derive(Default)]
pub(super) struct Cells {
    cells: IndexMap<String, Cell>,
    bytes: usize,
    unknown: IndexSet<String>,
}
impl Cells {
    pub fn retire(&mut self, ids: &std::collections::BTreeSet<String>) {
        for id in ids {
            if let Some(cell) = self.cells.shift_remove(id) {
                self.bytes -=
                    cell.input.text().len() * 6 + cell.extra + cell.input.context_charge();
                self.unknown.insert(id.clone());
            }
        }
    }
    pub fn restore_order(
        &mut self,
        first_evidence: &std::collections::BTreeMap<String, usize>,
        submissions: &std::collections::BTreeMap<String, u64>,
    ) {
        self.cells.sort_by(|left, _, right, _| {
            let key = |id: &String| match submissions.get(id) {
                Some(order) => (1u8, *order),
                None => (
                    0,
                    first_evidence.get(id).copied().unwrap_or(usize::MAX) as u64,
                ),
            };
            key(left).cmp(&key(right))
        });
    }
    pub fn order(&self, id: &str) -> u64 {
        self.cells.get_index_of(id).expect("admitted identity") as u64
    }
    pub fn observed(&self) -> Vec<super::ObservedCell> {
        self.cells
            .values()
            .map(|cell| super::ObservedCell {
                input: cell.input.clone(),
                reply: cell.reply.clone(),
            })
            .collect()
    }
    pub fn restore_unknown(&mut self, id: &str) -> Result<bool, SessionError> {
        if self.cells.contains_key(id) || self.unknown.contains(id) {
            return Ok(false);
        }
        let bytes = id.len() * 6 + 1024;
        if self.cells.len() + self.unknown.len() >= max_cells()
            || bytes > max_bytes().saturating_sub(self.bytes)
        {
            return Err(SessionError::Capacity);
        }
        self.bytes += bytes;
        self.unknown.insert(id.to_owned());
        Ok(true)
    }
    /// Startup-only atomic insertion. Never replace an existing identity or evict one for capacity.
    pub fn restore(
        &mut self,
        input: SourceInput,
        reply: SubmissionReply,
    ) -> Result<(), SessionError> {
        if let Some(cell) = self.cells.get(input.cell()) {
            return if cell.input.same_request(&input) {
                Ok(())
            } else {
                Err(SessionError::Conflict)
            };
        }
        let extra = reply.as_ref().map_or(0, |reply| charge(reply));
        let bytes =
            input.text().len() * 6 + input.cell().len() * 6 + 1024 + extra + input.context_charge();
        if self.unknown.contains(input.cell()) {
            return Err(SessionError::HistoricalReplyUnavailable);
        }
        if self.cells.len() + self.unknown.len() >= max_cells()
            || bytes > max_bytes().saturating_sub(self.bytes)
        {
            return Err(SessionError::Capacity);
        }
        self.bytes += bytes;
        self.cells.insert(
            input.cell().to_owned(),
            Cell {
                input,
                extra,
                reply: Some(reply),
                waiting: vec![],
                definition: None,
            },
        );
        Ok(())
    }
    pub fn contains(&self, id: &str) -> bool {
        self.cells.contains_key(id) || self.unknown.contains(id)
    }
    /// Attach historical diagnostic evidence without inventing a failed execution on reopen.
    pub fn restore_diagnostic(
        &mut self,
        record: &crate::history::DiagnosticRecord,
    ) -> Result<(), SessionError> {
        let cell = self
            .cells
            .get(record.cell())
            .ok_or(SessionError::Conflict)?;
        if cell.input.text() != record.source() {
            return Err(SessionError::Conflict);
        }
        let Some(Ok(reply)) = &cell.reply else {
            return Err(SessionError::Conflict);
        };
        let mut reply = (**reply).clone();
        reply
            .diagnostics
            .diagnostics
            .push(record.diagnostic().clone());
        reply
            .diagnostics
            .check_limit()
            .map_err(|_| SessionError::Capacity)?;
        if !self.reserve(record.cell(), charge(&reply)) {
            return Err(SessionError::Capacity);
        }
        self.cells.get_mut(record.cell()).unwrap().reply = Some(Ok(std::sync::Arc::new(reply)));
        Ok(())
    }
    pub fn input(&self, id: &str) -> Option<&SourceInput> {
        self.cells.get(id).map(|cell| &cell.input)
    }
    pub fn nodes(&self, id: &str) -> Result<Vec<crate::graph::NodeId>, SessionError> {
        self.cells
            .get(id)
            .and_then(|cell| cell.reply.as_ref())
            .and_then(|reply| reply.as_ref().ok())
            .map(|reply| reply.nodes.clone())
            .ok_or(SessionError::UnknownNode)
    }
    pub fn begin(&mut self, input: SourceInput, reply: oneshot::Sender<SubmissionReply>) -> bool {
        if self.unknown.contains(input.cell()) {
            let _ = reply.send(Err(SessionError::HistoricalReplyUnavailable));
            return false;
        }
        if let Some(cell) = self.cells.get_mut(input.cell()) {
            cell.waiting.retain(|reply| !reply.is_closed());
            if !cell.input.same_request(&input) {
                let _ = reply.send(Err(SessionError::Conflict));
            } else if let Some(result) = &cell.reply {
                let _ = reply.send(result.clone());
            } else if cell.waiting.len() >= max_waiters() {
                let _ = reply.send(Err(SessionError::Capacity));
            } else {
                cell.waiting.push(reply);
            }
            return false;
        }
        let bytes = input.text().len() * 6 + input.cell().len() * 6 + 1024 + input.context_charge();
        if self.cells.len() + self.unknown.len() >= max_cells()
            || bytes > max_bytes().saturating_sub(self.bytes)
        {
            let _ = reply.send(Err(SessionError::Capacity));
            return false;
        }
        self.bytes += bytes;
        self.cells.insert(
            input.cell().to_owned(),
            Cell {
                input,
                extra: 0,
                reply: None,
                waiting: vec![reply],
                definition: None,
            },
        );
        true
    }
    pub fn reserve(&mut self, id: &str, extra: usize) -> bool {
        let cell = self
            .cells
            .get_mut(id)
            .expect("reserve follows identity admission");
        let bytes = self.bytes - cell.extra;
        if extra > max_bytes().saturating_sub(bytes) {
            return false;
        }
        self.bytes = bytes + extra;
        cell.extra = extra;
        true
    }
    pub fn has_revision(&self, id: &str) -> bool {
        self.cells
            .values()
            .any(|c| c.input.revision_of() == Some(id) && c.definition.is_some())
    }
    pub fn definition(&self, id: &str) -> Option<&super::repeat::Definition> {
        self.cells.get(id)?.definition.as_ref()
    }
    pub fn set_definition(&mut self, id: &str, definition: super::repeat::Definition) {
        self.cells.get_mut(id).expect("accepted cell").definition = Some(definition);
    }
    pub fn finish(&mut self, id: &str, reply: SubmissionReply) {
        let cell = self
            .cells
            .get_mut(id)
            .expect("completion follows identity admission");
        if reply.is_err() {
            self.bytes -= cell.extra;
            cell.extra = 0;
        } else if let Ok(result) = &reply {
            debug_assert!(charge(result) <= cell.extra);
        }
        for waiting in cell.waiting.drain(..) {
            let _ = waiting.send(reply.clone());
        }
        cell.reply = Some(reply);
    }
    pub fn close(&mut self) {
        for cell in self.cells.values_mut() {
            for reply in cell.waiting.drain(..) {
                let _ = reply.send(Err(SessionError::Stopped));
            }
        }
    }
}
/// Conservative text and collection charge, not allocator/RSS accounting or external Arc ownership.
pub(super) fn charge(result: &SubmissionResult) -> usize {
    let mut bytes = 1024 + result.cell.len() * 6 + result.accepted.len() * 32;
    for node in result
        .nodes
        .iter()
        .chain(&result.refreshed)
        .chain(&result.removed)
    {
        bytes += 128 + node.as_str().len() * 6;
    }
    for receipt in &result.receipts {
        bytes += receipt.charge();
    }
    for name in &result.unbound {
        bytes += 32 + name.len() * 6;
    }
    for diagnostic in &result.diagnostics.diagnostics {
        bytes += 256
            + 6 * (diagnostic.code.len()
                + diagnostic.message.len()
                + diagnostic
                    .public_message
                    .as_ref()
                    .map_or(0, |message| message.len()));
        for hint in &diagnostic.hints {
            bytes += 32 + hint.len() * 6;
        }
    }
    for statement in &result.diagnostics.issues {
        bytes += 64;
        for issue in &statement.issues {
            bytes += 128 + 6 * (issue.code.len() + issue.message.len() + issue.path.len());
        }
    }
    bytes
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn prepared_success_reply_fits_exact_credit_and_shortfall_refuses_before_installation() {
        use super::super::{
            RecordingMode, SessionLog, SessionSeed, reply_reservation, spawn_owned,
        };
        use crate::{
            driver::CancellationToken,
            source::{SourcePreparation, prepare_declarations},
            type_sources::{TypeSourceCapture, TypeSourceError, TypeSourceReader},
            workspace::Workspace,
        };
        use std::{num::NonZeroUsize, sync::Arc};
        struct NoFiles;
        impl TypeSourceReader for NoFiles {
            fn read(&self, _: &str, _: usize) -> Result<String, TypeSourceError> {
                panic!("unexpected file read")
            }
        }
        for shortfall in [0, 1] {
            let workspace = Workspace::local(crate::providers::LocalScope::new("fixture").unwrap());
            let request = input(
                "install",
                ":def identity() -> Int as :calc pure { return 1; }",
            );
            let SourcePreparation::Declarations(prepared) = prepare_declarations(
                request.clone(),
                workspace.draft().unwrap(),
                TypeSourceCapture::live(Arc::new(NoFiles)),
                CancellationToken::new(),
            )
            .await
            .unwrap() else {
                panic!("expected definition");
            };
            let required = reply_reservation(&prepared);
            let base = request.text().len() * 6
                + request.cell().len() * 6
                + 1024
                + request.context_charge();
            let mut cells = Cells::default();
            let (reply, _held) = oneshot::channel();
            assert!(cells.begin(input("held", ""), reply));
            assert!(cells.reserve(
                "held",
                max_bytes() - cells.bytes - base - required + shortfall
            ));
            let recording = RecordingMode::Ephemeral;
            let (handle, task) = spawn_owned(
                workspace,
                recording.clone(),
                Arc::new(NoFiles),
                NonZeroUsize::new(1).unwrap(),
                SessionSeed {
                    capacity: None,
                    cells,
                    log: SessionLog::new(&recording),
                    values: None,
                    restoration: None,
                    actions: None,
                },
            )
            .unwrap();
            let reply = handle.submit(request).await;
            if shortfall == 0 {
                let reply = reply.unwrap();
                assert_eq!(reply.accepted.len(), 1);
                assert!(
                    reply
                        .diagnostics
                        .diagnostics
                        .iter()
                        .any(|d| d.code == "TMP000")
                );
                assert_eq!(charge(&reply), required);
            } else {
                assert!(matches!(reply, Err(SessionError::Capacity)));
            }
            let observation = handle.observe().await.unwrap();
            assert_eq!(observation.templates.contains("identity"), shortfall == 0);
            handle.shutdown().await.unwrap();
            task.join().await.unwrap();
        }
    }
    fn input(cell: &str, text: &str) -> SourceInput {
        SourceInput::new(cell.into(), text.into()).unwrap()
    }
    #[tokio::test]
    async fn recovered_and_unknown_identities_share_capacity_and_never_authorize_an_overwrite() {
        let mut cells = Cells::default();
        cells
            .restore(
                input("known", "old"),
                Err(SessionError::HistoricalReplyUnavailable),
            )
            .unwrap();
        assert!(cells.restore_unknown("unknown").unwrap());
        assert!(!cells.restore_unknown("unknown").unwrap());
        let before = cells.bytes;
        assert!(matches!(
            cells.restore(input("known", "different"), Err(SessionError::Preparation)),
            Err(SessionError::Conflict)
        ));
        assert!(matches!(
            cells.restore(input("unknown", "new"), Err(SessionError::Preparation)),
            Err(SessionError::HistoricalReplyUnavailable)
        ));
        assert_eq!(cells.bytes, before);
        for number in 2..max_cells() {
            cells.restore_unknown(&format!("old-{number}")).unwrap();
        }
        assert!(matches!(
            cells.restore_unknown("overflow"),
            Err(SessionError::Capacity)
        ));
        let (reply, result) = oneshot::channel();
        assert!(!cells.begin(input("overflow", "new"), reply));
        assert!(matches!(result.await.unwrap(), Err(SessionError::Capacity)));
        let (reply, result) = oneshot::channel();
        assert!(!cells.begin(input("unknown", "arbitrary source"), reply));
        assert!(matches!(
            result.await.unwrap(),
            Err(SessionError::HistoricalReplyUnavailable)
        ));
    }
    #[tokio::test]
    async fn identity_never_eviction_retries_and_conflicting_text_never_replaces_a_cell() {
        let mut cells = Cells::default();
        let (reply, first) = oneshot::channel();
        assert!(cells.begin(input("cell", "first"), reply));
        let (reply, conflict) = oneshot::channel();
        assert!(!cells.begin(input("cell", "different"), reply));
        assert!(matches!(
            conflict.await.unwrap(),
            Err(SessionError::Conflict)
        ));
        let (reply, duplicate) = oneshot::channel();
        assert!(!cells.begin(input("cell", "first"), reply));
        cells.finish("cell", Err(SessionError::Recording));
        assert!(matches!(first.await.unwrap(), Err(SessionError::Recording)));
        assert!(matches!(
            duplicate.await.unwrap(),
            Err(SessionError::Recording)
        ));
        let (reply, retry) = oneshot::channel();
        assert!(!cells.begin(input("cell", "first"), reply));
        assert!(matches!(retry.await.unwrap(), Err(SessionError::Recording)));
    }
    #[tokio::test]
    async fn failed_reservation_is_atomic_and_failure_releases_unused_reply_credit() {
        let mut cells = Cells::default();
        let (reply, _first) = oneshot::channel();
        assert!(cells.begin(input("first", ""), reply));
        let base = cells.bytes;
        assert!(cells.reserve("first", max_bytes() - base));
        assert_eq!(cells.bytes, max_bytes());
        assert!(!cells.reserve("first", max_bytes()));
        assert_eq!(cells.bytes, max_bytes());
        let (reply, rejected) = oneshot::channel();
        assert!(!cells.begin(input("next", ""), reply));
        assert!(matches!(
            rejected.await.unwrap(),
            Err(SessionError::Capacity)
        ));
        assert!(!cells.cells.contains_key("next"));
        cells.finish("first", Err(SessionError::Preparation));
        assert_eq!(cells.bytes, base);
        let (reply, _next) = oneshot::channel();
        assert!(cells.begin(input("next", ""), reply));
    }
    #[tokio::test]
    async fn cell_and_duplicate_waiter_counts_are_bounded_and_abandonment_releases_waiter_slots() {
        let mut cells = Cells::default();
        let mut receivers = vec![];
        for _ in 0..max_waiters() {
            let (reply, receive) = oneshot::channel();
            cells.begin(input("pending", "same"), reply);
            receivers.push(receive);
        }
        let (reply, rejected) = oneshot::channel();
        assert!(!cells.begin(input("pending", "same"), reply));
        assert!(matches!(
            rejected.await.unwrap(),
            Err(SessionError::Capacity)
        ));
        drop(receivers.pop());
        let (reply, mut accepted) = oneshot::channel();
        assert!(!cells.begin(input("pending", "same"), reply));
        assert!(matches!(
            accepted.try_recv(),
            Err(oneshot::error::TryRecvError::Empty)
        ));
        for index in 1..max_cells() {
            let (reply, _receive) = oneshot::channel();
            assert!(cells.begin(input(&index.to_string(), ""), reply));
        }
        let (reply, rejected) = oneshot::channel();
        assert!(!cells.begin(input("overflow", ""), reply));
        assert!(matches!(
            rejected.await.unwrap(),
            Err(SessionError::Capacity)
        ));
        cells.close();
        assert!(matches!(
            accepted.await.unwrap(),
            Err(SessionError::Stopped)
        ));
    }
}
