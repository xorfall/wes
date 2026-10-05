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
