use super::model::{Scenario, ScenarioError, Step};
use serde::Serialize;
use std::collections::BTreeSet;
use tokio::time::{Instant, timeout_at};
use wes_core::{Data, ErrorValue, Value, environments::EnvironmentContext, flow::FlowPolicy};
use wes_engine::{
    driver::CancellationToken, graph::NodeState, session::SessionHandle, source::SourceInput,
};
use wes_language::Severity;

#[derive(Serialize)]
struct WireReport {
    schema: u8,
    name: String,
    run: String,
    status: &'static str,
    duration_ms: u64,
    steps: Vec<StepReport>,
    cleanup: Vec<StepReport>,
}
/// Result metadata inherits input policy. Only `export()` may disclose the report to stdout/files.
pub struct Report {
    wire: WireReport,
    policy: FlowPolicy,
}
impl Report {
    pub fn passed(&self) -> bool {
        self.wire.status == "passed"
    }
    pub fn export(&self) -> Option<serde_json::Value> {
        if self.policy.is_private() || self.policy.is_unknown() {
            None
        } else {
            Some(serde_json::to_value(&self.wire).expect("report contains serializable metadata"))
        }
    }
}
#[derive(Serialize)]
struct StepReport {
    name: String,
    status: &'static str,
    codes: Vec<String>,
    evidence: Vec<Evidence>,
    checks: Vec<CheckReport>,
    error: Option<ErrorEvidence>,
}
impl StepReport {
    fn skipped(step: &Step) -> Self {
        Self {
            name: step.name.clone(),
            status: "skipped",
            codes: vec![],
            evidence: vec![],
            error: None,
            checks: step
                .expect
                .iter()
                .map(|c| CheckReport {
                    name: c.name.clone(),
                    status: "skipped",
                    codes: vec![],
                })
                .collect(),
        }
    }
}
#[derive(Serialize)]
struct ErrorEvidence {
    code: String,
    issues: Vec<String>,
}
#[derive(Serialize)]
struct CheckReport {
    name: String,
    status: &'static str,
    codes: Vec<String>,
}
#[derive(Serialize)]
struct Evidence {
    node: String,
    run: Option<String>,
    state: String,
    environments: Vec<EnvironmentEvidence>,
}
#[derive(Serialize)]
struct EnvironmentEvidence {
    name: String,
    revision: String,
}
struct Executed {
    ok: bool,
    codes: Vec<String>,
    evidence: Vec<Evidence>,
    error: Option<ErrorValue>,
    value: Option<Value>,
    policy: FlowPolicy,
}
impl Executed {
    fn rejected(code: &str) -> Self {
        Self {
            ok: false,
            codes: vec![code.into()],
            evidence: vec![],
            error: None,
            value: None,
            policy: FlowPolicy::default(),
        }
    }
}
struct Runner {
    session: SessionHandle,
    context: Option<EnvironmentContext>,
    cancel: CancellationToken,
    deadline: Instant,
    ended: bool,
    policy: FlowPolicy,
    owned: BTreeSet<String>,
}

/// The caller owns a dedicated, idle session for the duration. Timeout/cancellation closes it;
/// the application owner must then join normal runtime shutdown. This never retries a submission.
pub async fn run(
    session: SessionHandle,
    scenario: &Scenario,
    context: Option<EnvironmentContext>,
    cancel: CancellationToken,
) -> Result<Report, ScenarioError> {
    let scenario = &scenario.document;
    let initial = session
        .snapshot()
        .await
        .map_err(|_| ScenarioError("session unavailable".into()))?;
    if !initial.execution.idle
        || !initial.execution.streaming.is_empty()
        || initial.admission_pending
    {
        return Err(ScenarioError(
            "scenario requires an idle, exclusively owned session".into(),
        ));
    }
    let started = Instant::now();
    let mut runner = Runner {
        session,
        context,
        cancel,
        deadline: started + std::time::Duration::from_secs(scenario.timeout_seconds),
        ended: false,
        policy: FlowPolicy::default(),
        owned: BTreeSet::new(),
    };
    let mut steps = Vec::new();
    let mut failed = false;
    for step in &scenario.steps {
        if failed || runner.ended {
            steps.push(StepReport::skipped(step));
            continue;
        }
        let report = runner.step(step).await;
        failed |= report.status != "passed";
        steps.push(report);
    }
    let mut cleanup = Vec::new();
    for step in &scenario.cleanup {
        let report = if runner.ended {
            StepReport::skipped(step)
        } else {
            runner.step(step).await
        };
        failed |= report.status != "passed";
        cleanup.push(report);
    }
    Ok(Report {
        policy: runner.policy,
        wire: WireReport {
            schema: 1,
            name: scenario.name.clone(),
            run: uuid::Uuid::new_v4().to_string(),
            status: if failed || runner.ended {
                "failed"
            } else {
                "passed"
            },
            duration_ms: started.elapsed().as_millis() as u64,
            steps,
            cleanup,
        },
    })
}
impl Runner {
    async fn step(&mut self, step: &Step) -> StepReport {
        let result = self.execute(&step.run).await;
        let mut ok = match &step.expect_error {
            None => result.ok,
            Some(expected) => {
                result.evidence.len() == 1
                    && result.evidence[0].state == "failed"
                    && result.error.as_ref().is_some_and(|error| {
                        error.code() == expected.code
                            && expected
                                .issue
                                .as_ref()
                                .is_none_or(|issue| error.issues().iter().any(|i| &i.code == issue))
                    })
                    && result.codes.len() == 1
            }
        };
        let mut codes = result.codes;
        if step.expect_error.is_some() && !ok && result.ok {
            codes.push("TEST_EXPECTED_ERROR".into());
        }
        let mut checks = Vec::new();
        let run_checks = ok;
        for check in &step.expect {
            if !run_checks || self.ended {
                checks.push(CheckReport {
                    name: check.name.clone(),
                    status: "skipped",
                    codes: vec![],
                });
                continue;
            }
            let checked = self.execute(&check.source()).await;
            let passed = checked.ok
                && checked
                    .value
                    .as_ref()
                    .is_some_and(|v| v.data() == &Data::Bool(true));
            let mut codes = checked.codes;
            if checked.ok && !passed {
                codes.push(
                    if checked
                        .value
                        .as_ref()
                        .is_some_and(|v| matches!(v.data(), Data::Bool(_)))
                    {
                        "TEST_FALSE"
                    } else {
                        "TEST_EXPECTED_BOOL"
                    }
                    .into(),
                );
            }
            checks.push(CheckReport {
                name: check.name.clone(),
                status: if passed { "passed" } else { "failed" },
                codes,
            });
            ok &= passed;
        }
        StepReport {
            name: step.name.clone(),
            status: if ok { "passed" } else { "failed" },
            codes,
            evidence: result.evidence,
            checks,
            error: result.error.as_ref().map(|e| ErrorEvidence {
                code: e.code().to_owned(),
                issues: e.issues().iter().map(|i| i.code.clone()).collect(),
            }),
        }
    }
    async fn execute(&mut self, source: &str) -> Executed {
        let session = self.session.clone();
        let context = self.context.clone();
        let owned = &self.owned;
        let work = async {
            let before = session.snapshot().await.map_err(|_| "TEST_SESSION")?;
            for reference in super::model::references(source) {
                let name = reference
                    .split("::")
                    .next()
                    .unwrap_or(&reference)
                    .split('.')
                    .next()
                    .unwrap_or(&reference);
                let node = before
                    .names
                    .get(name)
                    .map(|o| o.node.as_str())
                    .unwrap_or(name);
                if !owned.contains(node) {
                    return Ok(Executed::rejected("TEST_FOREIGN_INPUT"));
                }
            }
            let mut input = SourceInput::new(uuid::Uuid::new_v4().to_string(), source.to_owned())
                .map_err(|_| "TEST_SOURCE")?;
            if let Some(context) = context {
                input = input
                    .with_environments(context)
                    .map_err(|_| "TEST_CONTEXT")?;
            }
            let reply = session.submit(input).await.map_err(|_| "TEST_ADMISSION")?;
            session.wait_idle().await.map_err(|_| "TEST_EXECUTION")?;
            let snapshot = session.snapshot().await.map_err(|_| "TEST_SESSION")?;
            let mut result = Executed {
                ok: true,
                codes: vec![],
                evidence: vec![],
                error: None,
                value: None,
                policy: FlowPolicy::default(),
            };
            for d in &reply.diagnostics.diagnostics {
                if d.severity == Severity::Error {
                    result.ok = false;
                    result.codes.push(d.code.to_string());
                }
            }
            for id in &reply.nodes {
                let Some(node) = snapshot.execution.graph.node(id) else {
                    result.ok = false;
                    result.codes.push("TEST_MISSING_NODE".into());
                    continue;
                };
                let state = node.state();
                let environments: BTreeSet<_> = node
                    .payload()
                    .environments()
                    .map(|b| {
                        (
                            b.environment().name().to_owned(),
                            b.environment().revision().to_string(),
                        )
                    })
                    .collect();
                result.evidence.push(Evidence {
                    node: id.to_string(),
                    run: snapshot.execution.runs.get(id).map(ToString::to_string),
                    state: format!("{state:?}").to_lowercase(),
                    environments: environments
                        .into_iter()
                        .map(|(name, revision)| EnvironmentEvidence { name, revision })
                        .collect(),
                });
                if let Some(typing) = snapshot.execution.actual_typings.get(id) {
                    result.policy = result.policy.join(typing.provenance.policy());
                }
                if let Some(value) = snapshot.execution.values.get(id) {
                    result.policy = result.policy.join(value.provenance().policy());
                    result.value = Some(value.clone());
                }
                if let Some(error) = snapshot.execution.errors.get(id) {
                    result.policy = result.policy.join(error.policy());
                    result.codes.push(error.code().to_owned());
                    result.error = Some(error.clone());
                    result.ok = false;
                }
                if state != NodeState::Ready {
                    result.ok = false;
                }
                if !matches!(state, NodeState::Ready | NodeState::Failed) {
                    result.codes.push("TEST_INCOMPLETE".into());
                    // Missing/cancelled input must not silently erase its unavailable provenance.
                    result.policy = result.policy.unknown();
                }
            }
            if !snapshot.execution.streaming.is_empty() {
                return Err("TEST_STREAM");
            }
            Ok::<_, &'static str>(result)
        };
        let result = tokio::select! {
            biased;
            _ = self.cancel.cancelled() => Err("TEST_CANCELLED"),
            result = timeout_at(self.deadline, work) => result.unwrap_or(Err("TEST_TIMEOUT")),
        };
        match result {
            Ok(result) => {
                self.owned
                    .extend(result.evidence.iter().map(|e| e.node.clone()));
                self.policy = self.policy.join(&result.policy);
                result
            }
            Err(code) => {
                self.ended = true;
                // Admission may still be in flight. Closing the owned session cancels it too;
                // normal application shutdown joins all physical readers/workers afterward.
                let _ = self.session.shutdown().await;
                self.policy = self.policy.clone().unknown();
                Executed::rejected(code)
            }
        }
    }
}
