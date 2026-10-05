//! Explicit versioned JSONL record formats for current records.
//! This module encodes one record, without its line delimiter; it never reads/writes a file.
const JOURNAL_FORMAT: &str = "wes.journal";
const RECOVERY_FORMAT: &str = "wes.recovery";
const VERSION: u32 = 1;
use super::{CodecError, Limits, encode, raw};
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;
use std::{borrow::Cow, collections::BTreeMap};
use wes_core::{ErrorId, ErrorValue, Timestamp, ValidationIssue};
use wes_engine::{
    graph::{NodeId, NodeState},
    history::{
        CallRecord, CommandRecord, DiagnosticRecord, ExecutionRecord, JournalEntry, NoticeContext,
        NoticeRecord, RecoveryEntry, RetainedResult,
    },
    imports::{ImportOrigin, ImportRecipe, ImportRequest, ImportSnapshot},
    runtime::RunId,
    storage::ValueHandle,
};
use wes_language::{Diagnostic, Severity, SourceText, Span};

#[derive(Clone, Debug)]
pub struct DecodedRecord<T> {
    pub entry: T,
}

#[derive(Serialize)]
struct Envelope<T> {
    format: &'static str,
    version: u32,
    entry: T,
}

pub fn encode_journal(entry: &JournalEntry, limits: Limits) -> Result<Vec<u8>, CodecError> {
    // Conversion borrows all potentially large text. Auxiliary collections are bounded first.
    let entry = journal_wire(entry, limits)?;
    encode_record(JOURNAL_FORMAT, VERSION, entry, limits)
}
pub fn encode_recovery(entry: &RecoveryEntry, limits: Limits) -> Result<Vec<u8>, CodecError> {
    encode_record(RECOVERY_FORMAT, VERSION, recovery_wire(entry), limits)
}
fn encode_record(
    format: &'static str,
    version: u32,
    entry: impl Serialize,
    limits: Limits,
) -> Result<Vec<u8>, CodecError> {
    let bytes = encode::write(
        &Envelope {
            format,
            version,
            entry,
        },
        limits,
    )?;
    raw::checked_document(&bytes, limits)?;
    Ok(bytes)
}
pub fn decode_journal(
    bytes: &[u8],
    limits: Limits,
) -> Result<DecodedRecord<JournalEntry>, CodecError> {
    let record = record(bytes, JOURNAL_FORMAT, VERSION, limits)?;
    let fields = raw::Context::new(limits).object(record)?;
    // Internally tagged Serde enums buffer fields through Content, which cannot preserve
    // RawValue. Select this variant from the already checked object and deserialize its body
    // directly from JSON; the library still owns all grammar and nested value parsing.
    let wire: JournalWire<'_> = if raw::string(raw::required(&fields, "record")?)? == "command" {
        let command: CommandWire<'_> = serde_json::from_str(record.get())?;
        JournalWire::Command {
            source_name: command.source_name,
            source_start: command.source_start,
            document: command.document,
            revision_of: command.revision_of,
            environments: command.environments,
            cell: command.cell,
            text: command.text,
            nodes: command.nodes,
            changed_nodes: command.changed_nodes,
            type_sources: command.type_sources,
            calculation_package: command.calculation_package,
            imports: command.imports,
            replay: command.replay,
        }
    } else {
        serde_json::from_str(record.get())?
    };
    let entry = journal_entry(wire, limits)?;
    Ok(DecodedRecord { entry })
}
pub fn decode_recovery(
    bytes: &[u8],
    limits: Limits,
) -> Result<DecodedRecord<RecoveryEntry>, CodecError> {
    let record = record(bytes, RECOVERY_FORMAT, VERSION, limits)?;
    recovery_entry(serde_json::from_str(record.get())?).map(|entry| DecodedRecord { entry })
}

fn record<'a>(
    bytes: &'a [u8],
    format: &str,
    expected_version: u32,
    limits: Limits,
) -> Result<&'a RawValue, CodecError> {
    let root = raw::checked_document(bytes, limits)?;
    let mut context = raw::Context::new(limits);
    let object = context.object(root)?;
    if let Some(written_format) = object.get("format") {
        let version = raw::scalar::<u32>(raw::required(&object, "version")?)?;
        if raw::string(written_format)? != format || version != expected_version {
            return Err(invalid("unsupported history format or version"));
        }
        if object.len() != 3 {
            return Err(invalid("unexpected history envelope fields"));
        }
        Ok(raw::required(&object, "entry")?)
    } else {
        Err(invalid("history envelope is required"))
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase", deny_unknown_fields)]
enum JournalWire<'a> {
    Views {
        id: Cow<'a, str>,
        value: String,
    },
    Snapshot {
        source: Cow<'a, str>,
        epoch: Cow<'a, str>,
        delivery: Option<u64>,
        observation: ExecutionWire<'a>,
        handle: Option<Cow<'a, str>>,
        retention: Option<Cow<'a, str>>,
    },
    Requested {
        namespace: Cow<'a, str>,
        request: Cow<'a, str>,
        cell: Cow<'a, str>,
        fingerprint: Cow<'a, str>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        steps: Vec<Cow<'a, str>>,
    },
    ProtectedRun {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        handle: Cow<'a, str>,
    },
    Payload {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        handle: Cow<'a, str>,
    },
    Retired {
        nodes: Vec<Cow<'a, str>>,
        payloads: Vec<Cow<'a, str>>,
        protected: Vec<Cow<'a, str>>,
    },
    Submitted {
        source_name: Cow<'a, str>,
        source_start: [usize; 2],
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        refreshed: Vec<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        document: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision_of: Option<Cow<'a, str>>,
        id: Cow<'a, str>,
        cell: Cow<'a, str>,
        text: Cow<'a, str>,
        client: Cow<'a, str>,
        context: Option<EnvironmentContextWire<'a>>,
        order: u64,
        nodes: Vec<Cow<'a, str>>,
        origin: Option<Cow<'a, str>>,
        acknowledge_effects: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Cow<'a, str>>,
        run: Cow<'a, str>,
    },
    Trace {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        value: String,
    },
    Environments {
        id: Cow<'a, str>,
        yaml: Cow<'a, str>,
        sources: Vec<EnvironmentSourceWire<'a>>,
        before: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
        after: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
    },
    Notice {
        value: NoticeWire<'a>,
    },
    #[serde(skip_deserializing)]
    Command {
        source_name: Cow<'a, str>,
        source_start: [usize; 2],
        #[serde(default, skip_serializing_if = "Option::is_none")]
        document: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        revision_of: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        environments: Option<EnvironmentContextWire<'a>>,
        cell: Cow<'a, str>,
        text: Cow<'a, str>,
        nodes: Vec<Cow<'a, str>>,
        #[serde(rename = "changedNodes")]
        changed_nodes: Vec<Cow<'a, str>>,
        #[serde(default, rename = "typeSources")]
        type_sources: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
        #[serde(
            default,
            rename = "calculationPackage",
            skip_serializing_if = "Option::is_none"
        )]
        calculation_package: Option<Cow<'a, str>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        imports: Option<Vec<ImportWire<'a>>>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        replay: Option<Cow<'a, str>>,
    },
    Result {
        node: Cow<'a, str>,
        handle: Cow<'a, str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        retention: Option<Cow<'a, str>>,
        run: Cow<'a, str>,
    },
    Observation {
        value: ExecutionWire<'a>,
    },
    Diagnostic {
        value: DiagnosticWire<'a>,
    },
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentSourceWire<'a> {
    kind: Cow<'a, str>,
    location: Cow<'a, str>,
    format: Cow<'a, str>,
    source: Cow<'a, str>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CommandWire<'a> {
    source_name: Cow<'a, str>,
    source_start: [usize; 2],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    document: Option<Cow<'a, str>>,
    #[serde(default)]
    revision_of: Option<Cow<'a, str>>,
    #[serde(default)]
    environments: Option<EnvironmentContextWire<'a>>,
    #[serde(rename = "record")]
    _record: CommandTag,
    cell: Cow<'a, str>,
    text: Cow<'a, str>,
    nodes: Vec<Cow<'a, str>>,
    #[serde(rename = "changedNodes")]
    changed_nodes: Vec<Cow<'a, str>>,
    #[serde(default, rename = "typeSources")]
    type_sources: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
    #[serde(default, rename = "calculationPackage")]
    calculation_package: Option<Cow<'a, str>>,
    #[serde(default)]
    imports: Option<Vec<ImportWire<'a>>>,
    #[serde(default)]
    replay: Option<Cow<'a, str>>,
}
#[derive(Deserialize)]
enum CommandTag {
    #[serde(rename = "command")]
    Command,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvironmentContextWire<'a> {
    selected: Option<Cow<'a, str>>,
    revisions: BTreeMap<Cow<'a, str>, Cow<'a, str>>,
}
fn environment_context_wire(
    context: &wes_core::environments::EnvironmentContext,
) -> EnvironmentContextWire<'_> {
    EnvironmentContextWire {
        selected: context.selected.as_deref().map(Into::into),
        revisions: context
            .revisions
            .iter()
            .map(|(n, r)| (n.as_str().into(), r.to_string().into()))
            .collect(),
    }
}
fn environment_context(
    context: EnvironmentContextWire<'_>,
) -> Result<wes_core::environments::EnvironmentContext, CodecError> {
    let context = wes_core::environments::EnvironmentContext {
        selected: context.selected.map(Cow::into_owned),
        revisions: context
            .revisions
            .into_iter()
            .map(|(n, r)| Ok((n.into_owned(), r.parse().map_err(invalid)?)))
            .collect::<Result<_, CodecError>>()?,
    };
    context.validate().map_err(invalid)?;
    Ok(context)
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ImportWire<'a> {
    #[serde(default, skip_serializing_if = "ImportOriginWire::is_literal")]
    origin: ImportOriginWire,
    kind: Cow<'a, str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    alias: Option<Cow<'a, str>>,
    arguments: BTreeMap<Cow<'a, str>, Box<RawValue>>,
    format: Cow<'a, str>,
    source: Cow<'a, str>,
}
#[derive(Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ImportOriginWire {
    #[default]
    Literal,
    Applied,
}
impl ImportOriginWire {
    fn is_literal(&self) -> bool {
        matches!(self, Self::Literal)
    }
}
fn import_wire(snapshot: &ImportSnapshot, limits: Limits) -> Result<ImportWire<'_>, CodecError> {
    Ok(ImportWire {
        origin: match snapshot.origin() {
            ImportOrigin::Literal => ImportOriginWire::Literal,
            ImportOrigin::Applied => ImportOriginWire::Applied,
        },
        kind: snapshot.request().kind().into(),
        alias: snapshot.request().alias().map(Into::into),
        arguments: snapshot
            .request()
            .arguments()
            .iter()
            .map(|(key, value)| {
                let bytes = super::encode_value(value, limits)?;
                let json = String::from_utf8(bytes)
                    .map_err(|_| invalid("invalid encoded import argument"))?;
                Ok((key.as_str().into(), RawValue::from_string(json)?))
            })
            .collect::<Result<_, CodecError>>()?,
        format: snapshot.recipe().format().into(),
        source: snapshot.recipe().source().into(),
    })
}
fn read_import(wire: ImportWire<'_>, limits: Limits) -> Result<ImportSnapshot, CodecError> {
    let argument_bytes = wire
        .arguments
        .iter()
        .try_fold(0usize, |bytes, (key, value)| {
            bytes.checked_add(key.len())?.checked_add(value.get().len())
        })
        .ok_or(CodecError::Bytes)?;
    if wire.arguments.len() > 256
        || argument_bytes > 128 * 1024
        || wire.source.len() > wes_engine::imports::max_recipe_bytes()
    {
        return Err(invalid("captured import exceeds its input budget"));
    }
    let arguments = wire
        .arguments
        .into_iter()
        .map(|(key, raw)| {
            let decoded = super::decode_value(raw.get().as_bytes(), limits)?;
            Ok((key.into_owned(), decoded.value))
        })
        .collect::<Result<_, CodecError>>()?;
    let request = ImportRequest::new(
        wire.kind.into_owned(),
        wire.alias.map(Cow::into_owned),
        arguments,
    )
    .map_err(invalid)?;
    let recipe =
        ImportRecipe::new(wire.format.into_owned(), wire.source.into_owned()).map_err(invalid)?;
    Ok(ImportSnapshot::from_origin(
        request,
        recipe,
        match wire.origin {
            ImportOriginWire::Literal => ImportOrigin::Literal,
            ImportOriginWire::Applied => ImportOrigin::Applied,
        },
    ))
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct NoticeWire<'a> {
    id: Cow<'a, str>,
    at: Cow<'a, str>,
    context: NoticeContextWire<'a>,
    error: ErrorWire<'a>,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum NoticeContextWire<'a> {
    Execution {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
    },
    Publication {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        handle: Option<Cow<'a, str>>,
    },
    Keep {
        handle: Cow<'a, str>,
        #[serde(rename = "mayHaveApplied")]
        may_have_applied: bool,
    },
    Release {
        handle: Cow<'a, str>,
        #[serde(rename = "mayHaveApplied")]
        may_have_applied: bool,
    },
    Eviction {},
    StorageWorker {},
    WorkspaceShutdown {},
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ExecutionWire<'a> {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    stale_reason: Option<Cow<'a, str>>,
    id: Cow<'a, str>,
    node: Cow<'a, str>,
    run: Cow<'a, str>,
    at: Cow<'a, str>,
    state: StateWire,
    error: ErrorSlot<'a>,
}
#[derive(Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
enum StateWire {
    Pending,
    Running,
    Ready,
    Stale,
    Failed,
    Cancelled,
    Skipped,
}
#[derive(Serialize, Deserialize)]
#[serde(untagged)]
enum ErrorSlot<'a> {
    Present(ErrorWire<'a>),
    Absent([u8; 0]),
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorWire<'a> {
    locations: Vec<LocationWire>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    policy: Option<ErrorPolicy>,
    id: Cow<'a, str>,
    code: Cow<'a, str>,
    message: Cow<'a, str>,
    issues: Vec<IssueWire<'a>>,
    #[serde(rename = "causeId")]
    cause: Cow<'a, str>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct LocationWire {
    source: String,
    start: usize,
    end: usize,
    line: usize,
    column: usize,
    end_line: usize,
    end_column: usize,
}
impl From<&wes_core::SourceLocation> for LocationWire {
    fn from(p: &wes_core::SourceLocation) -> Self {
        Self {
            source: p.source.clone(),
            start: p.start,
            end: p.end,
            line: p.line,
            column: p.column,
            end_line: p.end_line,
            end_column: p.end_column,
        }
    }
}
impl From<LocationWire> for wes_core::SourceLocation {
    fn from(p: LocationWire) -> Self {
        Self {
            source: p.source,
            start: p.start,
            end: p.end,
            line: p.line,
            column: p.column,
            end_line: p.end_line,
            end_column: p.end_column,
        }
    }
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorPolicy {
    origins: Vec<String>,
    private: bool,
    unknown: bool,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IssueWire<'a> {
    path: Cow<'a, str>,
    code: Cow<'a, str>,
    message: Cow<'a, str>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticWire<'a> {
    id: Cow<'a, str>,
    at: Cow<'a, str>,
    cell: Cow<'a, str>,
    source: Cow<'a, str>,
    diagnostic: DiagnosticDetail<'a>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct DiagnosticDetail<'a> {
    code: Cow<'a, str>,
    severity: SeverityWire,
    message: Cow<'a, str>,
    start: usize,
    end: usize,
    #[serde(default)]
    hints: Vec<Cow<'a, str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    public_message: Option<Cow<'a, str>>,
}
#[derive(Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
enum SeverityWire {
    Error,
    Warning,
    Info,
}
#[derive(Serialize, Deserialize)]
#[serde(tag = "record", rename_all = "lowercase", deny_unknown_fields)]
enum RecoveryWire<'a> {
    Accepted {
        cell: Cow<'a, str>,
    },
    Calling {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        #[serde(default)]
        cell: Cow<'a, str>,
        capability: Cow<'a, str>,
        safe: bool,
        at: Cow<'a, str>,
    },
    Called {
        node: Cow<'a, str>,
        run: Cow<'a, str>,
        produced: bool,
    },
}

fn journal_wire(entry: &JournalEntry, limits: Limits) -> Result<JournalWire<'_>, CodecError> {
    Ok(match entry {
        JournalEntry::ProtectedRun { node, run, handle } => JournalWire::ProtectedRun {
            node: node.as_str().into(),
            run: run.as_str().into(),
            handle: handle.as_str().into(),
        },
        JournalEntry::Payload { node, run, handle } => JournalWire::Payload {
            node: node.as_str().into(),
            run: run.as_str().into(),
            handle: handle.as_str().into(),
        },
        JournalEntry::Retired(r) => {
            r.validate().map_err(invalid)?;
            JournalWire::Retired {
                nodes: r.nodes.iter().map(|n| n.as_str().into()).collect(),
                payloads: r.payloads.iter().map(|h| h.as_str().into()).collect(),
                protected: r.protected.iter().map(|h| h.as_str().into()).collect(),
            }
        }
        JournalEntry::Requested(r) => {
            r.validate().map_err(invalid)?;
            JournalWire::Requested {
                namespace: r.namespace.as_str().into(),
                request: r.request.as_str().into(),
                cell: r.cell.as_str().into(),
                fingerprint: r.fingerprint.as_str().into(),
                steps: r.steps.iter().map(|s| s.as_str().into()).collect(),
            }
        }
        JournalEntry::Submitted(s) => {
            s.input().map_err(invalid)?;
            JournalWire::Submitted {
                source_name: s.source_name.as_str().into(),
                source_start: [s.source_start.line, s.source_start.column],
                refreshed: s.refreshed.iter().map(|n| n.as_str().into()).collect(),
                document: s.document.as_deref().map(Cow::Borrowed),
                revision_of: s.revision_of.as_deref().map(Cow::Borrowed),
                id: s.id.as_str().into(),
                cell: s.cell.as_str().into(),
                text: s.text.as_str().into(),
                client: s.client.as_str().into(),
                context: s.context.as_ref().map(environment_context_wire),
                order: s.order,
                nodes: s.nodes.iter().map(|n| n.as_str().into()).collect(),
                origin: s.repeat.as_ref().map(|r| r.origin.as_str().into()),
                acknowledge_effects: s.repeat.as_ref().is_some_and(|r| r.acknowledge_effects),
                from: s
                    .repeat
                    .as_ref()
                    .and_then(|r| r.from.as_ref())
                    .map(|n| n.as_str().into()),
                run: run_text(s.run.as_ref()),
            }
        }
        JournalEntry::Views(record) => {
            record.validate().map_err(invalid)?;
            JournalWire::Views {
                id: record.id.as_str().into(),
                value: String::from_utf8(super::encode_value(&record.value, limits)?)
                    .map_err(invalid)?,
            }
        }
        JournalEntry::Trace(record) => {
            if !record.validate() {
                return Err(invalid("invalid public trace record"));
            }
            JournalWire::Trace {
                node: record.node.as_str().into(),
                run: record.run.as_str().into(),
                value: String::from_utf8(super::encode_value(&record.value, limits)?)
                    .map_err(invalid)?,
            }
        }
        JournalEntry::Environments(record) => JournalWire::Environments {
            id: record.id().into(),
            yaml: record.yaml().into(),
            sources: record
                .sources()
                .iter()
                .map(|(key, value)| EnvironmentSourceWire {
                    kind: key.kind().into(),
                    location: key.location().into(),
                    format: value.format().into(),
                    source: value.bytes().into(),
                })
                .collect(),
            before: record
                .before()
                .iter()
                .map(|(k, v)| (k.as_str().into(), v.to_string().into()))
                .collect(),
            after: record
                .after()
                .iter()
                .map(|(k, v)| (k.as_str().into(), v.to_string().into()))
                .collect(),
        },
        JournalEntry::Noticed(record) => JournalWire::Notice {
            value: NoticeWire {
                id: record.id().into(),
                at: record.at().to_string().into(),
                error: error_wire(record.error(), limits)?,
                context: match record.context() {
                    NoticeContext::Execution { node, run } => NoticeContextWire::Execution {
                        node: node.as_str().into(),
                        run: run.as_str().into(),
                    },
                    NoticeContext::Publication { node, run, handle } => {
                        NoticeContextWire::Publication {
                            node: node.as_str().into(),
                            run: run.as_str().into(),
                            handle: handle.as_ref().map(|handle| handle.as_str().into()),
                        }
                    }
                    NoticeContext::Keep {
                        handle,
                        may_have_applied,
                    } => NoticeContextWire::Keep {
                        handle: handle.as_str().into(),
                        may_have_applied: *may_have_applied,
                    },
                    NoticeContext::Release {
                        handle,
                        may_have_applied,
                    } => NoticeContextWire::Release {
                        handle: handle.as_str().into(),
                        may_have_applied: *may_have_applied,
                    },
                    NoticeContext::Eviction => NoticeContextWire::Eviction {},
                    NoticeContext::StorageWorker => NoticeContextWire::StorageWorker {},
                    NoticeContext::WorkspaceShutdown => NoticeContextWire::WorkspaceShutdown {},
                },
            },
        },
        JournalEntry::Command(command) => {
            if command
                .nodes
                .len()
                .saturating_add(command.type_sources.len())
                .saturating_add(command.changed_nodes.len())
                .saturating_add(command.imports.len())
                > limits.nodes
            {
                return Err(CodecError::Work);
            }
            command.validate().map_err(invalid)?;
            JournalWire::Command {
                source_name: (&command.source_name).into(),
                source_start: [command.source_start.line, command.source_start.column],
                document: command.document.as_deref().map(Cow::Borrowed),
                revision_of: command.revision_of.as_deref().map(Cow::Borrowed),
                environments: command.environments.as_ref().map(environment_context_wire),
                cell: (&command.cell).into(),
                text: (&command.text).into(),
                nodes: command
                    .nodes
                    .iter()
                    .map(|node| node.as_str().into())
                    .collect(),
                changed_nodes: command
                    .changed_nodes
                    .iter()
                    .map(|node| node.as_str().into())
                    .collect(),
                calculation_package: command.calculation_package.as_deref().map(Cow::Borrowed),
                type_sources: command
                    .type_sources
                    .iter()
                    .map(|(k, v)| (k.as_str().into(), v.as_str().into()))
                    .collect(),
                imports: if command.imports.is_empty() {
                    None
                } else {
                    Some(
                        command
                            .imports
                            .iter()
                            .map(|snapshot| import_wire(snapshot, limits))
                            .collect::<Result<_, _>>()?,
                    )
                },
                replay: (command.replay != command.text).then(|| command.replay.as_str().into()),
            }
        }
        JournalEntry::Snapshot(snapshot) => {
            snapshot.validate().map_err(invalid)?;
            JournalWire::Snapshot {
                source: snapshot.source.as_str().into(),
                epoch: snapshot.epoch.as_str().into(),
                delivery: snapshot.delivery,
                observation: execution_wire(&snapshot.observation, limits)?,
                handle: snapshot.result.as_ref().map(|r| r.handle.as_str().into()),
                retention: snapshot
                    .result
                    .as_ref()
                    .map(|r| r.retention.as_str().into()),
            }
        }
        JournalEntry::Result(result) => JournalWire::Result {
            retention: (result.retention != wes_engine::storage::Retention::Unknown)
                .then(|| result.retention.as_str().into()),
            node: result.node.as_str().into(),
            handle: result.handle.as_str().into(),
            run: result.run.as_str().into(),
        },
        JournalEntry::Observed(record) => JournalWire::Observation {
            value: ExecutionWire {
                stale_reason: record.stale_reason().map(|r| r.code().into()),
                id: record.id().into(),
                node: record.node().as_str().into(),
                run: run_text(record.run()),
                at: record.at().to_string().into(),
                state: state_wire(record.state()),
                error: match record.error() {
                    None => ErrorSlot::Absent([]),
                    Some(error) => ErrorSlot::Present(error_wire(error, limits)?),
                },
            },
        },
        JournalEntry::Diagnosed(record) => {
            if record.source().len() > limits.bytes {
                return Err(CodecError::Bytes);
            }
            let source = SourceText::new("", record.source());
            let diagnostic = record.diagnostic();
            if diagnostic.hints.len() > limits.nodes {
                return Err(CodecError::Work);
            }
            JournalWire::Diagnostic {
                value: DiagnosticWire {
                    id: record.id().into(),
                    at: record.at().to_string().into(),
                    cell: record.cell().into(),
                    source: record.source().into(),
                    diagnostic: DiagnosticDetail {
                        public_message: diagnostic
                            .public_message
                            .as_ref()
                            .map(|message| message.as_str().into()),
                        code: diagnostic.code.as_ref().into(),
                        message: diagnostic.message.as_str().into(),
                        severity: match diagnostic.severity {
                            Severity::Error => SeverityWire::Error,
                            Severity::Warning => SeverityWire::Warning,
                            Severity::Info => SeverityWire::Info,
                        },
                        start: source
                            .utf16_offset(diagnostic.span.start())
                            .map_err(invalid)?,
                        end: source
                            .utf16_offset(diagnostic.span.end())
                            .map_err(invalid)?,
                        hints: diagnostic
                            .hints
                            .iter()
                            .map(|hint| hint.as_str().into())
                            .collect(),
                    },
                },
            }
        }
    })
}
fn journal_entry(wire: JournalWire<'_>, limits: Limits) -> Result<JournalEntry, CodecError> {
    Ok(match wire {
        JournalWire::ProtectedRun { node, run, handle } => JournalEntry::ProtectedRun {
            node: NodeId::new(&node).map_err(invalid)?,
            run: RunId::new(&run).map_err(invalid)?,
            handle: ValueHandle::new(&handle).map_err(invalid)?,
        },
        JournalWire::Payload { node, run, handle } => {
            if node.len() > 256 || run.len() > 256 {
                return Err(invalid("invalid payload ownership identity"));
            }
            JournalEntry::Payload {
                node: NodeId::new(&node).map_err(invalid)?,
                run: RunId::new(&run).map_err(invalid)?,
                handle: ValueHandle::new(&handle).map_err(invalid)?,
            }
        }
        JournalWire::Retired {
            nodes,
            payloads,
            protected,
        } => {
            let record = wes_engine::history::RetiredWork {
                nodes: nodes
                    .into_iter()
                    .map(|n| NodeId::new(n).map_err(invalid))
                    .collect::<Result<_, _>>()?,
                payloads: payloads
                    .into_iter()
                    .map(|h| ValueHandle::new(&h).map_err(invalid))
                    .collect::<Result<_, _>>()?,
                protected: protected
                    .into_iter()
                    .map(|h| ValueHandle::new(&h).map_err(invalid))
                    .collect::<Result<_, _>>()?,
            };
            record.validate().map_err(invalid)?;
            JournalEntry::Retired(record)
        }
        JournalWire::Requested {
            namespace,
            request,
            cell,
            fingerprint,
            steps,
        } => {
            let record = wes_engine::history::RequestRecord {
                namespace: namespace.into_owned(),
                request: request.into_owned(),
                cell: cell.into_owned(),
                fingerprint: fingerprint.into_owned(),
                steps: steps.into_iter().map(Cow::into_owned).collect(),
            };
            record.validate().map_err(invalid)?;
            JournalEntry::Requested(record)
        }
        JournalWire::Submitted {
            source_name,
            source_start,
            refreshed,
            document,
            revision_of,
            id,
            cell,
            text,
            client,
            context,
            order,
            nodes,
            origin,
            acknowledge_effects,
            from,
            run,
        } => {
            if origin.is_none() && (acknowledge_effects || from.is_some()) {
                return Err(invalid("repeat acknowledgement requires origin"));
            }
            let from = from.map(NodeId::new).transpose().map_err(invalid)?;
            let record = wes_engine::history::SubmissionRecord {
                source_name: source_name.into_owned(),
                source_start: wes_language::Position {
                    line: source_start[0],
                    column: source_start[1],
                },
                refreshed: refreshed
                    .into_iter()
                    .map(|n| NodeId::new(n).map_err(invalid))
                    .collect::<Result<_, _>>()?,
                document: document.map(Cow::into_owned),
                revision_of: revision_of.map(Cow::into_owned),
                id: id.into_owned(),
                cell: cell.into_owned(),
                text: text.into_owned(),
                client: client.into_owned(),
                context: context.map(environment_context).transpose()?,
                order,
                nodes: nodes
                    .into_iter()
                    .map(|n| NodeId::new(n).map_err(invalid))
                    .collect::<Result<_, _>>()?,
                repeat: origin.map(|origin| wes_engine::source::Repeat {
                    origin: origin.into_owned(),
                    acknowledge_effects,
                    from,
                }),
                run: read_run(&run)?,
            };
            record.input().map_err(invalid)?;
            JournalEntry::Submitted(record)
        }
        JournalWire::Trace { node, run, value } => {
            let decoded = super::decode_value(value.as_bytes(), limits)?;
            let record = wes_engine::trace::TraceRecord {
                node: NodeId::new(&node).map_err(invalid)?,
                run: RunId::new(&run).map_err(invalid)?,
                value: decoded.value,
            };
            if !record.validate() {
                return Err(invalid("invalid public trace record"));
            }
            JournalEntry::Trace(record)
        }
        JournalWire::Views { id, value } => {
            let record = wes_engine::views::ViewRecord {
                id: id.into_owned(),
                value: super::decode_value(value.as_bytes(), limits)?.value,
            };
            record.validate().map_err(invalid)?;
            JournalEntry::Views(record)
        }
        JournalWire::Environments {
            id,
            yaml,
            sources,
            before,
            after,
        } => {
            use wes_core::environments::{CapturedSource, CapturedSources, SourceKey};
            if sources.len() > 1024 || before.len() > 128 || after.len() > 128 {
                return Err(CodecError::Work);
            }
            let mut captured = CapturedSources::default();
            let mut seen = std::collections::BTreeSet::new();
            for source in sources {
                let key = SourceKey::new(&source.kind, &source.location).map_err(invalid)?;
                if !seen.insert(key.clone()) {
                    return Err(invalid("duplicate environment source"));
                }
                captured
                    .insert(
                        key,
                        CapturedSource::new(&source.format, &source.source).map_err(invalid)?,
                    )
                    .map_err(invalid)?;
            }
            let revisions = |m: BTreeMap<Cow<'_, str>, Cow<'_, str>>| {
                m.into_iter()
                    .map(|(k, v)| Ok((k.into_owned(), v.parse().map_err(invalid)?)))
                    .collect::<Result<_, CodecError>>()
            };
            JournalEntry::Environments(
                wes_engine::environments::EnvironmentRecord::new(
                    id.into_owned(),
                    yaml.into_owned(),
                    captured,
                    revisions(before)?,
                    revisions(after)?,
                )
                .map_err(invalid)?,
            )
        }
        JournalWire::Notice { value } => {
            let context = match value.context {
                NoticeContextWire::Execution { node, run } => NoticeContext::Execution {
                    node: NodeId::new(node).map_err(invalid)?,
                    run: RunId::new(run).map_err(invalid)?,
                },
                NoticeContextWire::Publication { node, run, handle } => {
                    NoticeContext::Publication {
                        node: NodeId::new(node).map_err(invalid)?,
                        run: RunId::new(run).map_err(invalid)?,
                        handle: handle
                            .map(|handle| ValueHandle::new(&handle))
                            .transpose()
                            .map_err(invalid)?,
                    }
                }
                NoticeContextWire::Keep {
                    handle,
                    may_have_applied,
                } => NoticeContext::Keep {
                    handle: ValueHandle::new(&handle).map_err(invalid)?,
                    may_have_applied,
                },
                NoticeContextWire::Release {
                    handle,
                    may_have_applied,
                } => NoticeContext::Release {
                    handle: ValueHandle::new(&handle).map_err(invalid)?,
                    may_have_applied,
                },
                NoticeContextWire::Eviction {} => NoticeContext::Eviction,
                NoticeContextWire::StorageWorker {} => NoticeContext::StorageWorker,
                NoticeContextWire::WorkspaceShutdown {} => NoticeContext::WorkspaceShutdown,
            };
            JournalEntry::Noticed(
                NoticeRecord::new(
                    value.id.into_owned(),
                    value.at.parse()?,
                    context,
                    read_error(value.error)?,
                )
                .map_err(invalid)?,
            )
        }
        JournalWire::Command {
            source_name,
            source_start,
            document,
            revision_of,
            environments,
            cell,
            text,
            nodes,
            changed_nodes,
            type_sources,
            calculation_package,
            imports,
            replay,
        } => {
            let text = text.into_owned();
            let nodes = nodes
                .into_iter()
                .map(|n| NodeId::new(n).map_err(invalid))
                .collect::<Result<Vec<_>, _>>()?;
            let command = CommandRecord {
                source_name: source_name.into_owned(),
                source_start: wes_language::Position {
                    line: source_start[0],
                    column: source_start[1],
                },
                changed_nodes: changed_nodes
                    .into_iter()
                    .map(|node| NodeId::new(node).map_err(invalid))
                    .collect::<Result<_, _>>()?,
                document: document.map(Cow::into_owned),
                revision_of: revision_of.map(Cow::into_owned),
                environments: environments.map(environment_context).transpose()?,
                cell: cell.into_owned(),
                replay: replay.map_or_else(|| text.clone(), Cow::into_owned),
                text,
                nodes,
                imports: imports
                    .unwrap_or_default()
                    .into_iter()
                    .map(|wire| read_import(wire, limits))
                    .collect::<Result<_, _>>()?,
                calculation_package: calculation_package.map(Cow::into_owned),
                type_sources: type_sources
                    .into_iter()
                    .map(|(k, v)| (k.into_owned(), v.into_owned()))
                    .collect(),
            };
            command.validate().map_err(invalid)?;
            JournalEntry::Command(command)
        }
        JournalWire::Snapshot {
            source,
            epoch,
            delivery,
            observation,
            handle,
            retention,
        } => {
            let observation = read_execution(observation)?;
            let result = match (handle, retention.as_deref()) {
                (None, None) => None,
                (Some(handle), Some(reason @ ("automatic" | "protected" | "unknown"))) => {
                    Some(RetainedResult {
                        node: observation.node().clone(),
                        run: observation
                            .run()
                            .cloned()
                            .ok_or_else(|| invalid("retained snapshot requires run identity"))?,
                        handle: ValueHandle::new(&handle).map_err(invalid)?,
                        retention: if reason == "automatic" {
                            wes_engine::storage::Retention::Automatic
                        } else if reason == "protected" {
                            wes_engine::storage::Retention::Protected
                        } else {
                            wes_engine::storage::Retention::Unknown
                        },
                    })
                }
                _ => return Err(invalid("invalid snapshot retention")),
            };
            let snapshot = wes_engine::history::LiveSnapshot {
                source: NodeId::new(source).map_err(invalid)?,
                epoch: read_run(&epoch)?.ok_or_else(|| invalid("missing source epoch"))?,
                delivery,
                observation,
                result,
            };
            snapshot.validate().map_err(invalid)?;
            JournalEntry::Snapshot(snapshot)
        }
        JournalWire::Result {
            node,
            handle,
            run,
            retention,
        } => JournalEntry::Result(RetainedResult {
            retention: match retention.as_deref() {
                None => wes_engine::storage::Retention::Unknown,
                Some("automatic") => wes_engine::storage::Retention::Automatic,
                Some("protected") => wes_engine::storage::Retention::Protected,
                _ => return Err(invalid("invalid retained result reason")),
            },
            node: NodeId::new(node).map_err(invalid)?,
            handle: ValueHandle::new(&handle).map_err(invalid)?,
            run: RunId::new(&run).map_err(invalid)?,
        }),
        JournalWire::Observation { value } => JournalEntry::Observed(read_execution(value)?),
        JournalWire::Diagnostic { value } => {
            let source = SourceText::new("", value.source.as_ref());
            let detail = value.diagnostic;
            let span = Span::new(
                source.byte_offset(detail.start).map_err(invalid)?,
                source.byte_offset(detail.end).map_err(invalid)?,
            )
            .map_err(invalid)?;
            let diagnostic = Diagnostic {
                public_message: detail
                    .public_message
                    .map(|message| std::sync::Arc::new(message.into_owned())),
                code: detail.code.into_owned().into(),
                message: detail.message.into_owned(),
                span,
                severity: match detail.severity {
                    SeverityWire::Error => Severity::Error,
                    SeverityWire::Warning => Severity::Warning,
                    SeverityWire::Info => Severity::Info,
                },
                hints: detail.hints.into_iter().map(Cow::into_owned).collect(),
            };
            JournalEntry::Diagnosed(
                DiagnosticRecord::new(
                    value.id.into_owned(),
                    value.at.parse()?,
                    value.cell.into_owned(),
                    value.source.into_owned(),
                    diagnostic,
                )
                .map_err(invalid)?,
            )
        }
    })
}
fn error_wire(error: &ErrorValue, limits: Limits) -> Result<ErrorWire<'_>, CodecError> {
    if error.issues().len() > limits.nodes {
        return Err(CodecError::Work);
    }
    Ok(ErrorWire {
        locations: error.locations().iter().map(LocationWire::from).collect(),
        policy: Some(ErrorPolicy {
            origins: error.policy().origins().iter().cloned().collect(),
            private: error.policy().is_private(),
            unknown: error.policy().is_unknown(),
        }),
        id: error.id().as_str().into(),
        code: error.code().into(),
        message: error.message().into(),
        cause: error.cause().map_or("", ErrorId::as_str).into(),
        issues: error
            .issues()
            .iter()
            .map(|issue| IssueWire {
                path: issue.path.as_str().into(),
                code: issue.code.as_str().into(),
                message: issue.message.as_str().into(),
            })
            .collect(),
    })
}
fn read_error(error: ErrorWire<'_>) -> Result<ErrorValue, CodecError> {
    let mut policy = wes_core::flow::FlowPolicy::default();
    if let Some(wire) = error.policy {
        if wire.origins.len() > 128 {
            return Err(CodecError::Work);
        }
        for origin in wire.origins {
            policy = policy.from_origin(origin);
        }
        if wire.private {
            policy = policy.private();
        }
        if wire.unknown {
            policy = policy.unknown();
        }
    } else {
        return Err(invalid("error policy is required"));
    }
    ErrorValue::new(
        ErrorId::new(error.id).map_err(invalid)?,
        error.code.into_owned(),
        error.message.into_owned(),
        error
            .issues
            .into_iter()
            .map(|issue| ValidationIssue {
                path: issue.path.into_owned(),
                code: issue.code.into_owned(),
                message: issue.message.into_owned(),
            })
            .collect(),
        if error.cause.is_empty() {
            None
        } else {
            Some(ErrorId::new(error.cause).map_err(invalid)?)
        },
    )
    .and_then(|value| value.with_locations(error.locations.into_iter().map(Into::into).collect()))
    .map(|value| value.with_policy(&policy))
    .map_err(invalid)
}
fn recovery_wire(entry: &RecoveryEntry) -> RecoveryWire<'_> {
    match entry {
        RecoveryEntry::Accepted { cell } => RecoveryWire::Accepted {
            cell: cell.as_str().into(),
        },
        RecoveryEntry::Calling(call) => RecoveryWire::Calling {
            node: call.node.as_str().into(),
            run: call.run.as_str().into(),
            cell: call.cell.as_str().into(),
            capability: call.capability.as_str().into(),
            safe: call.safe,
            at: call.at.to_string().into(),
        },
        RecoveryEntry::Called {
            node,
            run,
            produced,
        } => RecoveryWire::Called {
            node: node.as_str().into(),
            run: run.as_str().into(),
            produced: *produced,
        },
    }
}
fn recovery_entry(wire: RecoveryWire<'_>) -> Result<RecoveryEntry, CodecError> {
    Ok(match wire {
        RecoveryWire::Accepted { cell } => RecoveryEntry::Accepted {
            cell: cell.into_owned(),
        },
        RecoveryWire::Calling {
            node,
            run,
            cell,
            capability,
            safe,
            at,
        } => RecoveryEntry::Calling(CallRecord {
            node: NodeId::new(node).map_err(invalid)?,
            run: RunId::new(&run).map_err(invalid)?,
            cell: cell.into_owned(),
            capability: capability.into_owned(),
            safe,
            at: at.parse::<Timestamp>()?,
        }),
        RecoveryWire::Called {
            node,
            run,
            produced,
        } => RecoveryEntry::Called {
            node: NodeId::new(node).map_err(invalid)?,
            run: RunId::new(&run).map_err(invalid)?,
            produced,
        },
    })
}
fn read_run(run: &str) -> Result<Option<RunId>, CodecError> {
    if run.is_empty() {
        Ok(None)
    } else {
        RunId::new(run).map(Some).map_err(invalid)
    }
}
fn run_text(run: Option<&RunId>) -> Cow<'_, str> {
    run.map_or("", RunId::as_str).into()
}
fn state_wire(state: NodeState) -> StateWire {
    match state {
        NodeState::Pending => StateWire::Pending,
        NodeState::Running => StateWire::Running,
        NodeState::Ready => StateWire::Ready,
        NodeState::Stale => StateWire::Stale,
        NodeState::Failed => StateWire::Failed,
        NodeState::Cancelled => StateWire::Cancelled,
        NodeState::Skipped => StateWire::Skipped,
    }
}
fn read_state(state: StateWire) -> NodeState {
    match state {
        StateWire::Pending => NodeState::Pending,
        StateWire::Running => NodeState::Running,
        StateWire::Ready => NodeState::Ready,
        StateWire::Stale => NodeState::Stale,
        StateWire::Failed => NodeState::Failed,
        StateWire::Cancelled => NodeState::Cancelled,
        StateWire::Skipped => NodeState::Skipped,
    }
}
fn invalid(message: impl std::fmt::Display) -> CodecError {
    CodecError::Invalid(message.to_string())
}

fn execution_wire(
    record: &ExecutionRecord,
    limits: Limits,
) -> Result<ExecutionWire<'_>, CodecError> {
    Ok(ExecutionWire {
        stale_reason: record.stale_reason().map(|r| r.code().into()),
        id: record.id().into(),
        node: record.node().as_str().into(),
        run: run_text(record.run()),
        at: record.at().to_string().into(),
        state: state_wire(record.state()),
        error: match record.error() {
            None => ErrorSlot::Absent([]),
            Some(e) => ErrorSlot::Present(error_wire(e, limits)?),
        },
    })
}
fn read_execution(value: ExecutionWire<'_>) -> Result<ExecutionRecord, CodecError> {
    let reason = value
        .stale_reason
        .as_deref()
        .map(|code| {
            wes_engine::runtime::StaleReason::from_code(code)
                .ok_or_else(|| invalid("unknown stale reason"))
        })
        .transpose()?;
    let error = match value.error {
        ErrorSlot::Absent([]) => None,
        ErrorSlot::Present(e) => Some(read_error(e)?),
    };
    ExecutionRecord::new(
        value.id.into_owned(),
        NodeId::new(value.node).map_err(invalid)?,
        read_run(&value.run)?,
        value.at.parse()?,
        read_state(value.state),
        error,
    )
    .and_then(|record| record.with_stale_reason(reason))
    .map_err(invalid)
}
