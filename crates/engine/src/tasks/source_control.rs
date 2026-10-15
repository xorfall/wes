//! Native control bindings are captured at serialized entry. Stored projections
//! are inert; one explicit Send never refreshes or acquires its external source.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    graph::{OutputPort, OutputRef},
    plan::{Input, MetaTask},
    runtime::{Outcome, RuntimeCode},
    streams::{ControlError, LiveControls},
};
use wes_core::{Value, capability::Typing, flow::FlowPolicy};
use wes_language::{Diagnostic, Span, vocabulary::MetaCommand};

#[derive(Clone)]
pub struct BoundSourceControl {
    pub(crate) task: MetaTask,
    pub(crate) subject: OutputRef,
    pub(crate) captured: Option<Result<Captured, wes_core::ErrorValue>>,
    pub(crate) policy: FlowPolicy,
    pub(crate) admission: Option<crate::calls::AdmittedCommand>,
}
#[derive(Clone)]
pub(crate) enum Captured {
    Binding {
        value: Value,
        port: LiveControls,
    },
    Action {
        port: LiveControls,
        action: Value,
        budget: std::time::Duration,
    },
}
impl std::fmt::Debug for BoundSourceControl {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundSourceControl")
            .field("command", &self.task.spec.command)
            .field("subject", &self.subject)
            .finish_non_exhaustive()
    }
}
impl BoundSourceControl {
    pub(crate) fn bind(task: MetaTask, span: Span) -> Result<Self, Diagnostic> {
        let invalid = |message| Diagnostic::error("SRC001", span, message);
        let [Input::FromNode(subject)] = task.subjects.as_slice() else {
            return Err(invalid(
                "Source controls require one whole owned source or SourceControls reference",
            ));
        };
        if subject.port != OutputPort::Data {
            return Err(invalid(
                "Source controls require the original data selection",
            ));
        }
        if task.spec.command == MetaCommand::SourceSend && !task.inputs.contains_key("action") {
            return Err(invalid("Source Send requires a typed action"));
        }
        Ok(Self {
            subject: subject.clone(),
            task,
            captured: None,
            policy: Default::default(),
            admission: None,
        })
    }
    pub(crate) fn predicted_typing(&self) -> Typing {
        Typing::new(if self.task.spec.command == MetaCommand::SourceBind {
            wes_core::Shape::Meta(wes_core::MetaType::SourceControls)
        } else {
            wes_core::Shape::Unknown
        })
    }
    pub(crate) fn read_sources(&self) -> Vec<OutputRef> {
        self.task
            .inputs
            .values()
            .flat_map(Input::dependencies)
            .cloned()
            .chain(
                (self.task.spec.command == MetaCommand::SourceSend).then(|| self.subject.clone()),
            )
            .collect()
    }
    pub(crate) fn execute(
        self,
        run: crate::runtime::Run,
        journal: Option<crate::calls::CallJournal>,
        cancellation: CancellationToken,
    ) -> ExecutionFuture {
        Box::pin(async move {
            let failure = |message| {
                RuntimeCode::ExecutionFailed
                    .error(message, None)
                    .with_policy(&self.policy)
            };
            // Join any accepted admission repair before reconsidering cancellation.
            // The weak source port rechecks Run/epoch and current permission after
            // this await, at its queue and actual physical dispatch boundaries.
            if let Some(journal) = journal {
                let Some(admission) = &self.admission else {
                    return Outcome::Failed(failure("Source controls require acknowledged native command admission; nothing was dispatched")).into();
                };
                if journal.authorize(admission, &run).await.is_err() {
                    return Outcome::Failed(failure(
                        "Source control recording authority is unavailable; nothing was dispatched",
                    ))
                    .into();
                }
            } else if self.admission.is_some() {
                return Outcome::Failed(failure(
                    "Source control recording authority is unavailable; nothing was dispatched",
                ))
                .into();
            }
            let outcome = match self.captured {
                Some(Ok(Captured::Binding { value, port })) => {
                    if cancellation.is_cancelled() || port.description().is_err() {
                        return Outcome::Failed(failure("Source control authority changed before binding publication; no source was acquired")).into();
                    }
                    let policy = match port.provenance() {
                        Ok(provenance) => provenance,
                        Err(_) => return Outcome::Failed(failure("Source control authority changed before binding publication; no source was acquired")).into(),
                    };
                    return Outcome::Produced(value.with_provenance(value.provenance().clone().inheriting(&policy))).into();
                }
                Some(Ok(Captured::Action { port, action, budget })) => port.invoke(action,budget,cancellation).await,
                Some(Err(error)) => return Outcome::Failed(error).into(),
                None => return Outcome::Failed(failure("Source control requires a live authorized owner; reconstruction grants no authority")).into(),
            };
            match outcome {
                Ok(value) => Outcome::Produced(value).into(),
                Err(reason) => {
                    let message = match reason {
                        ControlError::Unknown => {
                            "External control outcome is unknown; the source retains physical ownership. Do not replay automatically."
                        }
                        ControlError::Invalid => {
                            "Source control action does not satisfy its captured contract or bounded deadline; nothing was dispatched."
                        }
                        ControlError::Capacity => {
                            "Source control capacity is exhausted; nothing was dispatched."
                        }
                        ControlError::Refused => {
                            "Source control authority refused or provider rejected the action; no external effect was confirmed."
                        }
                        ControlError::Changed => {
                            "Source control Run or connection changed; nothing was dispatched. Bind the current source explicitly."
                        }
                        ControlError::Expired => {
                            "Source control expired before dispatch; nothing was dispatched."
                        }
                        ControlError::Cancelled => {
                            "Source control cancelled before dispatch; nothing was dispatched."
                        }
                    };
                    let error = if reason == ControlError::Unknown {
                        RuntimeCode::ExternalOutcomeUnknown
                            .error(message, None)
                            .with_policy(&self.policy)
                    } else {
                        failure(message)
                    };
                    let mut report: crate::driver::ExecutionReport =
                        Outcome::Failed(error.clone()).into();
                    if reason == ControlError::Unknown {
                        report.notices.push(error);
                    }
                    report
                }
            }
        })
    }
}
