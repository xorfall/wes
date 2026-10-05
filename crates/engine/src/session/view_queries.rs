//! At most one physical runtime per target; replacement waits for its predecessor to join.
use super::*;
use crate::views::{Handle, QueryBinding};
use std::collections::BTreeMap;
use tokio::{
    sync::watch,
    time::{Duration, Instant},
};
const PERIOD: Duration = Duration::from_millis(250);
struct Running {
    target: Handle,
    generation: u64,
    binding: QueryBinding,
    origin: crate::runtime::Run,
    cancellation: CancellationToken,
    latest: watch::Receiver<Option<wes_core::Value>>,
    task: tokio::task::Id,
}
pub(super) struct Completed {
    target: Handle,
    generation: u64,
    binding: QueryBinding,
    origin: crate::runtime::Run,
    value: Option<wes_core::Value>,
    problem: Option<String>,
    uncertain: bool,
}
pub(super) struct Queries {
    running: BTreeMap<NodeId, Running>,
    pub tasks: JoinSet<Completed>,
    next: Instant,
    pub notices: Vec<(crate::runtime::Run, String)>,
    pub uncertain: bool,
}
impl Default for Queries {
    fn default() -> Self {
        Self {
            running: BTreeMap::new(),
            tasks: JoinSet::new(),
            next: Instant::now(),
            notices: vec![],
            uncertain: false,
        }
    }
}
impl Queries {
    pub fn deadline(&self, workspace: &Workspace) -> Option<Instant> {
        (!self.running.is_empty() || workspace.views.has_queries()).then_some(self.next)
    }
    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty()
    }
    pub fn stop(&self) {
        for run in self.running.values() {
            run.cancellation.cancel();
        }
    }
    pub fn tick(
        &mut self,
        workspace: &mut Workspace,
        reader: Arc<dyn TypeSourceReader>,
        capacity: crate::driver::ExecutionCapacity,
    ) {
        self.next = Instant::now() + PERIOD;
        workspace.views.update_query_intents();
        for run in self.running.values_mut() {
            let current = workspace.views.current_query(&run.target, run.generation)
                && workspace.query_access(&run.binding).is_ok();
            if !current {
                run.cancellation.cancel();
                continue;
            }
            if run.latest.has_changed().unwrap_or(false) {
                if let Some(value) = run.latest.borrow_and_update().clone() {
                    if workspace
                        .views
                        .sample(&run.target, Some(value), None)
                        .is_err()
                    {
                        self.notices.push((
                            run.origin.clone(),
                            "Query result exceeded its view contract or display budget".into(),
                        ));
                        workspace.views.query_finished(
                            &run.target,
                            run.generation,
                            Some(
                                "Query result exceeded its view contract or display budget".into(),
                            ),
                        );
                        run.cancellation.cancel();
                    }
                }
            }
        }
        if workspace.runtime().is_closed() {
            self.stop();
            return;
        }
        for request in workspace.views.query_requests() {
            if self.running.contains_key(request.target.id()) {
                continue;
            }
            if self.running.len() >= 8 {
                break;
            }
            let prepared = workspace.query_access(&request.binding).and_then(|()| {
                match &request.binding.adapter {
                    Some(adapter) => PreparedObservationQuery::capture_live(
                        workspace,
                        &request.binding.template,
                        &adapter.template,
                        request.input.clone(),
                        &request.caller,
                    ),
                    None => PreparedObservationQuery::capture(
                        workspace,
                        &request.binding.template,
                        request.input.clone(),
                        &request.caller,
                    ),
                }
                .map_err(|e| e.to_string())
            });
            let prepared = match prepared {
                Ok(prepared) => prepared,
                Err(error) => {
                    self.notices.push((request.origin.clone(), error.clone()));
                    workspace.views.query_finished(
                        &request.target,
                        request.generation,
                        Some(error),
                    );
                    continue;
                }
            };
            let (sender, latest) = watch::channel(None);
            let cancellation = CancellationToken::new();
            let target = request.target.clone();
            let generation = request.generation;
            let binding = request.binding.clone();
            let origin = request.origin.clone();
            let live = binding.adapter.is_some();
            let token = cancellation.clone();
            let reader = reader.clone();
            let capacity = capacity.clone();
            let evidence = wes_core::Provenance::default()
                .with_fact("view.query.origin", origin.id().to_string())
                .with_fact("view.query.generation", generation.to_string())
                .with_fact("view.query.template", binding.template.clone())
                .with_fact(
                    "view.query.templateRevision",
                    binding.template_revision.clone(),
                )
                .with_fact(
                    "view.query.inputDigest",
                    crate::views::query_input_digest(&request.input),
                );
            let task = self
                .tasks
                .spawn(async move {
                    let (value, problem, uncertain) =
                        run(prepared, reader, capacity, token, sender, live, evidence).await;
                    Completed {
                        target,
                        generation,
                        binding,
                        origin,
                        value,
                        problem,
                        uncertain,
                    }
                })
                .id();
            workspace.views.query_started(&request.target, generation);
            self.running.insert(
                request.target.id().clone(),
                Running {
                    target: request.target,
                    generation,
                    binding: request.binding,
                    origin: request.origin,
                    cancellation,
                    latest,
                    task,
                },
            );
        }
    }
    pub fn completed(
        &mut self,
        result: Result<Completed, tokio::task::JoinError>,
        workspace: &mut Workspace,
    ) {
        let completed = match result {
            Ok(value) => value,
            Err(error) => {
                let id = self
                    .running
                    .iter()
                    .find(|(_, run)| run.task == error.id())
                    .map(|(id, _)| id.clone());
                if let Some(id) = id {
                    let run = self.running.remove(&id).expect("captured worker");
                    self.uncertain = true;
                    self.notices.push((
                        run.origin.clone(),
                        "Query worker ended unexpectedly; automatic execution stopped".into(),
                    ));
                    workspace.views.stop_query(&run.target);
                    workspace.views.query_finished(
                        &run.target,
                        run.generation,
                        Some("Query worker ended unexpectedly".into()),
                    );
                }
                return;
            }
        };
        self.running.remove(completed.target.id());
        let current = workspace
            .views
            .current_query(&completed.target, completed.generation);
        let mut problem = completed.problem;
        if completed.uncertain {
            self.uncertain = true;
            workspace.views.stop_query(&completed.target);
            problem =
                Some("Query shutdown could not be confirmed; automatic queries stopped".into());
        }
        if current && problem.is_none() {
            if let Err(error) = workspace.query_access(&completed.binding) {
                problem = Some(error);
            } else if let Some(value) = completed.value {
                if workspace
                    .views
                    .sample(&completed.target, Some(value), None)
                    .is_err()
                {
                    problem = Some(
                        "Query result does not satisfy its view contract or display budget".into(),
                    );
                }
            }
        }
        if let Some(message) = &problem {
            self.notices
                .push((completed.origin.clone(), message.clone()));
        }
        workspace
            .views
            .query_finished(&completed.target, completed.generation, problem);
        self.next = Instant::now();
    }
}
async fn run(
    prepared: PreparedObservationQuery,
    reader: Arc<dyn TypeSourceReader>,
    capacity: crate::driver::ExecutionCapacity,
    cancellation: CancellationToken,
    latest: watch::Sender<Option<wes_core::Value>>,
    live: bool,
    evidence: wes_core::Provenance,
) -> (Option<wes_core::Value>, Option<String>, bool) {
    // Startup owns its cleanup guard. Even when cancellation arrives during preparation, join the
    // admitted scope before allowing the replacement; no detached start/retry race.
    let query = match prepared.start(reader, capacity).await {
        Ok(query) => query,
        Err(error) => return (None, Some(error.to_string()), false),
    };
    let stamp = |sample: DisplaySample| {
        let epochs = sample
            .epochs
            .iter()
            .map(|(node, run)| format!("{}:{}", node.as_str(), run))
            .collect::<Vec<_>>()
            .join(",");
        let producer = sample
            .producer_run
            .as_ref()
            .map(ToString::to_string)
            .unwrap_or_default();
        sample.value.map(|value| {
            value.with_provenance(
                evidence
                    .clone()
                    .with_fact("view.query.runs", epochs)
                    .with_fact("view.query.resultRun", producer)
                    .inheriting(value.provenance()),
            )
        })
    };
    let mut value = None;
    let mut problem = None;
    {
        let idle = query.wait_idle();
        tokio::pin!(idle);
        let mut tick = tokio::time::interval(PERIOD);
        loop {
            tokio::select! {
                biased;
                () = cancellation.cancelled() => break,
                idle = &mut idle, if !live => {
                    if let Err(error) = idle { problem = Some(error.to_string()); }
                    else { match query.sample().await { Ok(sample) => value = stamp(sample), Err(error) => problem = Some(error.to_string()) } }
                    break;
                },
                _ = tick.tick() => {
                    match query.sample().await {
                        Ok(sample) => { if sample.value.is_some() { value = stamp(sample); latest.send_replace(value.clone()); } },
                        Err(error) => { problem = Some(error.to_string()); break; },
                    }
                    if live { match query.live_active().await { Ok(true) => {}, Ok(false) => break, Err(error) => { problem = Some(error.to_string()); break; } } }
                },
            }
        }
    }
    match query.close().await {
        Ok(uncertain) => (value, problem, uncertain),
        Err(error) => (None, Some(error.to_string()), true),
    }
}
