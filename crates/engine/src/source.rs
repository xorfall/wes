//! Source-to-declaration preparation. No live workspace mutation, journal writes or execution.
use crate::{
    calls::AdmittedCommand,
    driver::CancellationToken,
    graph::NodeId,
    history::CommandRecord,
    imports::{ImportCapture, ImportError},
    type_sources::{TypeSourceCapture, TypeSourceError},
    workspace::{
        BatchApplied, DeclarationDraft, Preparation, PreparedBatch, PreparedChange,
        PreparedControl, Workspace, WorkspaceError,
    },
};
use thiserror::Error;
use wes_core::ValidationIssue;
use wes_language::{
    Diagnostic, Expression, Severity, SourceText, Span, Statement, vocabulary::MetaCommand,
};

pub fn max_source_bytes() -> usize {
    wes_budgets::get("source.bytes") as usize
}
fn max_statements() -> usize {
    wes_budgets::get("source.statements") as usize
}
const MAX_CELL_BYTES: usize = 256;

pub(crate) fn validate_source(cell: &str, text: &str) -> Result<(), SourceError> {
    if cell.trim().is_empty() || cell.len() > MAX_CELL_BYTES || cell.chars().any(char::is_control) {
        return Err(SourceError::Identity);
    }
    if text.len() > max_source_bytes() {
        return Err(SourceError::Capacity);
    }
    Ok(())
}

pub(crate) fn validate_source_name(name: &str) -> Result<(), SourceError> {
    if name.trim().is_empty() || name.len() > 512 || name.chars().any(char::is_control) {
        return Err(SourceError::Identity);
    }
    Ok(())
}

/// A client submission identity is separate from source text and generated node/run identities.
#[derive(Clone)]
pub struct SourceInput {
    client: String,
    cell: String,
    source_name: String,
    source_start: wes_language::Position,
    text: String,
    document: Option<String>,
    environments: Option<wes_core::environments::EnvironmentContext>,
    repeat: Option<Repeat>,
    revision_of: Option<String>,
    cooperative: bool,
    reactive: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Repeat {
    pub origin: String,
    pub acknowledge_effects: bool,
    pub from: Option<NodeId>,
}
impl SourceInput {
    pub fn new(cell: String, text: String) -> Result<Self, SourceError> {
        validate_source(&cell, &text)?;
        Ok(Self {
            client: "terminal".into(),
            source_name: format!("cell {cell}"),
            source_start: wes_language::Position { line: 1, column: 1 },
            cell,
            text,
            document: None,
            environments: None,
            repeat: None,
            revision_of: None,
            cooperative: false,
            reactive: false,
        })
    }
    pub fn source_name(&self) -> &str {
        &self.source_name
    }
    pub fn with_source_name(mut self, name: String) -> Result<Self, SourceError> {
        validate_source_name(&name)?;
        self.source_name = wes_language::portable_source_name(&name);
        Ok(self)
    }
    pub fn source_start(&self) -> wes_language::Position {
        self.source_start
    }
    /// Parse top-level workflow steps once before admitting any of them.
    pub fn statement_spans(&self) -> Result<Vec<Span>, Vec<Diagnostic>> {
        let parsed = wes_language::parse(&SourceText::new(self.source_name(), self.text()));
        if parsed
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            return Err(parsed.diagnostics);
        }
        Ok(parsed.script.statements.iter().map(|s| s.span).collect())
    }
    pub(crate) fn workflow_step(&self, cell: String, span: Span) -> Result<Self, SourceError> {
        let origin = SourceText::new(self.source_name(), self.text());
        let start = origin
            .position(span.start())
            .map_err(|_| SourceError::Identity)?;
        let mut step = self.clone();
        validate_source(&cell, &self.text[span.start()..span.end()])?;
        step.cell = cell;
        step.text = self.text[span.start()..span.end()].into();
        let position = wes_language::Position {
            line: self.source_start.line + start.line - 1,
            column: if start.line == 1 {
                self.source_start.column + start.column - 1
            } else {
                start.column
            },
        };
        step.with_source_start(position)
    }
    pub fn with_source_start(mut self, start: wes_language::Position) -> Result<Self, SourceError> {
        if start.line == 0
            || start.column == 0
            || start.line > max_source_bytes() + 1
            || start.column > max_source_bytes() + 1
        {
            return Err(SourceError::Identity);
        }
        self.source_start = start;
        Ok(self)
    }
    /// Raw editor bytes remain separate from command syntax, identity-bound and durable.
    pub fn with_document(mut self, document: Option<String>) -> Result<Self, SourceError> {
        if document
            .as_ref()
            .is_some_and(|s| s.len() > max_source_bytes())
        {
            return Err(SourceError::Capacity);
        }
        self.document = document;
        Ok(self)
    }
    pub fn document(&self) -> Option<&str> {
        self.document.as_deref()
    }
    pub fn with_environments(
        mut self,
        context: wes_core::environments::EnvironmentContext,
    ) -> Result<Self, SourceError> {
        context.validate().map_err(|_| SourceError::Identity)?;
        self.environments = Some(context);
        Ok(self)
    }
    pub fn with_client(mut self, client: String) -> Result<Self, SourceError> {
        if client.is_empty() || client.len() > 128 || client.chars().any(char::is_control) {
            return Err(SourceError::Identity);
        }
        self.client = client;
        Ok(self)
    }
    /// Trusted application boundary assigns the actor; source text cannot grant authority.
    pub fn cooperative(mut self) -> Self {
        self.cooperative = true;
        self
    }
    pub fn is_cooperative(&self) -> bool {
        self.cooperative
    }
    pub fn is_reactive(&self) -> bool {
        self.reactive
    }
    pub fn with_reactive(mut self, reactive: bool) -> Self {
        self.reactive = reactive;
        self
    }
    pub fn client(&self) -> &str {
        &self.client
    }
    /// Repeat is a typed live-session intent, never parsed as fresh source or replayed at restore.
    pub fn with_repeat(
        mut self,
        origin: String,
        acknowledge_effects: bool,
    ) -> Result<Self, SourceError> {
        if origin.trim().is_empty()
            || origin.len() > MAX_CELL_BYTES
            || origin.chars().any(char::is_control)
        {
            return Err(SourceError::Identity);
        }
        if self.revision_of.is_some() {
            return Err(SourceError::Identity);
        }
        self.repeat = Some(Repeat {
            origin,
            acknowledge_effects,
            from: None,
        });
        Ok(self)
    }
    pub fn with_repeat_from(mut self, from: Option<NodeId>) -> Result<Self, SourceError> {
        let repeat = self.repeat.as_mut().ok_or(SourceError::Identity)?;
        repeat.from = from;
        Ok(self)
    }
    pub fn with_revision(mut self, origin: String) -> Result<Self, SourceError> {
        if self.repeat.is_some()
            || origin == self.cell
            || origin.trim().is_empty()
            || origin.len() > MAX_CELL_BYTES
            || origin.chars().any(char::is_control)
        {
            return Err(SourceError::Identity);
        }
        self.revision_of = Some(origin);
        Ok(self)
    }
    pub fn revision_of(&self) -> Option<&str> {
        self.revision_of.as_deref()
    }
    pub fn parent(&self) -> Option<&str> {
        self.revision_of()
            .or_else(|| self.repeat().map(|r| r.origin.as_str()))
    }
    pub fn repeat(&self) -> Option<&Repeat> {
        self.repeat.as_ref()
    }
    pub fn environments(&self) -> Option<&wes_core::environments::EnvironmentContext> {
        self.environments.as_ref()
    }
    pub(crate) fn without_environments(mut self) -> Self {
        self.environments = None;
        self
    }
    pub(crate) fn same_request(&self, other: &Self) -> bool {
        self.cooperative == other.cooperative
            && self.reactive == other.reactive
            && self.client == other.client
            && self.text == other.text
            && self.source_name == other.source_name
            && self.source_start == other.source_start
            && self.document == other.document
            && self.environments == other.environments
            && self.repeat == other.repeat
            && self.revision_of == other.revision_of
    }
    pub(crate) fn context_charge(&self) -> usize {
        self.environments
            .as_ref()
            .map_or(0, |c| 2048 + c.revisions.len() * 1024)
            + self.document.as_ref().map_or(0, |s| s.len() * 16)
            + self.source_name.len() * 6
    }
    pub fn cell(&self) -> &str {
        &self.cell
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}
impl std::fmt::Debug for SourceInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceInput")
            .field("bytes", &self.text.len())
            .finish_non_exhaustive()
    }
}
#[derive(Debug, Error)]
pub enum SourceError {
    #[error("recorded source no longer reconstructs its exact accepted declarations")]
    ReplayMismatch,
    #[error("recorded source was rejected during reconstruction ({})", replay_diagnostic_codes(.0))]
    ReplayRejected(Box<SourceDiagnostics>),
    #[error("the submission identity is invalid or exceeds its byte limit")]
    Identity,
    #[error("the submission exceeds its source, statement or diagnostic limit")]
    Capacity,
    #[error("submission preparation was cancelled")]
    Cancelled,
    #[error("submission preparation terminated unexpectedly")]
    Worker,
    #[error(transparent)]
    Workspace(#[from] WorkspaceError),
}
#[derive(Clone, Debug)]
pub struct StatementIssues {
    pub span: Span,
    pub issues: Vec<ValidationIssue>,
}
#[derive(Clone, Debug, Default)]
pub struct SourceDiagnostics {
    pub diagnostics: Vec<Diagnostic>,
    pub issues: Vec<StatementIssues>,
}
impl SourceDiagnostics {
    pub(crate) fn check_limit(&self) -> Result<(), SourceError> {
        let mut entries = self.diagnostics.len();
        let mut bytes = 0usize;
        let mut charge = |text: &str| -> Result<(), SourceError> {
            bytes = bytes.checked_add(text.len()).ok_or(SourceError::Capacity)?;
            if bytes > 4 * 1024 * 1024 {
                return Err(SourceError::Capacity);
            }
            Ok(())
        };
        for diagnostic in &self.diagnostics {
            if !diagnostic.valid_public_message() {
                return Err(SourceError::Capacity);
            }
            charge(&diagnostic.code)?;
            charge(&diagnostic.message)?;
            if let Some(message) = &diagnostic.public_message {
                charge(message)?;
            }
            for hint in &diagnostic.hints {
                charge(hint)?;
            }
        }
        for statement in &self.issues {
            entries = entries
                .checked_add(statement.issues.len())
                .ok_or(SourceError::Capacity)?;
            for issue in &statement.issues {
                charge(&issue.code)?;
                charge(&issue.message)?;
                charge(&issue.path)?;
            }
        }
        if entries > 10_000 {
            return Err(SourceError::Capacity);
        }
        Ok(())
    }
}

/// Non-recorded single actions must be dispatched by the live coordinator. Syntax/mixed-action
/// rejection is distinct from semantic partial acceptance and cannot carry a committable batch.
pub enum SourcePreparation<T = PreparedSource> {
    Declarations(T),
    Immediate {
        input: SourceInput,
        statement: Box<Statement>,
    },
    Rejected {
        input: SourceInput,
        diagnostics: SourceDiagnostics,
    },
}

pub struct PreparedSource {
    input: SourceInput,
    batch: PreparedBatch,
    record: Option<CommandRecord>,
    accepted: Vec<Span>,
    diagnostics: SourceDiagnostics,
}
/// Exact historical declarations, not fresh submission authority. There is no live commit method.
pub struct PreparedReplay {
    pub(crate) batch: PreparedBatch,
}
impl PreparedReplay {
    pub fn nodes(&self) -> impl Iterator<Item = &NodeId> {
        self.batch.nodes()
    }
}
/// Strict reconstruction: no live type reader, no semantic partial acceptance and no ID fallback.
/// The result can only be installed by the held reconstruction boundary.
pub async fn prepare_replay(
    command: &CommandRecord,
    draft: DeclarationDraft,
    cancellation: CancellationToken,
) -> Result<PreparedReplay, SourceError> {
    command
        .validate()
        .map_err(|_| SourceError::ReplayMismatch)?;
    let has_calc = wes_language::lex(&SourceText::new("<replay>", &command.replay))
        .tokens
        .iter()
        .any(|token| token.kind == wes_language::TokenKind::Calc);
    if has_calc && command.calculation_package.is_none() {
        return Err(SourceError::ReplayMismatch);
    }
    let draft = draft.with_environment_context(command.environments.clone(), true);
    let draft = if let Some(package) = &command.calculation_package {
        draft.with_calculation_package(std::sync::Arc::new(
            wes_language::calc::Package::load(package).map_err(|_| SourceError::ReplayMismatch)?,
        ))
    } else {
        draft
    };
    let imports = ImportCapture::replay(draft.importers().clone(), command.imports.clone())
        .map_err(|_| SourceError::ReplayMismatch)?;
    let draft = draft.with_recorded_ids(&command.nodes)?;
    let types = TypeSourceCapture::replay(command.type_sources.clone())
        .map_err(|_| SourceError::ReplayMismatch)?;
    let input = SourceInput::new(command.cell.clone(), command.replay.clone())?
        .with_source_name(command.source_name.clone())?
        .with_source_start(command.source_start)?
        .with_document(command.document.clone())?;
    let source = match prepare_with_imports(input, draft, types, imports, cancellation).await? {
        SourcePreparation::Declarations(source) => source,
        SourcePreparation::Rejected { diagnostics, .. } => {
            return Err(SourceError::ReplayRejected(Box::new(diagnostics)));
        }
        SourcePreparation::Immediate { .. } => return Err(SourceError::ReplayMismatch),
    };
    if source
        .diagnostics
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        return Err(SourceError::ReplayRejected(Box::new(source.diagnostics)));
    }
    if source.nodes().ne(command.nodes.iter())
        || source
            .batch
            .changed_nodes()
            .collect::<indexmap::IndexSet<_>>()
            .into_iter()
            .ne(command.changed_nodes.iter())
    {
        return Err(SourceError::ReplayMismatch);
    }
    Ok(PreparedReplay {
        batch: source.batch,
    })
}
impl PreparedSource {
    pub fn input(&self) -> &SourceInput {
        &self.input
    }
    pub(crate) fn changed_nodes(&self) -> Vec<NodeId> {
        self.batch.changed_nodes().cloned().collect()
    }
    pub(crate) fn written_names(&self) -> Vec<String> {
        self.batch.written_names()
    }
    pub fn record(&self) -> Option<&CommandRecord> {
        self.record.as_ref()
    }
    pub fn accepted(&self) -> &[Span] {
        &self.accepted
    }
    pub fn diagnostics(&self) -> &SourceDiagnostics {
        &self.diagnostics
    }
    pub fn nodes(&self) -> impl Iterator<Item = &NodeId> {
        self.batch.nodes()
    }
    pub fn removed(&self) -> &[NodeId] {
        self.batch.removed()
    }
    pub fn unbound(&self) -> &[String] {
        self.batch.unbound()
    }
    pub fn with_admission(mut self, admission: AdmittedCommand) -> Result<Self, WorkspaceError> {
        if admission.cell() != self.input.cell() {
            return Err(WorkspaceError::AdmissionMismatch);
        }
        self.batch = self.batch.with_admission(admission)?;
        Ok(self)
    }
    /// A recorded session obtains the source record's acknowledged receipt first. Consume the
    /// returned effects after the whole commit; provider execution independently checks admission.
    pub(crate) fn receipt_charge(&self) -> usize {
        self.batch.receipt_charge()
    }
    pub(crate) fn success_diagnostics(&self) -> impl Iterator<Item = &Diagnostic> {
        self.batch.success_diagnostics()
    }
    pub fn commit(
        self,
        workspace: &mut Workspace,
        now: std::time::Duration,
    ) -> Result<BatchApplied, WorkspaceError> {
        if workspace
            .safe_observation
            .is_some_and(|kind| !self.batch.safe_observation(kind))
        {
            return Err(WorkspaceError::UnsafeObservation);
        }
        workspace.commit_batch(self.batch, now)
    }
}

enum PreparedStep {
    Declaration(PreparedChange),
    Control(PreparedControl),
}
impl PreparedStep {
    fn diagnostics(&self) -> &[Diagnostic] {
        match self {
            Self::Declaration(change) => change.diagnostics(),
            Self::Control(control) => control.diagnostics(),
        }
    }
    fn stage(self, draft: &mut DeclarationDraft) -> Result<(), WorkspaceError> {
        match self {
            Self::Declaration(change) => draft.stage(change),
            Self::Control(control) => draft.stage_control(control),
        }
    }
}

/// Syntax-checked source with an immutable AST. It does not hold a live workspace or execute work.
pub struct ParsedSource {
    pub(crate) apply_import: Option<crate::imports::ImportRequest>,
    input: SourceInput,
    parsed: wes_language::Parsed,
}
impl ParsedSource {
    pub(crate) fn statements(&self) -> &[Statement] {
        &self.parsed.script.statements
    }
    pub fn input(&self) -> &SourceInput {
        &self.input
    }
}
/// Pure syntax and whole-source mode classification. Call from a bounded analysis worker.
pub fn preflight(input: SourceInput) -> Result<SourcePreparation<ParsedSource>, SourceError> {
    preflight_with_package(input, wes_language::calc::Package::standard())
}
fn preflight_with_package(
    input: SourceInput,
    package: std::sync::Arc<wes_language::calc::Package>,
) -> Result<SourcePreparation<ParsedSource>, SourceError> {
    let mut parsed = wes_language::parse_with_calculation(
        &SourceText::new(input.source_name(), input.text())
            .with_start(input.source_start())
            .map_err(|_| SourceError::Identity)?,
        package,
    );
    if let Some(warning) = credential_literal_warning(input.text()) {
        parsed.diagnostics.push(warning);
    }
    if parsed
        .diagnostics
        .iter()
        .any(|d| d.severity == Severity::Error)
    {
        let diagnostics = SourceDiagnostics {
            diagnostics: parsed.diagnostics,
            issues: vec![],
        };
        diagnostics.check_limit()?;
        return Ok(SourcePreparation::Rejected { input, diagnostics });
    }
    if input.is_cooperative()
        && let Err(message) = crate::access::admit(&parsed)
    {
        return Ok(SourcePreparation::Rejected {
            input,
            diagnostics: SourceDiagnostics {
                diagnostics: vec![
                    Diagnostic::error("AUT001", parsed.script.span, message)
                        .with_public_message(message),
                ],
                issues: vec![],
            },
        });
    }
    if let Some(document) = input.document() {
        let valid = attach_document(&mut parsed, document);
        if !valid {
            return Ok(SourcePreparation::Rejected {
                input,
                diagnostics: SourceDiagnostics {
                    diagnostics: vec![Diagnostic::error(
                        "SRC010",
                        Span::at(0),
                        "document input requires exactly one :env plan or :package load with one empty quoted source and no file/path",
                    )],
                    issues: vec![],
                },
            });
        }
    }
    if parsed
        .script
        .statements
        .iter()
        .map(Statement::stage_count)
        .sum::<usize>()
        > max_statements()
    {
        return Err(SourceError::Capacity);
    }
    if input.revision_of().is_some()
        && (parsed.script.statements.len() != 1
            || matches!(
                &parsed.script.statements[0].expression,
                Expression::Definition(_) | Expression::Reference(_)
            )
            || matches!(&parsed.script.statements[0].expression, Expression::Call(call) if call.marker.is_some()))
    {
        return Ok(SourcePreparation::Rejected {
            input,
            diagnostics: SourceDiagnostics {
                diagnostics: vec![Diagnostic::error(
                    "REV001",
                    parsed.script.span,
                    "A definition revision must contain one command, calculation or pipeline, without workspace controls.",
                )],
                issues: vec![],
            },
        });
    }
    let recorded = parsed.script.statements.iter().all(worth_recording);
    if parsed.script.statements.len() > 1 && !recorded {
        return Ok(SourcePreparation::Rejected {
            input,
            diagnostics: SourceDiagnostics {
                diagnostics: vec![Diagnostic::error(
                    "ENG005",
                    parsed.script.span,
                    "submit wait, refresh, cancel, save and load separately from multi-statement declarations or calls",
                )],
                issues: vec![],
            },
        });
    }
    if !recorded {
        return Ok(SourcePreparation::Immediate {
            input,
            statement: parsed
                .script
                .statements
                .into_iter()
                .next()
                .map(Box::new)
                .expect("non-recorded script is one action"),
        });
    }

    Ok(SourcePreparation::Declarations(ParsedSource {
        input,
        parsed,
        apply_import: None,
    }))
}
/// Prepare finite calls, aliases, templates, type loads/checks and queries in source order. Other
/// recorded meta operations without a handler are rejected, never accepted as no-ops.
/// Retain this future through disconnect/shutdown: cancellation joins entered preparation
/// and file workers before discarding the uncommitted draft.
pub async fn prepare_declarations(
    input: SourceInput,
    draft: DeclarationDraft,
    types: TypeSourceCapture,
    cancellation: CancellationToken,
) -> Result<SourcePreparation, SourceError> {
    let draft = draft.with_environment_context(input.environments().cloned(), false);
    let imports = ImportCapture::live(draft.importers().clone());
    prepare_with_imports(input, draft, types, imports, cancellation).await
}
async fn prepare_with_imports(
    input: SourceInput,
    draft: DeclarationDraft,
    types: TypeSourceCapture,
    imports: ImportCapture,
    cancellation: CancellationToken,
) -> Result<SourcePreparation, SourceError> {
    let package = draft.calculation_package();
    let parsed = joined(&cancellation, move || {
        preflight_with_package(input, package)
    })
    .await??;
    match parsed {
        SourcePreparation::Declarations(parsed) => {
            prepare_parsed_with_imports(parsed, draft, types, imports, cancellation)
                .await
                .map(SourcePreparation::Declarations)
        }
        SourcePreparation::Immediate { input, statement } => {
            Ok(SourcePreparation::Immediate { input, statement })
        }
        SourcePreparation::Rejected { input, diagnostics } => {
            Ok(SourcePreparation::Rejected { input, diagnostics })
        }
    }
}
/// Continue a previously checked source without parsing it twice. Structural admissions remain
/// serialized by the owner; the declaration-only snapshot is not execution authority.
pub async fn prepare_parsed(
    source: ParsedSource,
    draft: DeclarationDraft,
    types: TypeSourceCapture,
    cancellation: CancellationToken,
) -> Result<PreparedSource, SourceError> {
    let imports = ImportCapture::live(draft.importers().clone());
    prepare_parsed_with_imports(source, draft, types, imports, cancellation).await
}
async fn prepare_parsed_with_imports(
    source: ParsedSource,
    mut draft: DeclarationDraft,
    mut types: TypeSourceCapture,
    mut imports: ImportCapture,
    cancellation: CancellationToken,
) -> Result<PreparedSource, SourceError> {
    check_cancel(&cancellation)?;
    let ParsedSource {
        input,
        parsed,
        mut apply_import,
    } = source;
    let mut accepted = vec![];
    let mut diagnostics = SourceDiagnostics {
        diagnostics: parsed.diagnostics,
        issues: vec![],
    };
    for statement in parsed.script.statements {
        let span = statement.span;
        if let Expression::Pipeline(stages) = statement.expression {
            let (returned, result) = joined(&cancellation, move || {
                let result = draft.stage_pipeline(&stages);
                (draft, result)
            })
            .await?;
            draft = returned;
            match result {
                Ok(warnings) => {
                    accepted.push(span);
                    diagnostics.diagnostics.extend(warnings);
                }
                Err(WorkspaceError::Rejected {
                    diagnostics: errors,
                    issues,
                }) => {
                    diagnostics.diagnostics.extend(errors);
                    if !issues.is_empty() {
                        diagnostics.issues.push(StatementIssues { span, issues });
                    }
                }
                Err(WorkspaceError::Cancelled) => return Err(SourceError::Cancelled),
                Err(error) => return Err(error.into()),
            }
            diagnostics.check_limit()?;
            continue;
        }
        let applied = matches!(&statement.expression, Expression::Call(call) if wes_language::vocabulary::commands::invocation(call).is_ok_and(|i|i.spec.command==MetaCommand::ImportApply));
        let authorized_apply = apply_import.is_some() || imports.replay_applied_request().is_ok();
        let (returned, prepared) = joined(&cancellation, move || {
            let result = if applied {
                if authorized_apply {draft.prepare_import_apply(&statement)} else {Err(WorkspaceError::from(Diagnostic::error("IMP001",span,"Import apply requires live owner admission or exact recorded Applied evidence")))}
            } else {draft.prepare(&statement)};
            (draft, result)
        })
        .await?;
        draft = returned;
        let mut package = None;
        let mut imported = None;
        let prepared = match prepared {
            Ok(Preparation::Change(change)) => Ok(PreparedStep::Declaration(change)),
            Ok(Preparation::Meta(meta)) => {
                if meta.command() == MetaCommand::Type && meta.is_type_load() {
                    let captured = match meta.type_source() {
                        Ok(crate::type_sources::TypeInput::File(path)) => {
                            types.read_package(&path, cancellation.clone()).await
                        }
                        Ok(crate::type_sources::TypeInput::Text { source, origin }) => {
                            types.text(&origin, &source, cancellation.clone()).await
                        }
                        Err(error) => Err(error),
                    };
                    match captured {
                        Ok(captured) => {
                            let (returned, prepared, captured) = joined(&cancellation, move || {
                                let result = draft.prepare_type_load(meta, &captured);
                                (draft, result, captured)
                            })
                            .await?;
                            draft = returned;
                            package = Some(captured);
                            prepared.map(PreparedStep::Declaration)
                        }
                        Err(TypeSourceError::Cancelled) => return Err(SourceError::Cancelled),
                        Err(error) => {
                            let mut errors = meta.diagnostics().to_vec();
                            errors.push(Diagnostic::error("TYP007", span, error.to_string()));
                            Err(WorkspaceError::Rejected {
                                diagnostics: errors,
                                issues: vec![],
                            })
                        }
                    }
                } else if matches!(
                    meta.command(),
                    MetaCommand::Import | MetaCommand::ImportApply
                ) {
                    let applied = meta.command() == MetaCommand::ImportApply;
                    let request = if applied {
                        apply_import.take().map(Ok).unwrap_or_else(|| {
                            imports
                                .replay_applied_request()
                                .map_err(|error| WorkspaceError::from(error.diagnostic(span)))
                        })
                    } else {
                        meta.import_request()
                    };
                    match request {
                        Err(error) => Err(error),
                        Ok(request) => match if applied {
                            imports.read_applied(&request, cancellation.clone()).await
                        } else {
                            imports.read(&request, cancellation.clone()).await
                        } {
                            Ok(captured) => {
                                let (returned, prepared, captured) =
                                    joined(&cancellation, move || {
                                        let result = meta.import_step(request).and_then(|step| {
                                            draft.prepare_import_step(step, &captured)
                                        });
                                        (draft, result, captured)
                                    })
                                    .await?;
                                draft = returned;
                                imported = Some(captured);
                                prepared.map(PreparedStep::Declaration)
                            }
                            Err(ImportError::Cancelled) => return Err(SourceError::Cancelled),
                            Err(error) => {
                                let mut errors = meta.diagnostics().to_vec();
                                errors.push(error.diagnostic(span));
                                Err(WorkspaceError::Rejected {
                                    diagnostics: errors,
                                    issues: vec![],
                                })
                            }
                        },
                    }
                } else if matches!(
                    meta.command(),
                    MetaCommand::Change
                        | MetaCommand::Policy
                        | MetaCommand::Timeout
                        | MetaCommand::Drop
                ) {
                    let (returned, prepared) = joined(&cancellation, move || {
                        let result = draft.prepare_control(meta);
                        (draft, result)
                    })
                    .await?;
                    draft = returned;
                    prepared.map(PreparedStep::Control)
                } else {
                    let mut errors = meta.diagnostics().to_vec();
                    errors.push(Diagnostic::error(
                        "ENG007",
                        span,
                        format!(
                            "':{}' is not yet supported by source declaration preparation",
                            meta.command().name()
                        ),
                    ));
                    Err(WorkspaceError::Rejected {
                        diagnostics: errors,
                        issues: vec![],
                    })
                }
            }
            Err(error) => Err(error),
        };
        match prepared {
            Ok(change) => {
                let warnings = change.diagnostics().to_vec();
                let (returned, staged) = joined(&cancellation, move || {
                    let result = change.stage(&mut draft);
                    (draft, result)
                })
                .await?;
                draft = returned;
                if let Err(WorkspaceError::Rejected {
                    diagnostics: errors,
                    issues,
                }) = staged
                {
                    diagnostics.diagnostics.extend(errors);
                    if !issues.is_empty() {
                        diagnostics.issues.push(StatementIssues { span, issues });
                    }
                    diagnostics.check_limit()?;
                    continue;
                } else {
                    staged?;
                }
                if let Some(package) = package {
                    types
                        .accept(&package)
                        .expect("package belongs to this capture");
                }
                if let Some(imported) = imported {
                    imports
                        .accept(&imported)
                        .expect("import belongs to this capture");
                }
                accepted.push(span);
                diagnostics.diagnostics.extend(warnings);
            }
            Err(WorkspaceError::Rejected {
                diagnostics: errors,
                issues,
            }) => {
                diagnostics.diagnostics.extend(errors);
                if !issues.is_empty() {
                    diagnostics.issues.push(StatementIssues { span, issues });
                }
            }
            Err(WorkspaceError::Cancelled) => return Err(SourceError::Cancelled),
            Err(error) => return Err(error.into()),
        }
        diagnostics.check_limit()?;
    }
    check_cancel(&cancellation)?;
    let policies = if input.reactive {
        draft.reactive_nodes()?
    } else {
        String::new()
    };
    let package = draft.calculation_package();
    let environment_context = draft.environment_context().cloned();
    let batch = draft.finish();
    if !diagnostics
        .diagnostics
        .iter()
        .any(|diagnostic| diagnostic.severity == Severity::Error)
    {
        imports
            .verify_complete()
            .map_err(|_| SourceError::ReplayMismatch)?;
    }
    let record = if batch.is_empty() {
        None
    } else {
        let mut replay = if diagnostics
            .diagnostics
            .iter()
            .any(|d| d.severity == Severity::Error)
        {
            accepted
                .iter()
                .map(|span| &input.text()[span.start()..span.end()])
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            input.text().to_owned()
        };
        replay.push_str(&policies);
        Some(CommandRecord {
            source_name: input.source_name().to_owned(),
            source_start: input.source_start(),
            changed_nodes: batch
                .changed_nodes()
                .cloned()
                .collect::<indexmap::IndexSet<_>>()
                .into_iter()
                .collect(),
            document: input.document().map(str::to_owned),
            revision_of: input.revision_of().map(str::to_owned),
            environments: environment_context,
            cell: input.cell().to_owned(),
            text: input.text().to_owned(),
            nodes: batch.nodes().cloned().collect(),
            calculation_package: batch
                .calculation_package_used()
                .then(|| package.source().to_owned()),
            type_sources: types.finish(),
            imports: imports.finish(),
            replay,
        })
    };
    Ok(PreparedSource {
        input,
        batch,
        record,
        accepted,
        diagnostics,
    })
}
fn attach_document(parsed: &mut wes_language::Parsed, document: &str) -> bool {
    let [statement] = parsed.script.statements.as_mut_slice() else {
        return false;
    };
    let Expression::Call(call) = &mut statement.expression else {
        return false;
    };
    let path: Vec<_> = call.path.iter().map(|p| p.text.as_str()).collect();
    if call.marker.is_none()
        || !matches!(path.as_slice(), ["env", "plan"] | ["package", "load"])
        || !call.operands.is_empty()
        || !statement.annotations.is_empty()
        || statement.error_binding.is_some()
        || call
            .arguments
            .iter()
            .any(|a| matches!(a.key.text.as_str(), "file" | "path"))
        || call
            .arguments
            .iter()
            .filter(|a| a.key.text == "source")
            .count()
            != 1
    {
        return false;
    }
    let Some(arg) = call.arguments.iter_mut().find(|a| a.key.text == "source") else {
        return false;
    };
    let wes_language::Value::Text(value) = &mut arg.value else {
        return false;
    };
    if !value.text.is_empty() {
        return false;
    }
    value.text = document.to_owned();
    true
}
fn worth_recording(statement: &Statement) -> bool {
    let Expression::Call(call) = &statement.expression else {
        return true;
    };
    wes_language::vocabulary::commands::invocation(call)
        .map_or(true, |invocation| invocation.spec.recorded)
}
fn check_cancel(token: &CancellationToken) -> Result<(), SourceError> {
    if token.is_cancelled() {
        Err(SourceError::Cancelled)
    } else {
        Ok(())
    }
}
async fn joined<T: Send + 'static>(
    token: &CancellationToken,
    work: impl FnOnce() -> T + Send + 'static,
) -> Result<T, SourceError> {
    check_cancel(token)?;
    let worker_token = token.clone();
    let result = tokio::task::spawn_blocking(move || {
        check_cancel(&worker_token)?;
        Ok(work())
    })
    .await;
    check_cancel(token)?;
    result.map_err(|_| SourceError::Worker)?
}

fn replay_diagnostic_codes(diagnostics: &SourceDiagnostics) -> String {
    diagnostics
        .diagnostics
        .iter()
        .take(4)
        .map(|d| d.code.as_ref())
        .collect::<Vec<_>>()
        .join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn diagnostic_entries_hints_and_issue_text_share_explicit_limits() {
        let mut diagnostics = SourceDiagnostics::default();
        diagnostics.diagnostics.push(
            Diagnostic::error("test", Span::at(0), "m").with_hint("x".repeat(4 * 1024 * 1024)),
        );
        assert!(matches!(
            diagnostics.check_limit(),
            Err(SourceError::Capacity)
        ));
        diagnostics.diagnostics.clear();
        diagnostics.issues.push(StatementIssues {
            span: Span::at(0),
            issues: vec![ValidationIssue {
                path: String::new(),
                code: "test".into(),
                message: "x".repeat(4 * 1024 * 1024),
            }],
        });
        assert!(matches!(
            diagnostics.check_limit(),
            Err(SourceError::Capacity)
        ));
        diagnostics.issues.clear();
        diagnostics.diagnostics = vec![Diagnostic::error("test", Span::at(0), "m"); 10_000];
        diagnostics.check_limit().unwrap();
        diagnostics.issues.push(StatementIssues {
            span: Span::at(0),
            issues: vec![ValidationIssue {
                path: String::new(),
                code: "test".into(),
                message: String::new(),
            }],
        });
        assert!(matches!(
            diagnostics.check_limit(),
            Err(SourceError::Capacity)
        ));
    }
    #[test]
    fn public_diagnostic_text_is_admitted_and_charged_with_other_text() {
        for text in ["".into(), " ".into(), "ç".repeat(257)] {
            let diagnostics = SourceDiagnostics {
                diagnostics: vec![
                    Diagnostic::error("FUTURE", Span::at(0), "m").with_public_message(text),
                ],
                issues: vec![],
            };
            assert!(diagnostics.check_limit().is_err());
        }
        let diagnostic =
            Diagnostic::error("X", Span::at(0), "m").with_public_message("x".repeat(512));
        assert!(
            SourceDiagnostics {
                diagnostics: vec![diagnostic.clone()],
                issues: vec![]
            }
            .check_limit()
            .is_ok()
        );
        assert!(
            SourceDiagnostics {
                diagnostics: vec![diagnostic; 9000],
                issues: vec![]
            }
            .check_limit()
            .is_err()
        );
    }
    #[tokio::test]
    async fn joined_preparation_sanitizes_host_panic_payloads() {
        let result: Result<(), SourceError> =
            joined(&CancellationToken::new(), || panic!("private-source-data")).await;
        assert!(matches!(result, Err(SourceError::Worker)));
        assert!(
            !result
                .unwrap_err()
                .to_string()
                .contains("private-source-data")
        );
    }
}

/// Advisory only: source and ordinary values are still recorded exactly as submitted.
/// Run before the syntax-error return so failed commands receive the same warning.
pub(crate) fn credential_literal_warning(source: &str) -> Option<Diagnostic> {
    use std::sync::LazyLock;
    static CREDENTIAL: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(
        r#"(?ix)(?:[\"']\s*(?:bearer|basic)\s+[^\s\"']+|(?:[\"']?(?:proxy-)?authorization[\"']?)\s*[:=]\s*[\"'][^\"'\r\n]+|[\"']-----BEGIN\s+(?:RSA\s+|EC\s+|OPENSSH\s+)?PRIVATE\s+KEY-----)"#
    ).expect("constant credential advisory pattern")
    });
    CREDENTIAL.is_match(source).then(|| {
        Diagnostic::error("SEC001", Span::at(0),
            "Possible credential literal in command source. Commands, including rejected commands, are saved in workspace history; ordinary results may also be retained. This warning does not redact or prevent storage.")
            .with_severity(Severity::Warning)
            .with_hint("Use /env to supply named credentials, or --credentials-stdin with --grant-provider in the CLI. Do not put credential values in commands. Detection is limited to recognizable patterns and is not a secret scan.")
    })
}
