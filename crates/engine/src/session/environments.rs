//! Typed, bounded definition admission under the session owner. No invoker/credential construction.
use super::*;
use crate::{
    environments::{Change, EnvironmentRecord, Plan},
    history::{JournalEntry, Record},
};
use std::collections::BTreeMap;
use std::time::Duration;
use wes_core::environments::{CapturedSources, Revision};

/// Dedicated non-source channel. Deliberately no Debug/Serialize: Supply owns secret material.
pub enum EnvironmentAuthorityCommand {
    Supply {
        reference: String,
        value: crate::credentials::Secret,
    },
    Forget {
        reference: String,
    },
    Grant {
        environment: String,
        revision: Revision,
        provider: String,
        seconds: u64,
    },
    Revoke {
        environment: String,
        revision: Revision,
        provider: String,
    },
    Transfer {
        origin: String,
        destination: String,
        revision: Revision,
        seconds: u64,
    },
}

pub struct EnvironmentPlan {
    plan: Plan,
    record: EnvironmentRecord,
    images: crate::environments::ExecutionImages,
    _credit: OwnedSemaphorePermit,
}

impl Actor {
    /// One write-ahead namespace anchor for a fresh configured workspace. No command or provider
    /// is executed here. On reopen, ordinary environment replay restores this same identity.
    pub(super) async fn initialize_default_namespace(&mut self) -> Result<(), SessionError> {
        if self.workspace.sandbox_runtime {
            return Ok(());
        }
        let Some(name) = &self.workspace.default_environment else {
            return Ok(());
        };
        let authority = self
            .workspace
            .environment_loader
            .as_ref()
            .and_then(|loader| loader.authority())
            .ok_or(SessionError::Preparation)?;
        let environment = self
            .workspace
            .environments()
            .inspect(name)
            .ok_or(SessionError::Preparation)?;
        if authority.origin(environment.identity()).is_some() {
            return Ok(());
        }
        if !self
            .workspace
            .environments()
            .recorded_revisions()
            .is_empty()
        {
            return Err(SessionError::Preparation);
        }
        let (_, record) = EnvironmentRecord::capture(
            "version: 1\nenvironments: {}\n".into(),
            CapturedSources::default(),
            self.workspace.environments(),
        )?;
        if let RecordingMode::Required(journal) = &self.recording {
            let (recorder, required) = journal.recording();
            let receipt = recorder
                .append(Arc::new(Record::Journal(JournalEntry::Environments(
                    record.clone(),
                ))))
                .await
                .map_err(|_| SessionError::Recording)?;
            if !required.accepts(receipt.persistence) {
                return Err(SessionError::Recording);
            }
        }
        authority.establish_namespace(record.id());
        self.workspace.environment_records.push(record.clone());
        self.workspace.environment_record = Some(record);
        Ok(())
    }
}
impl EnvironmentPlan {
    pub fn changes(&self) -> &[Change] {
        self.plan.changes()
    }
    pub fn revisions(&self) -> &BTreeMap<String, Revision> {
        self.record.after()
    }
    pub fn id(&self) -> &str {
        self.record.id()
    }
}
impl std::fmt::Debug for EnvironmentPlan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SessionEnvironmentPlan")
            .field("id", &self.id())
            .field("changes", &self.changes())
            .finish_non_exhaustive()
    }
}
#[derive(Clone, Debug)]
pub struct EnvironmentPublication {
    pub id: String,
    pub changes: Vec<Change>,
    /// True only after required persistence was acknowledged; false for ephemeral/no-op publication.
    pub recorded: bool,
}
pub(super) enum PlanReply {
    Auto(SubmissionResult),
    Typed(oneshot::Sender<Result<EnvironmentPlan, SessionError>>),
    Source {
        client: String,
        name: String,
        result: SubmissionResult,
    },
}
pub(super) enum ApplyReply {
    Typed(oneshot::Sender<Result<EnvironmentPublication, SessionError>>),
    Source(SubmissionResult),
}
impl PlanReply {
    fn reject(self, actor: &mut Actor, error: SessionError) {
        match self {
            Self::Auto(result) => actor.finish_cell(&result.cell, Err(error)),
            Self::Typed(reply) => {
                let _ = reply.send(Err(error));
            }
            Self::Source { result, .. } => actor.finish_cell(&result.cell, Err(error)),
        }
    }
}
impl ApplyReply {
    fn complete(self, actor: &mut Actor, outcome: Result<EnvironmentPublication, SessionError>) {
        match self {
            Self::Typed(reply) => {
                let _ = reply.send(outcome);
            }
            Self::Source(mut result) => match outcome {
                Ok(publication) => {
                    result.recorded = publication.recorded;
                    result.diagnostics.diagnostics.push(
                        Diagnostic::error(
                            "ENV000",
                            Span::at(0),
                            format!(
                                "published {} environment {}",
                                publication.changes.len(),
                                if publication.changes.len() == 1 {
                                    "change"
                                } else {
                                    "changes"
                                }
                            ),
                        )
                        .with_severity(wes_language::Severity::Info),
                    );
                    actor.finish_immediate(result);
                }
                Err(error) => actor.finish_cell(&result.cell, Err(error)),
            },
        }
    }
}
#[derive(Clone, Debug)]
pub struct EnvironmentAuthentication {
    pub environment: Arc<wes_core::environments::EffectiveEnvironment>,
    pub providers: BTreeMap<String, crate::environments::CredentialStatus>,
}
pub enum ProviderAuthorityCommand {
    SupplyWithPersistence {
        slot: String,
        value: crate::credentials::Secret,
        remember: bool,
    },
    Supply {
        slot: String,
        value: crate::credentials::Secret,
    },
    Forget {
        slot: String,
    },
    Grant,
    Revoke,
    Enable,
}
pub(super) enum Request {
    ProviderAuthority {
        environment: String,
        revision: Revision,
        provider: String,
        command: ProviderAuthorityCommand,
        reply: oneshot::Sender<Result<(), SessionError>>,
    },
    Authentication(oneshot::Sender<Result<Vec<EnvironmentAuthentication>, SessionError>>),
    PrepareTarget {
        environment: String,
        revision: Revision,
        target: String,
        reply: oneshot::Sender<Result<crate::execution::TargetPreparation, SessionError>>,
    },
    Authority(
        EnvironmentAuthorityCommand,
        oneshot::Sender<Result<(), SessionError>>,
    ),
    Plan {
        reconcile_file: bool,
        edit: Option<crate::environments::DefinitionEdit>,
        file: Option<String>,
        text: Option<(Option<String>, Option<String>)>,
        yaml: String,
        sources: CapturedSources,
        credit: OwnedSemaphorePermit,
        reply: PlanReply,
    },
    Apply {
        plan: EnvironmentPlan,
        reply: ApplyReply,
    },
    Observe(oneshot::Sender<BTreeMap<String, Revision>>),
    Documents(oneshot::Sender<Result<Vec<crate::environments::EnvironmentDocument>, SessionError>>),
}
pub(super) enum Completed {
    Authority {
        reply: oneshot::Sender<Result<(), SessionError>>,
        result: Result<(), SessionError>,
    },
    Authentication {
        reply: oneshot::Sender<Result<Vec<EnvironmentAuthentication>, SessionError>>,
        result: Result<Vec<EnvironmentAuthentication>, SessionError>,
    },
    Exported {
        result: SubmissionResult,
        outcome: Result<(), SessionError>,
    },
    Planned {
        reply: PlanReply,
        result: Result<EnvironmentPlan, SessionError>,
    },
    Recorded {
        reply: ApplyReply,
        plan: EnvironmentPlan,
        result: Result<bool, SessionError>,
    },
}
impl SessionHandle {
    /// Strict consumers require an exact revision and cannot accept a UI review.
    pub async fn capture_execution_target(
        &self,
        environment: String,
        revision: Revision,
        target: String,
    ) -> Result<crate::execution::TargetLease, SessionError> {
        match self
            .prepare_execution_target(environment, revision, target)
            .await?
        {
            crate::execution::TargetPreparation::Ready(lease) => Ok(lease),
            crate::execution::TargetPreparation::Review(_) => Err(SessionError::Environment(
                wes_core::environments::EnvironmentError {
                    code: "ENV020",
                    message:
                        "Terminal environment changed; review its current target before starting"
                            .into(),
                },
            )),
        }
    }

    /// Admission either captures a revocable lease or returns changes for explicit review.
    pub async fn prepare_execution_target(
        &self,
        environment: String,
        revision: Revision,
        target: String,
    ) -> Result<crate::execution::TargetPreparation, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::PrepareTarget {
                environment,
                revision,
                target,
                reply,
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }

    pub async fn plan_environment_file(
        &self,
        path: String,
        reconcile_file: bool,
    ) -> Result<EnvironmentPlan, SessionError> {
        if path.is_empty() || path.len() > 4096 {
            return Err(SessionError::Capacity);
        }
        let credit = self
            .environment_credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Plan {
                reconcile_file,
                edit: None,
                file: Some(path),
                text: None,
                yaml: String::new(),
                sources: CapturedSources::default(),
                credit,
                reply: PlanReply::Typed(reply),
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn environment_authority(
        &self,
        command: EnvironmentAuthorityCommand,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Authority(command, reply)))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    /// Inputs are captured bytes, not paths to read. Two credits bound queued/work/client-held plans.
    pub async fn plan_environments(
        &self,
        yaml: String,
        sources: CapturedSources,
    ) -> Result<EnvironmentPlan, SessionError> {
        if self.sources.is_closed() {
            return Err(SessionError::Stopped);
        }
        if yaml.len() > 1_048_576 {
            return Err(SessionError::Capacity);
        }
        let credit = self
            .environment_credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Plan {
                reconcile_file: false,
                edit: None,
                file: None,
                text: None,
                yaml,
                sources,
                credit,
                reply: PlanReply::Typed(reply),
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn apply_environments(
        &self,
        plan: EnvironmentPlan,
    ) -> Result<EnvironmentPublication, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Apply {
                plan,
                reply: ApplyReply::Typed(reply),
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn environment_revisions(&self) -> Result<BTreeMap<String, Revision>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Observe(reply)))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)
    }
    pub async fn provider_authority(
        &self,
        environment: String,
        revision: Revision,
        provider: String,
        command: ProviderAuthorityCommand,
    ) -> Result<(), SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::ProviderAuthority {
                environment,
                revision,
                provider,
                command,
                reply,
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn environment_authentication(
        &self,
    ) -> Result<Vec<EnvironmentAuthentication>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Authentication(reply)))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn plan_environment_document(
        &self,
        source: String,
        origin: String,
    ) -> Result<EnvironmentPlan, SessionError> {
        if source.len() > 1_048_576 || !origin.starts_with("environment-editor/") {
            return Err(SessionError::Capacity);
        }
        let credit = self
            .environment_credit
            .clone()
            .try_acquire_owned()
            .map_err(|_| SessionError::Capacity)?;
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Plan {
                reconcile_file: false,
                edit: None,
                file: None,
                text: Some((Some(origin), None)),
                yaml: source,
                sources: CapturedSources::default(),
                credit,
                reply: PlanReply::Typed(reply),
            }))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
    pub async fn environment_documents(
        &self,
    ) -> Result<Vec<crate::environments::EnvironmentDocument>, SessionError> {
        let (reply, receive) = oneshot::channel();
        self.controls
            .send(Control::environment(Request::Documents(reply)))
            .await
            .map_err(|_| SessionError::Stopped)?;
        receive.await.map_err(|_| SessionError::Stopped)?
    }
}
impl Actor {
    pub(super) fn environment_source(
        &mut self,
        input: SourceInput,
        statement: wes_language::Statement,
        mut result: SubmissionResult,
    ) {
        use wes_language::{Expression, Value};
        let Expression::Call(call) = &statement.expression else {
            unreachable!("checked env command")
        };
        let spec = match wes_language::vocabulary::EnvironmentCommand::validate(&statement) {
            Ok(spec) => spec,
            Err(diagnostic) => {
                result.diagnostics.diagnostics.push(diagnostic);
                self.finish_immediate(result);
                return;
            }
        };
        let mut args: BTreeMap<_, _> = call
            .arguments
            .iter()
            .map(|argument| {
                (
                    argument.key.text.as_str(),
                    argument.value.name().text.clone(),
                )
            })
            .collect();
        if spec.name_operand {
            args.insert("name", call.operands[0].name().text.clone());
        }
        let argument_span = |key: &str| {
            if key == "name" && spec.name_operand {
                call.operands[0].name().span
            } else {
                call.arguments
                    .iter()
                    .find(|a| a.key.text == key)
                    .map_or(statement.span, |a| a.value.name().span)
            }
        };
        let invalid_at = |result: &mut SubmissionResult, span: Span, message: &'static str| {
            result
                .diagnostics
                .diagnostics
                .push(Diagnostic::error("ENV001", span, message).with_public_message(message));
        };
        let invalid = |result: &mut SubmissionResult, message: &'static str| {
            invalid_at(result, statement.span, message);
        };
        let action = spec.name;
        result.accepted.push(statement.span);
        match action {
            "export" => {
                let Some(path) = args.get("file").cloned() else {
                    invalid(&mut result, "export requires a new explicit file:");
                    self.finish_immediate(result);
                    return;
                };
                if let Err(error) = self.environment_admission() {
                    self.finish_cell(&result.cell, Err(error));
                    return;
                }
                let Some(loader) = self.workspace.environment_loader.clone() else {
                    invalid(&mut result, "export unavailable");
                    self.finish_immediate(result);
                    return;
                };
                let records = self.workspace.environment_records.clone();
                self.environment_work.spawn(async move {
                    let outcome =
                        tokio::task::spawn_blocking(move || loader.export(&path, &records))
                            .await
                            .map_err(|_| SessionError::Preparation)
                            .and_then(|r| r.map_err(SessionError::Environment));
                    Completed::Exported { result, outcome }
                });
                return;
            }
            "rename" | "retire" | "delete" => {
                let Some(name) = args.get("name") else {
                    invalid(&mut result, "operation requires name:");
                    self.finish_immediate(result);
                    return;
                };
                let edit = match action {
                    "rename" => {
                        let Some(to) = args.get("to") else {
                            invalid(&mut result, "rename requires to:");
                            self.finish_immediate(result);
                            return;
                        };
                        crate::environments::DefinitionEdit::Rename {
                            name: name.clone(),
                            to: to.clone(),
                        }
                    }
                    "retire" => crate::environments::DefinitionEdit::Retire { name: name.clone() },
                    _ => crate::environments::DefinitionEdit::Delete { name: name.clone() },
                };
                self.environment_edit(edit, result);
                return;
            }
            "plan" => {
                let Some(name) = statement.binding.as_ref() else {
                    invalid(
                        &mut result,
                        "expected :env plan file:<path> or source:<text> > proposed",
                    );
                    self.finish_immediate(result);
                    return;
                };
                let file = args.get("file");
                let source = args.get("source");
                let problem = if file.is_none() && source.is_none() {
                    Some((
                        statement.span,
                        "Missing plan input. Expected file:<path> or source:<text>.",
                    ))
                } else if file.is_some() && source.is_some() {
                    Some((
                        argument_span("source"),
                        "Conflicting plan inputs. Supply only one of file: and source:.",
                    ))
                } else if file.is_some_and(|p| p.is_empty()) {
                    Some((
                        argument_span("file"),
                        "Environment plan file path is empty.",
                    ))
                } else if file.is_some_and(|p| p.len() > 4096) {
                    Some((
                        argument_span("file"),
                        "Environment plan file path exceeds 4096 bytes.",
                    ))
                } else if file.is_some_and(|p| p.chars().any(char::is_control)) {
                    Some((
                        argument_span("file"),
                        "Environment plan file path contains a control character.",
                    ))
                } else if source.is_some_and(|s| s.len() > crate::source::max_source_bytes()) {
                    Some((
                        argument_span("source"),
                        "Environment plan source exceeds 1 MiB.",
                    ))
                } else if file.is_some() && args.contains_key("base") {
                    Some((argument_span("base"), "base: requires source: input."))
                } else if file.is_some() && args.contains_key("origin") {
                    Some((argument_span("origin"), "origin: requires source: input."))
                } else if self
                    .workspace
                    .bindings()
                    .resolve(&name.name.text, self.workspace.runtime().graph())
                    .is_some()
                {
                    Some((
                        name.span,
                        "This name is already bound to an output reference.",
                    ))
                } else if self
                    .environment_plans
                    .contains_key(&(input.client().into(), name.name.text.clone()))
                {
                    Some((name.span, "Environment plan binding is already in use."))
                } else {
                    None
                };
                if let Some((span, reason)) = problem {
                    invalid_at(&mut result, span, reason);
                    self.finish_immediate(result);
                    return;
                }
                let Ok(credit) = self.environment_credit.clone().try_acquire_owned() else {
                    self.finish_cell(&result.cell, Err(SessionError::Capacity));
                    return;
                };
                self.environment_request(Request::Plan {
                    reconcile_file: match args.get("reconcile").map(String::as_str) {
                        None => false,
                        Some("file") if file.is_some() => true,
                        Some("source") if source.is_some() => true,
                        _ => {
                            invalid_at(
                                &mut result,
                                argument_span("reconcile"),
                                "reconcile: must match the supplied input kind (file or source).",
                            );
                            self.finish_immediate(result);
                            return;
                        }
                    },
                    edit: None,
                    file: file.cloned(),
                    text: source.map(|_| (args.get("origin").cloned(), args.get("base").cloned())),
                    yaml: source.cloned().unwrap_or_default(),
                    sources: CapturedSources::default(),
                    credit,
                    reply: PlanReply::Source {
                        client: input.client().into(),
                        name: name.name.text.clone(),
                        result,
                    },
                });
                return;
            }
            "apply" | "discard" => {
                let [Value::Reference(name)] = call.operands.as_slice() else {
                    invalid(&mut result, "expected :env apply $proposed");
                    self.finish_immediate(result);
                    return;
                };
                if self
                    .workspace
                    .bindings()
                    .resolve(&name.text, self.workspace.runtime().graph())
                    .is_some()
                {
                    invalid_at(
                        &mut result,
                        name.span,
                        "Expected an environment plan; received an output reference.",
                    );
                    self.finish_immediate(result);
                    return;
                }
                let Some(plan) = self
                    .environment_plans
                    .remove(&(input.client().into(), name.text.clone()))
                else {
                    invalid_at(
                        &mut result,
                        name.span,
                        "No live environment plan with this name belongs to this client. Plan again after load.",
                    );
                    self.finish_immediate(result);
                    return;
                };
                if action == "discard" {
                    drop(plan);
                    self.finish_immediate(result);
                    return;
                }
                self.environment_request(Request::Apply {
                    plan,
                    reply: ApplyReply::Source(result),
                });
                return;
            }
            "clear" | "use" => {
                if self.environment_clients.len() >= 128
                    && !self.environment_clients.contains_key(input.client())
                {
                    self.finish_cell(&result.cell, Err(SessionError::Capacity));
                    return;
                }
                let revisions = self.workspace.environments().revisions();
                let selected = if action == "clear" {
                    None
                } else {
                    let Some(name) = args.get("name") else {
                        invalid(&mut result, "use requires name:");
                        self.finish_immediate(result);
                        return;
                    };
                    let Some(revision) = revisions.get(name) else {
                        invalid_at(
                            &mut result,
                            argument_span("name"),
                            "Unknown environment name.",
                        );
                        self.finish_immediate(result);
                        return;
                    };
                    if self
                        .workspace
                        .environments()
                        .inspect(name)
                        .is_some_and(|e| e.is_retired())
                    {
                        invalid(
                            &mut result,
                            "retired environment cannot be selected for new work",
                        );
                        self.finish_immediate(result);
                        return;
                    }
                    if args
                        .get("revision")
                        .is_some_and(|r| r != &revision.to_string())
                    {
                        invalid(&mut result, "environment revision changed");
                        self.finish_immediate(result);
                        return;
                    }
                    Some(name.clone())
                };
                self.environment_clients.insert(
                    input.client().into(),
                    wes_core::environments::EnvironmentContext {
                        selected: selected.clone(),
                        revisions,
                    },
                );
                result.diagnostics.diagnostics.push(
                    Diagnostic::error(
                        "ENV000",
                        statement.span,
                        selected.map_or_else(
                            || "environment selection cleared; there is no local fallback".into(),
                            |name| format!("selected environment '{name}' for this client"),
                        ),
                    )
                    .with_severity(wes_language::Severity::Info),
                );
            }
            "disable" | "enable" => {
                let Some(name) = args
                    .get("name")
                    .filter(|name| self.workspace.environments().inspect(name).is_some())
                else {
                    invalid_at(
                        &mut result,
                        argument_span("name"),
                        "Unknown environment name.",
                    );
                    self.finish_immediate(result);
                    return;
                };
                let Some(authority) = self
                    .workspace
                    .environment_loader
                    .as_ref()
                    .and_then(|l| l.authority())
                else {
                    invalid(&mut result, "environment authority is unavailable");
                    self.finish_immediate(result);
                    return;
                };
                let identity = self
                    .workspace
                    .environments()
                    .inspect(name)
                    .expect("checked environment")
                    .identity();
                let changed = if action == "disable" {
                    authority.disable(identity)
                } else {
                    authority.enable(identity)
                };
                if changed.is_err() {
                    invalid(&mut result, "environment authority change failed");
                } else {
                    result.diagnostics.diagnostics.push(Diagnostic::error("ENV000", statement.span, format!("{action} '{name}' for this live session; credential grants are separate")).with_severity(wes_language::Severity::Info));
                }
            }
            _ => invalid(&mut result, "unknown environment subcommand"),
        }
        self.finish_immediate(result);
    }
    fn environment_edit(
        &mut self,
        edit: crate::environments::DefinitionEdit,
        result: SubmissionResult,
    ) {
        let Ok(credit) = self.environment_credit.clone().try_acquire_owned() else {
            self.finish_cell(&result.cell, Err(SessionError::Capacity));
            return;
        };
        self.environment_request(Request::Plan {
            reconcile_file: false,
            edit: Some(edit),
            file: None,
            text: None,
            yaml: String::new(),
            sources: CapturedSources::default(),
            credit,
            reply: PlanReply::Auto(result),
        });
    }
    pub(super) fn environment_import(
        &mut self,
        input: SourceInput,
        statements: &[wes_language::Statement],
    ) {
        let result = SubmissionResult {
            receipts: vec![],
            sandbox: None,
            cell: input.cell().into(),
            nodes: vec![],
            accepted: statements.iter().map(|s| s.span).collect(),
            removed: vec![],
            unbound: vec![],
            diagnostics: SourceDiagnostics::default(),
            recorded: false,
            restored: false,
            refreshed: vec![],
            repeated_run: None,
        };
        let parse = || -> Result<crate::environments::DefinitionEdit, &'static str> {
            use wes_language::{Expression, Value};
            let [statement] = statements else {
                return Err(
                    "managed imports must be submitted individually; definition changes and calls cannot share a submission",
                );
            };
            if !statement.annotations.is_empty()
                || statement.binding.is_some()
                || statement.error_binding.is_some()
            {
                return Err("import does not accept annotations or output bindings");
            }
            let Expression::Call(call) = &statement.expression else {
                unreachable!()
            };
            if call.path.len() != 2 || !call.operands.is_empty() {
                return Err(
                    "expected :import spec file:<path> or url:<http(s) URL> as:<alias> [env:<name>] [target:<name>] [replace:true]",
                );
            }
            let kind = call.path[1].text.as_str();
            if !matches!(kind, "spec" | "openapi" | "process" | "docker") {
                return Err("unsupported managed import kind");
            }
            let mut args = BTreeMap::new();
            for arg in &call.arguments {
                if ![
                    "file", "url", "bin", "socket", "as", "env", "target", "replace", "endpoint",
                ]
                .contains(&arg.key.text.as_str())
                {
                    return Err("unsupported managed import argument");
                }
                let value = match &arg.value {
                    Value::Word(v) | Value::Text(v) => v.text.clone(),
                    _ => return Err("managed import arguments must be literal text"),
                };
                if args.insert(arg.key.text.as_str(), value).is_some() {
                    return Err("duplicate import argument");
                }
            }
            let context = input
                .environments()
                .or_else(|| self.environment_clients.get(input.client()));
            let environment = args
                .get("env")
                .or_else(|| context.and_then(|c| c.selected.as_ref()))
                .ok_or("select an environment or pass env: explicitly")?;
            let current = self
                .workspace
                .environments()
                .inspect(environment)
                .ok_or("import environment unavailable")?;
            if context.is_some_and(|c| c.revisions.get(environment) != Some(&current.revision())) {
                return Err("environment revision changed; inspect and select again");
            }
            if current.is_retired() {
                return Err("retired environment cannot receive imports");
            }
            if args.contains_key(if matches!(kind, "spec" | "openapi") {
                "bin"
            } else {
                "file"
            }) {
                return Err("source path field does not match importer kind");
            }
            if kind != "docker" && args.contains_key("socket") {
                return Err("socket: requires the docker importer");
            }
            let field = if kind == "docker" {
                if ["file", "url", "bin", "endpoint"]
                    .iter()
                    .any(|key| args.contains_key(key))
                {
                    return Err("Docker observation requires socket:, not file/url/bin/endpoint");
                }
                "socket"
            } else if matches!(kind, "spec" | "openapi") {
                match (args.contains_key("file"), args.contains_key("url")) {
                    (true, false) => "file",
                    (false, true) => "url",
                    _ => return Err("spec requires exactly one of file: or url:"),
                }
            } else {
                if args.contains_key("url") || args.contains_key("endpoint") {
                    return Err("process imports do not accept url: or endpoint:");
                }
                "bin"
            };
            let location = args.get(field).ok_or("missing importer source path")?;
            let source = wes_core::environments::SourceKey::new(kind, location)
                .map_err(|_| "invalid source location")?;
            if source.source_field() != field {
                return Err("spec url requires http:// or https://; use file: for local paths");
            }
            let alias = args
                .get("as")
                .ok_or("managed imports require explicit as:")?;
            let replace = match args.get("replace").map(String::as_str) {
                None | Some("false") => false,
                Some("true") => true,
                _ => return Err("replace must be true or false"),
            };
            Ok(crate::environments::DefinitionEdit::Import {
                environment: environment.clone(),
                alias: alias.clone(),
                kind: kind.into(),
                location: location.clone(),
                target: args
                    .get("target")
                    .cloned()
                    .unwrap_or_else(|| "local".into()),
                replace,
                endpoint: args.get("endpoint").cloned(),
            })
        };
        match parse() {
            Ok(edit) => self.environment_edit(edit, result),
            Err(message) => self.finish_cell(
                &result.cell,
                Err(SessionError::Environment(
                    wes_core::environments::EnvironmentError {
                        code: "ENV011",
                        message: message.into(),
                    },
                )),
            ),
        }
    }
    fn environment_admission(&self) -> Result<(), SessionError> {
        if self.workspace.runtime().is_closed() {
            Err(SessionError::Stopped)
        } else if self.checkpoints.busy() {
            Err(SessionError::CheckpointBusy)
        } else if self.recording_failed {
            Err(SessionError::Recording)
        } else if self.active.is_some() || !self.environment_work.is_empty() {
            Err(SessionError::AdmissionBusy)
        } else {
            Ok(())
        }
    }
    pub(super) fn environment_request(&mut self, request: Request) {
        match request {
            Request::PrepareTarget {
                environment,
                revision,
                target,
                reply,
            } => {
                let result = (|| {
                    self.environment_admission()?;
                    let reject = |message: &str| {
                        SessionError::Environment(wes_core::environments::EnvironmentError {
                            code: "ENV020",
                            message: message.into(),
                        })
                    };
                    let env = self
                        .workspace
                        .environments()
                        .inspect(&environment)
                        .cloned()
                        .ok_or_else(|| reject("Terminal environment is unavailable"))?;
                    if env.is_abstract() || env.is_retired() {
                        return Err(reject(
                            "Terminal environment must be runnable and not retired",
                        ));
                    }
                    let target = env
                        .execution_targets()
                        .remove(&target)
                        .ok_or_else(|| reject("Target is not attached to this environment"))??;
                    let authority = self
                        .workspace
                        .environment_loader
                        .as_ref()
                        .and_then(|loader| loader.authority())
                        .ok_or_else(|| reject("Execution authority is unavailable"))?;
                    if !authority.available(env.identity()) {
                        return Err(reject("Terminal environment execution is disabled"));
                    }
                    if env.revision() != revision {
                        return Ok(crate::execution::TargetPreparation::Review(
                            crate::execution::TargetReview {
                                previous_revision: revision,
                                previous: self
                                    .workspace
                                    .environments()
                                    .retained_revision(&environment, revision)
                                    .cloned(),
                                current: env,
                                target,
                            },
                        ));
                    }
                    let cancelled = authority
                        .availability_lease(env.identity())
                        .map_err(|_| reject("Terminal environment execution is disabled"))?;
                    let reference = self
                        .workspace
                        .target_leases
                        .acquire(env.identity())
                        .map_err(reject)?;
                    Ok(crate::execution::TargetPreparation::Ready(
                        crate::execution::TargetLease {
                            _reference: reference,
                            environment,
                            revision,
                            target,
                            cancelled,
                        },
                    ))
                })();
                let _ = reply.send(result);
            }
            Request::Authority(command, reply) => {
                if !self.environment_work.is_empty() {
                    let _ = reply.send(Err(SessionError::AdmissionBusy));
                    return;
                }
                let authority = self
                    .workspace
                    .environment_loader
                    .as_ref()
                    .and_then(|l| l.authority());
                let Some(authority) = authority else {
                    let _ = reply.send(Err(SessionError::Preparation));
                    return;
                };
                use EnvironmentAuthorityCommand::*;
                let job: Box<
                    dyn FnOnce() -> Result<(), crate::credentials::CredentialError> + Send,
                > = match command {
                    Supply { reference, value } => {
                        Box::new(move || authority.supply(reference, value))
                    }
                    Forget { reference } => Box::new(move || authority.forget(&reference)),
                    Grant {
                        environment,
                        revision,
                        provider,
                        seconds,
                    } => {
                        let binding = self
                            .workspace
                            .environments()
                            .retained_revision(&environment, revision)
                            .and_then(|e| e.bind(&provider).ok());
                        let Some(binding) = binding else {
                            let _ = reply.send(Err(SessionError::Environment(wes_core::environments::EnvironmentError {
                                code: "ENV005", message: "Credential grant target is unavailable in the requested environment revision; inspect and select the environment and provider before granting access.".into(),
                            })));
                            return;
                        };
                        Box::new(move || {
                            authority.hydrate(&binding)?;
                            authority.grant(&binding, &provider, Duration::from_secs(seconds))
                        })
                    }
                    other => {
                        let _ = reply.send(self.change_environment_authority(other));
                        return;
                    }
                };
                self.environment_work.spawn(async move {
                    let result = tokio::task::spawn_blocking(job)
                        .await
                        .unwrap_or(Err(crate::credentials::CredentialError::Unavailable))
                        .map_err(SessionError::Credential);
                    Completed::Authority { reply, result }
                });
            }
            Request::Observe(reply) => {
                let _ = reply.send(self.workspace.environments().revisions());
            }
            Request::ProviderAuthority {
                environment,
                revision,
                provider,
                command,
                reply,
            } => {
                if !self.environment_work.is_empty() {
                    let _ = reply.send(Err(SessionError::AdmissionBusy));
                    return;
                }
                let prepared = (|| {
                    if self.workspace.runtime().is_closed() {
                        return Err(SessionError::Stopped);
                    }
                    let denied = || {
                        SessionError::Environment(wes_core::environments::EnvironmentError { code:"ENV022", message:"Authentication control refused; refresh the environment and check activation and credential bindings.".into() })
                    };
                    let env = self
                        .workspace
                        .environments()
                        .inspect(&environment)
                        .filter(|e| e.revision() == revision && !e.is_retired() && !e.is_abstract())
                        .ok_or_else(denied)?;
                    let binding = env.bind(&provider)?;
                    let authority = self
                        .workspace
                        .environment_loader
                        .as_ref()
                        .and_then(|l| l.authority())
                        .ok_or_else(denied)?;
                    Ok((authority, binding, env.identity().to_owned()))
                })();
                let (authority, binding, identity) = match prepared {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                };
                self.environment_work.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        use ProviderAuthorityCommand::*;
                        match command {
                            Supply {slot, value} => authority.supply(binding.import().credential_refs().get(&slot).ok_or(crate::credentials::CredentialError::InvalidName)?.clone(), value),
                            SupplyWithPersistence {slot, value, remember} => authority.supply_with_persistence(binding.import().credential_refs().get(&slot).ok_or(crate::credentials::CredentialError::InvalidName)?.clone(), value, remember),
                            Forget {slot} => authority.forget(binding.import().credential_refs().get(&slot).ok_or(crate::credentials::CredentialError::InvalidName)?),
                            Grant => { authority.hydrate(&binding)?; authority.grant(&binding, &provider, Duration::from_secs(300)) },
                            Revoke => authority.revoke(&binding, &provider),
                            Enable => authority.enable(&identity),
                        }
                    }).await.unwrap_or(Err(crate::credentials::CredentialError::Unavailable)).map_err(|error| SessionError::Environment(wes_core::environments::EnvironmentError {code: "ENV022", message: match error {
                        crate::credentials::CredentialError::Locked => "The credential vault is locked or not set up. Unlock or create it, then retry.",
                        _ => "Authentication control could not be confirmed. Check the secure credential store and refresh setup before retrying.",
                    }.into()}));
                    Completed::Authority {reply, result}
                });
            }
            Request::Authentication(reply) => {
                if !self.environment_work.is_empty() {
                    let _ = reply.send(Err(SessionError::AdmissionBusy));
                    return;
                }
                let prepared = (|| {
                    let authority = self
                        .workspace
                        .environment_loader
                        .as_ref()
                        .and_then(|l| l.authority())
                        .ok_or(SessionError::Preparation)?;
                    let environments = self
                        .workspace
                        .environments()
                        .names()
                        .filter_map(|name| self.workspace.environments().inspect(name))
                        .filter(|e| !e.is_retired() && !e.is_abstract())
                        .cloned()
                        .collect::<Vec<_>>();
                    Ok::<_, SessionError>((authority, environments))
                })();
                let (authority, environments) = match prepared {
                    Ok(value) => value,
                    Err(error) => {
                        let _ = reply.send(Err(error));
                        return;
                    }
                };
                self.environment_work.spawn(async move {
                    let result = tokio::task::spawn_blocking(move || {
                        environments
                            .into_iter()
                            .map(|environment| {
                                let providers = environment
                                    .imports()
                                    .keys()
                                    .map(|alias| {
                                        let binding = environment.bind(alias)?;
                                        // A locked vault leaves remembered values absent; the
                                        // report still shows the provider so it can be unlocked.
                                        match authority.hydrate(&binding) {
                                            Ok(())
                                            | Err(crate::credentials::CredentialError::Locked) => {}
                                            Err(_) => return Err(SessionError::Preparation),
                                        }
                                        let status = authority
                                            .status(&binding, alias)
                                            .map_err(|_| SessionError::Preparation)?;
                                        Ok((alias.clone(), status))
                                    })
                                    .collect::<Result<_, SessionError>>()?;
                                Ok(EnvironmentAuthentication {
                                    environment: environment.clone(),
                                    providers,
                                })
                            })
                            .collect::<Result<Vec<_>, SessionError>>()
                    })
                    .await
                    .unwrap_or(Err(SessionError::Preparation));
                    Completed::Authentication { reply, result }
                });
            }
            Request::Documents(reply) => {
                let result = (|| {
                    let loader = self
                        .workspace
                        .environment_loader
                        .as_ref()
                        .ok_or(SessionError::Preparation)?;
                    Ok(loader.editor_documents(
                        self.workspace.environment_record.as_ref(),
                        self.workspace.default_document()?,
                        &self.workspace.environments().document_token(),
                    )?)
                })();
                let _ = reply.send(result);
            }
            Request::Plan {
                reconcile_file,
                edit,
                file,
                text,
                yaml,
                sources,
                credit,
                reply,
            } => {
                if let Err(error) = self.environment_admission() {
                    reply.reject(self, error);
                    return;
                }
                let registry = self.workspace.environments().planning_snapshot();
                let loader = self.workspace.environment_loader.clone();
                let images = self.workspace.environment_images.clone();
                let current = self.workspace.environment_record.clone();
                let editor = text
                    .as_ref()
                    .and_then(|(origin, _)| origin.as_deref())
                    .and_then(|origin| origin.strip_prefix("environment-editor/"))
                    .map(str::to_owned);
                let default = if editor.is_some() {
                    self.workspace.default_document()
                } else {
                    Ok(None)
                };
                let history_bytes: u64 = self
                    .workspace
                    .environment_records
                    .iter()
                    .map(|r| r.charge())
                    .sum();
                self.environment_work.spawn(async move {
                    // Join physical parsing/resolution even if the requesting client disappears.
                    let result = tokio::task::spawn_blocking(move || {
                        let default = default?;
                        let document_mode = editor.is_some();
                        let (yaml, sources) = match (edit, file, text) {
                            (Some(edit), _, _) => {
                                let loaded =
                                    loader.as_ref().ok_or(SessionError::Preparation)?.edit(
                                        current.as_ref().ok_or(SessionError::Preparation)?,
                                        &edit,
                                    )?;
                                (loaded.yaml, loaded.sources)
                            }
                            (None, Some(path), _) => {
                                let loaded = loader
                                    .as_ref()
                                    .ok_or(SessionError::Preparation)?
                                    .capture(&path)?;
                                let loaded = loader.as_ref().expect("checked loader").merge(
                                    current.as_ref(),
                                    loaded,
                                    reconcile_file,
                                )?;
                                (loaded.yaml, loaded.sources)
                            }
                            (None, None, Some((origin, base))) => {
                                let loader = loader.as_ref().ok_or(SessionError::Preparation)?;
                                let loaded = if let Some(editor) = &editor {
                                    let (token, package) = editor.rsplit_once('/').ok_or(SessionError::Preparation)?;
                                    if token != registry.document_token() {
                                        return Err(SessionError::Environment(wes_core::environments::EnvironmentError { code: "ENV008", message: "Environment definitions changed since this document opened. Keep your edits and reopen /edit env before planning.".into() }));
                                    }
                                    loader.capture_editor(current.as_ref(), default, &yaml, package, base.as_deref())?
                                } else {
                                    let loaded = loader.capture_text(
                                    &yaml,
                                    origin.as_deref(),
                                    base.as_deref(),
                                    )?;
                                    loader.merge(current.as_ref(), loaded, reconcile_file)?
                                };
                                (loaded.yaml, loaded.sources)
                            }
                            (None, None, None) => (yaml, sources),
                        };
                        let (plan, record) = if document_mode { EnvironmentRecord::capture_document(yaml, sources, &registry)? } else { EnvironmentRecord::capture(yaml, sources, &registry)? };
                        if !plan.changes().is_empty()
                            && history_bytes.saturating_add(record.charge()) > 64 * 1024 * 1024
                        {
                            return Err(SessionError::Capacity);
                        }
                        let images = match loader {
                            Some(loader) => images.build(&plan, loader.as_ref())?,
                            None => images,
                        };
                        Ok(EnvironmentPlan {
                            plan,
                            record,
                            images,
                            _credit: credit,
                        })
                    })
                    .await
                    .unwrap_or(Err(SessionError::Preparation));
                    Completed::Planned { reply, result }
                });
            }
            Request::Apply { plan, reply } => {
                if let ApplyReply::Source(result) = &reply
                    && self
                        .cells
                        .input(&result.cell)
                        .is_some_and(SourceInput::is_cooperative)
                    && plan.changes().iter().any(|change| change.before.is_some())
                {
                    reply.complete(self, Err(SessionError::AccessDenied(
                        "This plan changes existing shared environment definitions. Add definitions under new names, or ask the user to review and apply this plan through wes's user controls.".into())));
                    return;
                }
                let check = self.environment_admission().and_then(|()| {
                    self.workspace
                        .environments()
                        .validate_plan(&plan.plan)
                        .map_err(SessionError::Environment)?;
                    self.workspace
                        .check_environment_plan(&plan.plan)
                        .map_err(|error| match error {
                            WorkspaceError::Rejected { diagnostics, .. } => {
                                SessionError::Environment(
                                    wes_core::environments::EnvironmentError {
                                        code: "ENV025",
                                        message: diagnostics
                                            .into_iter()
                                            .map(|d| d.message)
                                            .collect::<Vec<_>>()
                                            .join("; "),
                                    },
                                )
                            }
                            _ => SessionError::Commit,
                        })
                });
                if let Err(error) = check {
                    reply.complete(self, Err(error));
                    return;
                }
                let recording = self.recording.clone();
                self.environment_work.spawn(async move {
                    let result = if plan.changes().is_empty() {
                        Ok(false)
                    } else if let RecordingMode::Required(journal) = recording {
                        let (recorder, required) = journal.recording();
                        match recorder
                            .append(Arc::new(Record::Journal(JournalEntry::Environments(
                                plan.record.clone(),
                            ))))
                            .await
                        {
                            Ok(receipt) if required.accepts(receipt.persistence) => Ok(true),
                            _ => Err(SessionError::Recording),
                        }
                    } else {
                        Ok(false)
                    };
                    Completed::Recorded {
                        reply,
                        plan,
                        result,
                    }
                });
            }
        }
    }
    fn change_environment_authority(
        &mut self,
        command: EnvironmentAuthorityCommand,
    ) -> Result<(), SessionError> {
        use EnvironmentAuthorityCommand::*;
        if self.workspace.runtime().is_closed() {
            return Err(SessionError::Stopped);
        }
        let denied = || {
            SessionError::Environment(wes_core::environments::EnvironmentError {
            code: "ENV022", message: "scoped authority request refused; check identity, revision, activation and lease (1–3600 seconds)".into()
        })
        };
        let authority = self
            .workspace
            .environment_loader
            .as_ref()
            .and_then(|l| l.authority())
            .ok_or_else(denied)?;
        let binding = |environment: &str, revision: Revision, provider: &str| {
            self.workspace
                .environments()
                .retained_revision(environment, revision)
                .ok_or_else(denied)?
                .bind(provider)
                .map_err(SessionError::Environment)
        };
        match command {
            Supply { .. } | Forget { .. } | Grant { .. } => return Err(denied()),
            Revoke {
                environment,
                revision,
                provider,
            } => authority.revoke(&binding(&environment, revision, &provider)?, &provider),
            Transfer {
                origin,
                destination,
                revision,
                seconds,
            } => {
                if self
                    .workspace
                    .environments()
                    .inspect(&destination)
                    .is_none_or(|e| e.revision() != revision)
                {
                    return Err(denied());
                }
                let origin = self
                    .workspace
                    .environments()
                    .inspect(&origin)
                    .and_then(|e| authority.origin(e.identity()))
                    .ok_or_else(denied)?;
                let destination = self
                    .workspace
                    .environments()
                    .inspect(&destination)
                    .and_then(|e| authority.origin(e.identity()))
                    .ok_or_else(denied)?;
                authority.transfer(
                    origin,
                    format!("{destination}@{revision}"),
                    Duration::from_secs(seconds),
                )
            }
        }
        .map_err(|_| denied())
    }
    pub(super) fn environment_completed(&mut self, completed: Completed) {
        match completed {
            Completed::Authority { reply, result } => {
                let _ = reply.send(result);
            }
            Completed::Authentication { reply, result } => {
                let _ = reply.send(result);
            }
            Completed::Exported {
                mut result,
                outcome,
            } => match outcome {
                Ok(()) => {
                    result.diagnostics.diagnostics.push(Diagnostic::error("ENV000", Span::at(0), "Exported captured environment lock; no result values, credential material or grants included. Local paths and target metadata are included; review them before sharing.").with_severity(wes_language::Severity::Info));
                    self.finish_immediate(result);
                }
                Err(error) => self.finish_cell(&result.cell, Err(error)),
            },
            Completed::Planned { reply, result } => {
                let result = if self.workspace.runtime().is_closed() {
                    Err(SessionError::Stopped)
                } else {
                    result
                };
                match (reply, result) {
                    (PlanReply::Auto(result), Ok(plan)) => {
                        let guarded = plan.changes().iter().any(|change| {
                            plan.plan
                                .inspect(&change.name)
                                .is_some_and(|e| e.is_protected())
                                || self
                                    .workspace
                                    .environments()
                                    .inspect(&change.name)
                                    .is_some_and(|e| e.is_protected())
                        });
                        if guarded {
                            self.finish_cell(&result.cell, Err(SessionError::Environment(wes_core::environments::EnvironmentError { code: "ENV024", message: "change affects a protected environment; edit the definition package and use reviewed plan/apply".into() })));
                        } else {
                            self.environment_request(Request::Apply {
                                plan,
                                reply: ApplyReply::Source(result),
                            });
                        }
                    }
                    (PlanReply::Typed(reply), result) => {
                        let _ = reply.send(result);
                    }
                    (
                        PlanReply::Source {
                            client,
                            name,
                            mut result,
                        },
                        Ok(plan),
                    ) => {
                        if self.workspace.resolve(&name).is_some()
                            || self.reserved_output_names.contains(&name)
                        {
                            result.diagnostics.diagnostics.push(Diagnostic::error("ENV001",Span::at(0),"Plan name was bound to an output while preparation was running.").with_public_message("Plan name was bound to an output while preparation was running."));
                            self.finish_immediate(result);
                            return;
                        }
                        let count = plan.changes().len();
                        let mut preview = format!(
                            "plan '{name}': {count} environment change{}; apply uses these captured inputs\n",
                            if count == 1 { "" } else { "s" }
                        );
                        for change in plan.changes() {
                            preview.push_str(&format!("{change}\n"));
                            if let Some(environment) = plan.plan.inspect(&change.name) {
                                preview.push_str(&format!(
                                    "  id={} abstract={} retired={} protected={}\n",
                                    environment.identity(),
                                    environment.is_abstract(),
                                    environment.is_retired(),
                                    environment.is_protected()
                                ));
                                for (alias, import) in environment.imports() {
                                    preview.push_str(&format!("  {alias}: origin={} target={} endpoint={} private={} credential-refs={:?}\n", import.origin().environment, import.target().name(), import.endpoint().unwrap_or("descriptor-defined"), import.declaration().private_output, import.credential_refs()));
                                }
                            }
                            if preview.len() > 256 * 1024 {
                                preview.truncate(preview.floor_char_boundary(256 * 1024));
                                preview.push_str("\nPreview truncated; inspect individual bindings before applying.\n");
                                break;
                            }
                        }
                        self.environment_plans.insert((client, name.clone()), plan);
                        result.diagnostics.diagnostics.push(
                            Diagnostic::error("ENV000", Span::at(0), preview)
                                .with_severity(wes_language::Severity::Info),
                        );
                        self.finish_immediate(result);
                    }
                    (reply, Err(error)) => reply.reject(self, error),
                }
            }
            Completed::Recorded {
                reply,
                plan,
                result,
            } => {
                let result = match result {
                    Err(error) => {
                        self.recording_failed =
                            matches!(self.recording, RecordingMode::Required(_));
                        Err(error)
                    }
                    Ok(_) if self.workspace.runtime().is_closed() => Err(SessionError::Stopped),
                    Ok(_) if self.recording_failed => Err(SessionError::Recording),
                    Ok(recorded) => match self.workspace.apply_environment_plan(plan.plan) {
                        Ok(changes) => {
                            if let Some(authority) = self
                                .workspace
                                .environment_loader
                                .as_ref()
                                .and_then(|l| l.authority())
                            {
                                authority.establish_namespace(plan.record.id());
                            }
                            self.workspace.environment_images = plan.images;
                            if !changes.is_empty() {
                                self.workspace.environment_records.push(plan.record.clone());
                            }
                            self.workspace.environment_record = Some(plan.record.clone());
                            Ok(EnvironmentPublication {
                                id: plan.record.id().into(),
                                changes,
                                recorded,
                            })
                        }
                        Err(_) => {
                            self.recording_failed =
                                matches!(self.recording, RecordingMode::Required(_));
                            Err(SessionError::Commit)
                        }
                    },
                };
                reply.complete(self, result);
            }
        }
    }
}
