# Workspace concepts

A workspace contains command cells, definitions and result nodes. A result name
such as `$total` refers to a node. Field access such as `$response.body` selects
within a result. Calculations build on those references; stale or unavailable
inputs are not treated as fresh values.

Providers supply typed operations. Importing a service contract makes its operation
and authentication requirements discoverable; it does not provide credentials or
call that service. Environments bind contracts to endpoints and execution targets.
The chosen authentication method and credential availability are separate from the
provider definition. Use the environment UI to configure access deliberately.

Imported-operation parameters and typed `:def` parameters expose choices from
their declared scalar enum contracts. Text members keep their exact text; int,
decimal and bool members keep exact scalar spelling, including decimal scale.
Every documented `OneOf` rule narrows the full contract domain by intersection;
inferred rules do not narrow suggestions. Duplicates are removed in declaration
order before counting. Discovery never calls a provider or changes validation.

Vocabulary parameters include optional `choices` with `kind` (`text`, `int`,
`decimal` or `bool`), string `members`, the full effective `total`, and `complete`.
The preview contains at most 64 members and its encoded choices object is at most
16 KiB. A truncated preview sets `complete: false`; validation still uses the
full contract. An empty intersection publishes an empty, complete domain. When
there is no declared finite scalar domain, `choices` is absent. Selector and
legacy `allowed` suggestions remain unchanged, and `$` references remain valid
arguments.

Validated results can also carry optional captured contract metadata beside their
shape, provenance and data. Typed `:def` outputs, successful `:type check` commands
and documented HTTP response bodies capture the resolved contract that accepted
the result. Loading a type package or refining a structural shape alone does not
attach a contract. Parameter checks preserve the producer's captured metadata.

Browser value envelopes and MCP `value_read` with `typed: true` expose `meta`.
Its version is 1, with a root `contract: {name, digest}`, a `truncated` flag and a
`fields` map of declaration descriptors. Scalar descriptors use only `text`,
`int`, `decimal` or `bool`. Each carries `contract` and `kind`. Scalar enums
additionally carry `source: "validated"`, string `members`,
full deduplicated `total`, `complete`, and optional `tones`. Integer, decimal and
Boolean members use exact strings, preserving integer precision and decimal
scale. Missing metadata or an omitted path means unknown; a member absent from
an incomplete preview cannot be rejected as outside the declaration.

Declaration paths use root `""`, record `/f:<escaped-key>`, list `/e` and option
`/o`. Field keys escape `~` as `~0` and `/` as `~1`; for example
`/e/f:details/f:a~1b~0`. These are schema paths, separate from data JSON Pointers:
all indices of a list share `/e`. Empty lists retain their element descriptors,
and `Option<T>` describes the some-domain beneath `/o`. Optional record fields
keep their direct field path. Ambiguous unions, dynamic map fields and lazy
iterator elements do not expose inferred domains.

Each enum descriptor fits at most 64 members and 16 KiB of compact UTF-8 JSON,
including its identity and complete tone map. Metadata also stops at 128
descriptors, 64 KiB, 64 levels or 100,000 traversal visits. Aggregate omission
sets `truncated: true`; enum preview omission sets `complete: false`. Direct
identity, field/index projection and option unwrapping preserve and rebase
captured metadata. Arithmetic, concatenation, mapping, aggregation and record
reconstruction drop it; a typed output can validate and capture a fresh contract.
Rolling stream windows also reconstruct a list and drop item annotations before
retention and byte charging. Individually delivered stream events keep the
validated producer's metadata.
Paging keeps the declared domains regardless of the page's observed members.
Typed `shape_only` reads also include optional `meta`; untyped reads remain data.
Root ownership and export policy apply before selection, and delivery checks the
captured publication again. Reading metadata never executes a producer.

Type packages accept versions 1 and 2. Version 2 adds `display.enumTones` to
scalar Text, Int, Decimal and Bool enums. Cases must belong to the full effective
enum, use exact scalar spelling, and map to `ok`, `warn`, `bad`, `dim`, `meta` or
`ink`. Quote numeric and Boolean keys. `ink` requests neutral foreground;
`inherit` is reserved for separate presentation-entry fallback. Tone maps have
at most 64 cases and must fit the descriptor byte bound. Narrowed aliases inherit
only applicable cases, and explicit child cases override them. Invalid display
declarations fail atomically with `TYP002`. Display stays separate from validation
constraints and structural subtyping.
The browser compares Decimal contract tones by exact numeric value. If differently
spelled but numerically equal cases declare conflicting tones, the value remains
uncolored rather than choosing a tone from a spelling the reader may not retain.

Contract digests are `sha256:` hashes of versioned canonical resolved content,
including alias identity, constraints, ordered fields/enums, referenced digests
and display declarations. They do not depend on file paths or later registry
lookups. Container identities are kept separately from public scalar descriptors:
retained-value format version 2 uses an optional `metaProjection` snapshot to
preserve direct container projections after reopening. Its public `meta` is
exactly the scalar-only channel used by browser readers. The snapshot shares the
128-descriptor/64 KiB bounds and both representations are charged to retention
and read budgets. Names beyond the public 1024 UTF-16-unit bound are unavailable
on that channel; omitted scalar descriptors mark it truncated.

Retained-value format version 2 preserves captured descriptors; reopening
keeps yesterday's digests and tones. Imported definitions with different display
content conflict instead of silently reusing an older contract. Complete-domain
lookup against captured snapshots is a later extension. Until it exists,
consumers must treat membership outside an incomplete preview as unavailable.
See [declared tones](../examples/declared-tones/README.md) for a runnable synthetic
example.

Execution state describes whether a run completed, failed, stopped or remains live.
An HTTP response is data, including its status, headers and body. Receiving a 4xx
response is distinct from being unable to perform the transport operation. JSON
bodies can be inspected structurally; other bodies preserve their representation.

Streams can continue after an initial result is available. Their display and
retention are bounded separately from computation. Explicit views present live
values; stop unneeded work and do not assume closing an agent connection cancels
it. A stopped stream's last observation is not a current calculation input.

View input references distinguish a current result from a retained value.
**Current** follows committed publications of its referenced node; it does not
run that node. **Stop observing** pauses this view's reads. **Pin input** retains
the input actually shown and binds the view to that fixed value, including a
selected field rather than its entire parent result. Keeping the source result
is a separate operation.

The dashboard library and editor provide **Back to session**. A dashboard opened
in a result tab provides **Close dashboard**, which closes that tab. These
navigation actions do not remove the saved layout or cancel capture sources.
Leaving through the exit button while editing asks before discarding the draft.

For a live window, **Hold** freezes the local display while sampling continues.
Log **follow** keeps the newest event visible; **reading** keeps the reader's
position while new events arrive. Neither stops the source. Use the run's stop
action to cancel work, including an owned source when that is the intended scope.

The default execution policy, `automatic`, recomputes eligible bounded, pure,
repeatable dependents after their inputs change. `manual` leaves them stale;
`reactive` permits eligible repeatable dependents to run again. Execution
permissions and declared traits are still required. An unknown external outcome
never causes automatic replay.

Saving a workspace records its definitions and retained state. Loading it does not
automatically replay commands. Keep results explicitly when persistence is wanted.
Deletion uses a typed workspace deletion plan so its effects can be reviewed before
execution. A stale plan cannot silently authorize a changed set of effects.

UI slash commands manage presentation and panes. Colon commands belong to the
workspace language. The built-in help reports the current syntax, parameter types
and operation signatures rather than requiring separate handwritten command tables.
