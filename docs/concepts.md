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
