//! Help is data derived from language declarations and provider-owned metadata.
use crate::{
    driver::CancellationToken,
    runtime::{Outcome, RuntimeCode},
};
use std::sync::Arc;
use wes_core::capability::{Catalogue, ProviderDescription};
use wes_core::{Data, Provenance, RecordShape, Shape, Value, capability::Typing};
use wes_language::{
    targets::QueryTarget,
    vocabulary::{
        CommandSpec, MetaCommand,
        commands::{self, COMMAND_PATHS, roots},
    },
};

#[derive(Clone, Debug)]
pub struct BoundHelp {
    target: QueryTarget,
    provider: Option<Arc<ProviderDescription>>,
    importer: Option<(String, crate::imports::ImporterMetadata)>,
    available_importers: Vec<(String, Option<&'static str>)>,
}
impl BoundHelp {
    pub(crate) fn new(target: QueryTarget, catalogue: &Catalogue) -> Self {
        let provider = match &target {
            QueryTarget::Provider { name, .. } => catalogue.provider(name).cloned(),
            _ => None,
        };
        Self {
            target,
            provider,
            importer: None,
            available_importers: vec![],
        }
    }
    pub(crate) fn with_importers(
        mut self,
        parameters: &indexmap::IndexMap<String, crate::imports::ImporterMetadata>,
    ) -> Result<Self, &'static str> {
        self.available_importers = parameters
            .iter()
            .map(|(name, metadata)| (name.clone(), metadata.summary))
            .collect();
        if let QueryTarget::Command(path) = &self.target
            && path.first().is_some_and(|head| head == "import")
            && ((path.len() == 2 && !matches!(path[1].as_str(), "plan" | "apply"))
                || (path.len() == 3 && path[1] == "plan"))
        {
            let parameters = parameters.get(path.last().expect("importer path")).ok_or(
                "Importer is unavailable. Use :list importers to discover supported imports.",
            )?;
            self.importer = Some((
                path.last().expect("importer path").clone(),
                parameters.clone(),
            ));
        }
        Ok(self)
    }
    pub(crate) fn predicted_typing(&self) -> Typing {
        Typing::new(Shape::Unknown)
    }
    pub(crate) fn evaluate(&self, token: &CancellationToken) -> Outcome {
        if token.is_cancelled() {
            return Outcome::Cancelled(RuntimeCode::Cancelled.error("Help cancelled.", None));
        }
        if let QueryTarget::Command(path) = &self.target
            && path.first().is_some_and(|head| head == "import")
            && ((path.len() == 2 && !matches!(path[1].as_str(), "plan" | "apply"))
                || (path.len() == 3 && path[1] == "plan"))
        {
            return match &self.importer {
                Some((name, parameters)) => super::query::importer_help_command(
                    name,
                    parameters,
                    if path.len() == 3 {
                        MetaCommand::ImportPlan
                    } else {
                        MetaCommand::Import
                    },
                    token,
                ),
                None => Outcome::Failed(RuntimeCode::ExecutionFailed.error(
                    "Importer is unavailable. Use :list importers to discover supported imports.",
                    None,
                )),
            };
        }
        if let QueryTarget::Provider { path, .. } = &self.target {
            return match &self.provider {
                Some(provider) => super::query::provider_help_path(provider, path, token),
                None => Outcome::Failed(
                    RuntimeCode::ExecutionFailed.error("Help provider is unavailable.", None),
                ),
            };
        }
        let mut data = match &self.target {
            QueryTarget::Root => fields([
                ("path", text("")),
                (
                    "summary",
                    text(
                        "Choose a command family or provider. :list providers discovers the selected catalogue. :help arguments explains literals and nested references; :help errors explains diagnostic codes and typical fixes.",
                    ),
                ),
                (
                    "children",
                    Data::List(
                        roots()
                            .into_iter()
                            .map(|name| {
                                fields([
                                    ("name", text(name)),
                                    ("summary", text(commands::help_summary(&[name]))),
                                ])
                            })
                            .collect(),
                    ),
                ),
            ]),
            QueryTarget::Command(path) => command_help(path),
            _ => {
                return Outcome::Failed(
                    RuntimeCode::ExecutionFailed.error("Unsupported help target.", None),
                );
            }
        };
        if let QueryTarget::Command(path) = &self.target
            && (path == &["import"] || path == &["import", "plan"])
            && let Data::Record(record) = &mut data
            && let Some(Data::List(children)) = record.get_mut("children")
        {
            children.extend(self.available_importers.iter().map(|(name, summary)| {
                fields([
                    ("name", text(name)),
                    (
                        "summary",
                        text(summary.unwrap_or("Read importer help for required arguments.")),
                    ),
                ])
            }));
        }
        Outcome::Produced(help_value(data))
    }
}
pub(crate) fn command_help(path: &[String]) -> Data {
    if path == ["arguments"] {
        return fields([
            ("path", text("arguments")),
            (
                "summary",
                text(
                    "Named arguments accept literal scalars, existing output references and bounded records/lists. No calls or operations run inside an argument.",
                ),
            ),
            (
                "syntax",
                text(
                    "body:{mode:$level, nested:[$cfg.count, 7]} · values:[] · quoted keys are supported. Quote bracket text when a Text value is intended.",
                ),
            ),
            (
                "typing",
                text(
                    "Each literal is read against its declared field/item type; quoting escapes syntax and does not change contextual typing. Unknown scalar literals are Text. Existing values are never text-coerced. Check a loosely typed whole result before selecting a field.",
                ),
            ),
            (
                "capture",
                text(
                    "Every nested reference captures a producer and selected output once, participates in normal dependencies and resolves from the admitted run's input snapshot. A private child restricts the entire constructed argument. Missing or invalid input does not rerun its producer.",
                ),
            ),
            (
                "views",
                text(
                    "A view follows one result. Compose multiple referenced inputs in a named pure :calc or adapter and bind its result. Nested ?template placeholders are unsupported; whole-argument placeholders retain their contract checks.",
                ),
            ),
            (
                "limits",
                text(
                    "Depth 32, 1000 argument nodes per command, 64 KiB per structured literal and 128 KiB per constructed value and per newly constructed argument set. Management values and nonmaterialized Iter children are refused; collect explicitly.",
                ),
            ),
        ]);
    }
    if path == ["errors"] {
        return fields([
            ("path", text("errors")),
            ("summary", text("Codes describe causes; execution state is separate. Inspect nested causes/issues for provider and contract failures.")),
            ("families", Data::List([("CAL", "Calculation syntax, types, values and budgets"), ("PAR", "Command parsing"), ("CMD", "Command arguments and explicit scope acknowledgement"), ("RES", "Command/target resolution"), ("TYP", "Type/contract validation; TYP000 is successful validation, TYP005 reports a violated constraint"), ("RUN", "Node execution and output availability"), ("ENG", "Engine planning/authority"), ("STO", "Workspace/storage authority"), ("ENV", "Environment/provider binding"), ("IMP", "Import admission and advisory warnings; help import <kind> describes required inputs"), ("DSC", "API documentation conversion")].into_iter().map(|(code, meaning)| fields([("code",text(code)),("meaning",text(meaning))])).collect())),
            ("codes", Data::List(wes_language::calc::diagnostics::CODES.iter().map(|(code,meaning,fix)| fields([("code",text(*code)),("meaning",text(*meaning)),("fix",text(*fix))])).collect())),
        ]);
    }
    if path.len() == 2 && path[0] == "calc" {
        if let Some(spec) = wes_language::calc::Package::standard().operation(&path[1]) {
            return operation_help(&path[1], spec);
        }
    }
    let words: Vec<_> = path.iter().map(String::as_str).collect();
    let mut children = std::collections::BTreeMap::new();
    for p in COMMAND_PATHS
        .iter()
        .filter(|p| p.path.starts_with(&words) && p.path.len() > words.len())
    {
        children.entry(p.path[words.len()]).or_insert(p.summary);
    }
    if words == ["list"] {
        for registry in wes_language::vocabulary::ListRegistry::ALL.iter() {
            children.insert(registry.name(), registry.summary());
        }
    }
    if words == ["env"] {
        for action in wes_language::vocabulary::ENVIRONMENT_COMMANDS {
            children.insert(action.name, action.summary);
        }
    }
    let entry = COMMAND_PATHS.iter().find(|p| p.path == words);
    let summary = if words == ["node", "remove"] {
        "Remove the selected node and its downstream closure. scope:downstream is an explicit acknowledgement that dependents and their names are removed too; node-only removal is not supported."
    } else {
        commands::help_summary(&words)
    };
    let invocation = if let Some(entry) = entry.filter(|_| words != ["env"]) {
        let spec = wes_language::vocabulary::commands::signature(path).expect("command signature");
        let mut description = describe(&spec);
        if let Data::Record(fields) = &mut description {
            if let Some(short) = entry.short {
                let usage = commands::help_usage(&spec);
                fields.insert(
                    "shortForm".into(),
                    text(usage.replacen(&format!(":{}", spec.canonical), &format!(":{short}"), 1)),
                );
            }
            fields.insert("summary".into(), text(summary));
        }
        description
    } else if words.len() == 2 && matches!(words[0], "env" | "list") {
        let spec =
            wes_language::vocabulary::commands::signature(path).expect("environment signature");
        describe(&spec)
    } else {
        Data::Option(None)
    };
    fields([
        ("path", text(path.join(" "))),
        ("summary", text(summary)),
        ("invocation", invocation),
        (
            "children",
            Data::List(
                children
                    .into_iter()
                    .map(|(name, summary)| {
                        fields([("name", text(name)), ("summary", text(summary))])
                    })
                    .collect(),
            ),
        ),
    ])
}

fn operation_help(name: &str, spec: wes_language::calc::OperationSpec) -> Data {
    let help = spec.operation.help();
    let parameters = help
        .parameters
        .iter()
        .take(usize::from(spec.max))
        .enumerate()
        .map(|(i, (name, kind))| {
            fields([
                ("name", text(*name)),
                ("type", text(*kind)),
                ("required", Data::Bool(i < usize::from(spec.min))),
            ])
        })
        .collect();
    let signature = format!(
        "{}({}) -> {}",
        name,
        help.parameters
            .iter()
            .take(usize::from(spec.max))
            .enumerate()
            .map(|(i, (name, kind))| format!(
                "{name}{}: {kind}",
                if i >= usize::from(spec.min) { "?" } else { "" }
            ))
            .collect::<Vec<_>>()
            .join(", "),
        help.returns
    );
    fields([
        ("path", text(format!("calc {name}"))),
        ("summary", text(help.summary)),
        (
            "invocation",
            fields([
                ("usage", text(signature)),
                ("summary", text(help.summary)),
                ("parameters", Data::List(parameters)),
                ("returns", text(help.returns)),
                ("behavior", text(help.notes)),
                (
                    "examples",
                    Data::List(vec![text(format!(":calc {{ return {}; }}", help.example))]),
                ),
                ("prerequisites", text(help.prerequisites)),
            ]),
        ),
        ("children", Data::List(vec![])),
    ])
}

fn describe(spec: &CommandSpec) -> Data {
    let details = if spec.command == MetaCommand::Calc {
        let package = wes_language::calc::Package::standard();
        Some(fields([
            ("command", text(":calc")),
            (
                "functionHelp",
                text(
                    ":help calc <operation>; for example :help calc iter.matches. Function help includes positional parameters, return semantics, examples and prerequisites.",
                ),
            ),
            ("summary", text(spec.summary)),
            (
                "names",
                text(
                    "Local variables, named functions, parameters and loop bindings may shadow operation names. The nearest lexical binding wins; otherwise the registered operation is used. A local scalar called as a function fails rather than falling back to the builtin. Receiver methods such as list.map() or iterator.count() are independent of bare local names; record callable fields keep precedence. Declarations must be initialized before use, including their own initializer; no hoisting. Keywords, true/false/none and the special iter namespace remain reserved. Workspace references ($name) are separate from local names.",
                ),
            ),
            (
                "operations",
                Data::List(package.operations().map(|(name, _)| text(name)).collect()),
            ),
            (
                "time",
                text(
                    "instant(ISO text), duration(ISO duration), interval(start,end), around(center,positive radius). Interval is finite [start,end); equal endpoints are empty. Read .start/.end; subtract endpoints for Duration. Instant +/- Duration, Instant - Instant, Duration +/- Duration, Duration * Int/Decimal (either order), Duration / Int/Decimal and Duration / Duration (exact Decimal ratio); matching Instant/Duration comparisons. fromEpochSeconds/Millis/Nanos accept Int or Decimal with exact nanoseconds; toEpochSeconds/Millis/Nanos return exact Decimal. Int/Decimal display, JSON inspection and copying preserve exact values, including large epoch nanoseconds; text(toEpochNanos(t)) explicitly creates Text. Graphs require accurately representable finite drawing numbers; explicitly scale large values before plotting. durationSeconds/Millis/Nanos(Int|Decimal) and toSeconds/Millis/Nanos(Duration) convert explicit units. Scaling and conversions require exact whole nanoseconds; division by zero, nonterminating ratios and overflow fail. utcParts(Instant) returns UTC year/month/day/hour/minute/second/nanosecond/weekday (Monday=1). No clock reads, implicit units or rounding. API end parameters may be inclusive: explicitly adapt/filter returned endpoints to local [start,end) semantics; during is not implemented.",
                ),
            ),
            (
                "finite lists",
                text(
                    "concat(left,right) or left.concat(right) preserves input order and duplicates; element types must be structurally compatible, with no numeric coercion. Empty inputs are allowed. sortBy(list,selector) or list.sortBy(row => row.at) is ascending and stable for equal keys. Each pure selector runs once per item; captured mutable bindings and provider calls are forbidden. Keys must be matching Int, Decimal, Text, Instant or Duration; explicitly filter missing/Option keys and convert mixed numeric types. Sorting yields between bounded steps and obeys cancellation, work and memory budgets. Open-ended Iter/Stream inputs must first be bounded and collected. Example: concat([{at:2,label:'b'}],[{at:1,label:'a'}]).sortBy(row => row.at).",
                ),
            ),
            (
                "diagnostics and callbacks",
                text(
                    "CAL010: unresolved/reserved/duplicate names; CAL011: immutable binding assignment; CAL012: function/operation arity; CAL013: initialization or return/control-flow error; CAL014: duplicate record field; CAL015: bounds; CAL016: parsing; CAL017: contract validation (inspect issues). :help errors lists causes and fixes. Known same-scope reads before initialization and definite missing returns are rejected before execution; no function hoisting. Conditional/loop-dependent completion retains runtime checks. Eager List callbacks may use local mutable state. Lazy Iter callbacks and sortBy selectors must be verified pure, including helpers, and cannot capture mutable outer bindings; use for-of for effects. No provider/safety or workspace authority checks are bypassed.",
                ),
            ),
            (
                "text and records",
                text(
                    "join(List<Text>,separator) or list.join(separator) joins without adding a trailing separator; empty lists return empty Text. No implicit numeric/Bytes conversion. withFields(record,patch) or record.withFields(patch) creates a fresh shallow structural Record: patch fields replace/add, original values stay unchanged; nominal contracts must be checked again. slice(Text|List,start[,end]) or value.slice(start[,end]) selects [start,end), default end is length. Indices are nonnegative Int, within length; Text indices count Unicode scalar characters, not bytes or grapheme clusters. Examples: join(['a','b'],','); withFields({status:200},{status:503}); slice('a😀b',1,2). All operations obey calculation work/memory limits and yield between bounded steps.",
                ),
            ),
            (
                "capture groups",
                text(
                    "iter.captures(Text,regex) lazily yields {match:Text,groups:List<Option<Text>>}. match is the complete match; groups[0] is regex group 1. Unmatched optional groups are none; an empty matched group is some(''). Named groups also occupy their numbered position. Independent cursors, take/skip/map/filter/collect, strict Text input and existing regex/work/memory limits apply. Example: iter.captures('code=503','code=([0-9]+)').map(row=>unwrapOr(row.groups[0],'')).collect(). Bytes require explicit text(bytes). JSONL remains strict about blank lines.",
                ),
            ),
            (
                "timeline views",
                text(
                    "Timeline is a compiled SDK View for typed UTC samples and events. Its input has view:timeline, id/title, range/coverage Interval, omitted Int, sourceError Text, series:[{id,label,unit,samples:[{id,at:Instant,value:Union<Int,Decimal>,gap:Bool}]}], events:[{id,at:Instant,label,detail}]. Records must be sorted by Instant with unique IDs; gap:true breaks the line. TimelineGroup input is {view:timeline-group,title,range}; create independent Timeline instances and connect them to its members slot for shared viewport, selection and picked-item outputs. At most 8 members, 8 series and 20,000 records per Timeline; each renderer input is bounded to 1 MiB. Details show at most 200 records. Finite [start,end), UTC navigation never runs commands or refetches. Source retention and owner/revision checks still apply. Runnable synthetic adaptation: examples/timeline/main.wes and check.py.",
                ),
            ),
            (
                "iterators",
                text(
                    "iter.lines(text).take(20).collect(); iter.use('Recipe',source); each consumer starts fresh; lazy map/filter callbacks are pure; use for-of for effects; Keep retains recipe and source snapshot",
                ),
            ),
            (
                "calls",
                text(
                    "call('provider', ['capability','path'], {argument:value}); finite, sequential, admitted calls",
                ),
            ),
            (
                "limits",
                text("1,000,000 work units; 64 MiB cumulative allocation; 128 frames; 1,000 calls"),
            ),
        ]))
    } else {
        None
    };
    let mut description = fields([
        ("command", text(format!(":{}", spec.canonical))),
        ("usage", text(commands::help_usage(spec))),
        (
            "summary",
            text(if spec.reserved {
                "reserved, not implemented yet — the name is taken so that implementing it later breaks nothing"
            } else {
                spec.summary
            }),
        ),
        ("implemented", Data::Bool(!spec.reserved)),
        (
            "takes",
            Data::List(spec.tail_words.iter().map(|word| text(*word)).collect()),
        ),
        (
            "parameters",
            Data::List(
                spec.parameters
                    .iter()
                    .map(|parameter| {
                        let mut description = fields([
                            ("name", text(&parameter.name)),
                            ("type", text(parameter.shape.to_string())),
                            ("required", Data::Bool(parameter.required)),
                        ]);
                        let path: Vec<_> = spec.canonical.split_whitespace().collect();
                        let choices = commands::help_choices(&path, &parameter.name);
                        if !choices.is_empty()
                            && let Data::Record(fields) = &mut description
                        {
                            fields.insert(
                                "choices".into(),
                                Data::List(choices.iter().map(|s| text(*s)).collect()),
                            );
                        }
                        description
                    })
                    .collect(),
            ),
        ),
        ("otherArguments", Data::Bool(spec.open_arguments)),
        ("producesValue", Data::Bool(spec.produces_value)),
        (
            "operands",
            fields([
                ("min", Data::Int(spec.operands.min as i64)),
                (
                    "max",
                    spec.operands
                        .max
                        .map(|n| Data::Int(n as i64))
                        .unwrap_or(Data::Option(None)),
                ),
            ]),
        ),
    ]);
    if matches!(
        spec.command,
        MetaCommand::WorkspacePlan | MetaCommand::WorkspaceDelete
    ) && let Data::Record(fields) = &mut description
    {
        fields.insert("planType".into(), text("WorkspaceDeletePlan"));
        fields.insert(
            "examples".into(),
            Data::List(
                [
                    ":workspace plan delete",
                    ":workspace plan delete > plan",
                    ":workspace plan delete workspace:\"demo\" > plan",
                    ":inspect $plan",
                    ":workspace delete $plan",
                    ":workspace delete $plan stop:true protected:true",
                ]
                .into_iter()
                .map(text)
                .collect(),
            ),
        );
        fields.insert("semantics".into(), text("workspace: defaults to the issuing workspace. An existing saved target may be restored for inspection without running commands or changing selection; a missing name is never created. Planning creates an ordinary cell and automatic node reference; > plan is optional. The nominal management value uses shared references; its live authority belongs to the issuing client/session and captures the target identity. A saved projection never restores authority. Applying revalidates that target, never current focus or a replacement with the same name. Plans expire after 120 seconds; planning and read/inspect observations do not invalidate a plan, but changes to deletion effects do. stop:true and protected:true explicitly approve additional effects; blockers and uncertain outcomes still refuse deletion."));
    }
    if spec.command == MetaCommand::List
        && let Data::Record(fields) = &mut description
    {
        let selected = spec
            .canonical
            .strip_prefix("list ")
            .and_then(wes_language::vocabulary::ListRegistry::lookup);
        fields.insert(
            "listing".into(),
            match selected {
                Some(registry) => wes_language::vocabulary::list_metadata_for(&[registry]),
                None => wes_language::vocabulary::list_metadata(),
            },
        );
    }
    if let Data::Record(fields) = &mut description {
        let (examples, rule): (&[&str], &str) = match spec.canonical.as_str() {
            "describe" => (
                &[
                    ":describe file:\"openapi.json\" provider:inventory out:\"inventory.json\" > specification",
                ],
                "Choose exactly one of url: and file:, plus provider:. Parses OpenAPI 3.0/3.1 JSON or YAML deterministically into an editable draft; review in /spec and import separately. Prose and HTML are unsupported. Existing output files are never overwritten.",
            ),
            "calc" => (
                &[
                    ":calc { return 1 + 2; } > total",
                    "// comment\n:calc { return $total; }",
                    ":calc { return [1,2,3].map(x => x*2); } > result *> problem",
                    ":calc { return around(instant('2025-01-01T12:00:00Z'),duration('PT5M')); } > window",
                    ":calc { return $window.end - $window.start; }",
                ],
                "Binding names contain only letters, digits and underscores. // starts a comment outside quoted or argument text; comments preserve source lines. Ordered comparisons require matching Int, Decimal, Text, Instant or Duration operands; convert explicitly when mixing numbers.",
            ),
            "def" => (
                &[":def products as catalog list category:?category"],
                "Template placeholders use ?name; parameters can declare types. A template expands a single command.",
            ),
            "import" => (
                &[
                    ":import spec file:\"/path/service.json\" as:api",
                    ":import process bin:\"/bin/echo\" as:echo",
                ],
                "Choose an importer from :list importers. Importer-specific required arguments depend on that importer; use file: or url: plus endpoint: for spec/openapi, bin: for process.",
            ),
            "import plan" => (
                &[
                    ":import plan spec file:$cfg.path as:api > plan",
                    ":import apply $plan",
                ],
                "Planning freezes public scalar arguments but reads no contents. Existing reference types are checked without coercion. Apply uses the same live principal/environment and unchanged producing runs. Native authority expires after ten minutes, is consumed once before reading, and cannot be restored. Inputs carrying resource-origin restrictions need importer transfer admission and are refused before reading.",
            ),
            "import apply" => (
                &[":import apply $plan", ":import apply $plan replace:true"],
                "Explicitly read and install a live plan once. Submit as one statement with no output binding. A failed or uncertain apply requires a new plan; resubmission of the same cell retains its original outcome. Successfully applied metadata restores from its captured recipe without reading live contents.",
            ),
            "package load" => (
                &[
                    ":package load path:\"/path/package.yaml\"",
                    ":package load path:\"/path/task-board.wes-view.json\"",
                ],
                "Load types or a compiled View package. Use wes-view-package describe/init/build to author Views with the public SDK. Installed Views appear in :list views and :inspect view:TaskBoard; create with :view create TaskBoard input:$data. Packages do not run commands. Choose exactly one of path: and source:. origin: labels inline source.",
            ),
            "env plan" => (
                &[":env plan file:\"/path/environments.yaml\" > proposed"],
                "Choose exactly one of file: and source:. base: resolves relative inputs. reconcile:file or reconcile:source explicitly replaces conflicting workspace edits with that input.",
            ),
            "node timeout" => (
                &[":node timeout $result after:PT5S"],
                "after: is an ISO 8601 duration; PT5S means five seconds.",
            ),
            _ => (&[], ""),
        };
        if !examples.is_empty() {
            fields.insert(
                "examples".into(),
                Data::List(examples.iter().map(|s| text(*s)).collect()),
            );
        }
        if !rule.is_empty() {
            fields.insert("syntax".into(), text(rule));
        }
    }
    if spec.command == MetaCommand::Inspect
        && let Data::Record(fields) = &mut description
    {
        fields.insert(
            "examples".into(),
            Data::List(
                [
                    ":inspect $result",
                    ":inspect sensor history",
                    ":inspect type:Customer",
                    ":inspect view:Timeline",
                    r#":inspect type:"List<Customer>""#,
                    ":inspect type:List",
                ]
                .into_iter()
                .map(text)
                .collect(),
            ),
        );
        fields.insert("views".into(), text("Use :list views and :inspect view:Name for package contracts. :view create/bind references the current committed work output; visible readers do not start source commands. Finite versus window delivery follows the source. :view pin $chart > pinned keeps exactly the displayed projected input as a Protected result, then revision-checks the unchanged view binding after storage and history acknowledgement. Pin requires durable workspace recording; constants and query inputs may also be pinned. Linked input fields require capture first. A kept input survives a failed concurrent view bind; no automatic retry occurs. Pinned inputs refer to the exact retained node/run/handle, never a same-named replacement or a matching digest. Rebind pinned views before releasing their result. :view start/stop controls observation without running or cancelling source commands. Shared state outputs connect with :view link. Event ports preserve committed order in a volatile 32-event/16 KiB window; :view output reads List<T> and links target List<T> fields. Older-event omissions are reported. Event delivery is bounded, never automatically replayed after uncertainty, and does not run providers. :view query $target template:Name from:$source output:selection trigger:manual|commit configures a typed calc query without running it. :view apply explicitly runs SAFE-only work; commit mode follows changed selections until Stop or last close. For an owned stream use mode:live adapter:Draw: a provider template supplies its bounded List<T> window to a typed calc adapter. The source and all adapter operations must be SAFE; Stop or last close joins the owned stream. Stream window omissions remain visible. Query bindings capture their environment name and revision; another window cannot change the destination. Changed environments require explicit rebinding. Query jobs share operation and stream capacity, replace older jobs only after cancellation joins, and keep transient result data outside definition storage. Workspace save and normal shutdown checkpoint view definitions and committed selections; reopening waits for Start (observations) or Apply (queries). Source payloads use existing retention: missing saved values are unavailable, never rerun automatically. Unsaved interaction changes can be lost on a crash. :view output reads a committed port as a normal typed result (selection is Option<Interval>). :view capture $dashboard freezes native inputs, linked fields, outputs, membership and source/run metadata in a finite evidence result (8 MiB logical limit). Missing inputs, omissions and running queries remain explicit; inputs may represent different source instants. Capture does not run providers or guarantee durable retention. Check the result retention status and use Keep/Pin; existing JSON export includes the evidence data."));
        fields.insert("selection".into(), text("Choose exactly one target. type: is a literal type selector, never a value reference."));
        fields.insert("types".into(), text("Use :list types for workspace-wide contracts and generic constructors. Inspection captures effective constraints and optional fields at execution entry; it does not validate data or invoke providers."));
    }
    if let (Data::Record(fields), Some(Data::Record(extra))) = (&mut description, details) {
        fields.extend(extra);
    }
    description
}
fn fields<const N: usize>(fields: [(&str, Data); N]) -> Data {
    Data::Record(
        fields
            .into_iter()
            .map(|(name, data)| (name.to_string(), data))
            .collect(),
    )
}
fn text(value: impl Into<String>) -> Data {
    Data::Text(value.into().into())
}

/// One bounded help implementation for source execution and metadata-only API discovery.
pub fn help_query(
    call: &wes_language::Call,
    catalogue: &Catalogue,
    importers: &indexmap::IndexMap<String, crate::imports::ImporterMetadata>,
    token: &CancellationToken,
) -> Result<Value, wes_language::Diagnostic> {
    let target = wes_language::targets::resolve(call, true, catalogue)?;
    match BoundHelp::new(target, catalogue)
        .with_importers(importers)
        .map_err(|message| {
            wes_language::Diagnostic::error("RES005", call.span, message)
                .with_public_message(message)
        })?
        .evaluate(token)
    {
        Outcome::Produced(value) => Ok(value),
        _ => Err(wes_language::Diagnostic::error(
            "CMD003",
            call.span,
            "Help could not be produced within its budget.",
        )
        .with_public_message("Help could not be produced within its budget.")),
    }
}

/// Expand a bounded tree in one read; every node still comes from the authoritative help model.
/// Depth zero is the normal summary. Oversized trees ask for a narrower target, never emit partial JSON.
pub fn help_tree(
    call: &wes_language::Call,
    catalogue: &Catalogue,
    importers: &indexmap::IndexMap<String, crate::imports::ImporterMetadata>,
    token: &CancellationToken,
    depth: u8,
) -> Result<Value, wes_language::Diagnostic> {
    let failure = || {
        wes_language::Diagnostic::error(
            "CMD003",
            call.span,
            "Help tree exceeds depth 3, 128 entries or 64 KiB; choose a narrower command/provider path or lower depth.",
        )
    };
    if depth > 3 {
        return Err(failure());
    }
    let target = wes_language::targets::resolve(call, true, catalogue)?;
    fn expand(
        target: QueryTarget,
        catalogue: &Catalogue,
        importers: &indexmap::IndexMap<String, crate::imports::ImporterMetadata>,
        token: &CancellationToken,
        depth: u8,
        count: &mut usize,
        bytes: &mut u64,
    ) -> Result<Data, ()> {
        if *count == 0 || token.is_cancelled() {
            return Err(());
        }
        *count -= 1;
        let help = BoundHelp::new(target.clone(), catalogue)
            .with_importers(importers)
            .map_err(|_| ())?;
        let Outcome::Produced(value) = help.evaluate(token) else {
            return Err(());
        };
        *bytes = bytes
            .checked_sub(crate::value_size::data_charge(value.data(), *bytes).ok_or(())?)
            .ok_or(())?;
        let mut data = value.data().clone();
        if depth > 0
            && let Data::Record(fields) = &mut data
            && let Some(Data::List(children)) = fields.get_mut("children")
        {
            for child in children {
                let Data::Record(info) = child else {
                    continue;
                };
                let Some(Data::Text(name)) = info.get("name").cloned() else {
                    continue;
                };
                let next = match &target {
                    QueryTarget::Root => QueryTarget::Command(vec![name.to_string()]),
                    QueryTarget::Command(path) => QueryTarget::Command(
                        path.iter().cloned().chain([name.to_string()]).collect(),
                    ),
                    QueryTarget::Provider {
                        name: provider,
                        path,
                    } => QueryTarget::Provider {
                        name: provider.clone(),
                        path: path.iter().cloned().chain([name.to_string()]).collect(),
                    },
                    _ => return Err(()),
                };
                let mut expanded =
                    expand(next, catalogue, importers, token, depth - 1, count, bytes)?;
                if let Data::Record(fields) = &mut expanded {
                    fields.insert("name".into(), Data::Text(name));
                }
                *child = expanded;
            }
        }
        Ok(data)
    }
    let data = expand(
        target,
        catalogue,
        importers,
        token,
        depth,
        &mut 128,
        // Conservative in-memory charge is larger than the MCP wire budget (64 KiB).
        &mut (1024 * 1024),
    )
    .map_err(|_| failure())?;
    Ok(help_value(data))
}

/// A declared presentation type travels with kept/copied help data. The GUI never guesses
/// that an ordinary user record is help from its keys or the cell's source text.
pub(super) fn help_value(data: Data) -> Value {
    let Data::Record(fields) = &data else {
        unreachable!("help record")
    };
    let shape = Shape::Record(
        RecordShape::new(
            "wes.Help",
            fields.keys().map(|name| (name.clone(), Shape::Unknown)),
        )
        .expect("help fields"),
    );
    Value::new(shape, data, Provenance::default()).expect("help shape")
}
