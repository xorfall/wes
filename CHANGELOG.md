# Changelog

## 0.2.0 — Unreleased

Candidate changes since the `v0.1.0` tag. This entry does not announce a release.

### Added

- Scoped temporary investigations with `:transient { ... } > investigation`.
  Auto-Keep is disabled inside the scope, while ordinary workspace behavior is
  preserved. Dataset analysis and checkpoints can use bounded temporary storage.
  Explicit Keep can save one exact result with its required dependencies, or the
  investigation's captured membership and lineage for reopening without replay.
- Immutable captured sources, readable retained Datasets, bounded record analysis,
  excerpts and continuation with reviewed cumulative limits. Malformed records
  can retain source evidence instead of losing the entire investigation.
- Bounded stored snapshots for supported finite Text, Bytes and composite Process
  results. Cold inputs are prepared through shared storage before consumption;
  field projections and range reads can avoid materializing unrelated large data.
- Declarative typed CLI providers with explicit native argument mappings, optional
  parameters, list spreading, flags and JSON arguments. Definitions are frozen
  with their environment revision; no implicit shell interpolation is added.
- Captured contract metadata, declared enum completion and semantic display tones
  across typed definitions, retained results, paging and projections.
- Pure `decodeJson`, returning bounded diagnostic values for malformed input,
  contract mismatch or ambiguity. Cancellation and exhausted budgets remain failures.
- Encrypted result and Dataset storage in explicitly configured homes, with
  `confidential` and `confidential-temporary` residence policies. Memory-only
  `private` results retain that promise. Keep never grants export or execution access.
- Typed View command preparation, independently connected evidence, acknowledged
  source readiness and lifetime-owned live controls. Preparation does not submit
  commands; an explicit action still owns execution.
- Typed table selection actions, draggable graph layout with Reset and a working
  minimap, bounded graph value excerpts, and reusable detached windows with their
  own workspace selection.

### Changed

- Result validity, accessibility and memory residence are separate. Evicting a RAM
  copy with valid backing does not rerun its producer. Capacity refusal does not
  mean the result was deleted.
- Shared reads preserve capacity, denied/withdrawn authority, stale identity,
  unavailable storage, corruption, closure, failure and uncertain completion
  through engine, transport and UI. Unknown external outcomes never authorize replay.
- View creation accepts preparation data without requiring an automatic rendering
  discriminator. Explicit rendering and data preparation can be separate steps.
- Shift-click selects table rows; Command-click retains peek behavior. Selections
  remain tied to the exact captured source and revision.
- Long calculation previews retain syntax highlighting up to 2,048 characters.
  Larger command/result previews remain bounded and offer explicit expansion.
- Desktop-owned terminals survive suspended views and reconnect to their existing
  owner. Command discovery, saved pane history, local PATH, UTF-8 input and wheel
  routing follow shared lifetime and transport rules.
- MCP progress describes the actual pending step, and definition-loading messages
  explain what was committed and the next action.

### Fixed

- Terminal output gaps recover a bounded screen/history snapshot at an exact byte
  cursor instead of resetting the display and replaying an arbitrary byte tail.
  Split UTF-8/VT sequences no longer appear as stray control text. Normal history,
  cursor, common drawing/input modes and alternate-screen return are restored
  without restarting a process or resending input. Resize recovery uses an
  acknowledged geometry epoch, and late frames cannot cross terminal lifetimes.
- View publication, retained-source ownership, policy propagation and revocation
  remain consistent through resident and stored values, partial reads and rebinds.
- Process cleanup joins physical completion after natural exit; unknown cleanup
  outcomes remain visible rather than being reported as confirmed success.
- Acceptance helpers bound child completion and output draining; capacity fixtures
  exercise explicit refusal rather than a changed budget assumption.
- Updated the source-map-js dependency to 1.2.2, addressing indexed source-map
  offset denial of service (GHSA-68fv-2mgg-jv7q).

### Upgrade and scope notes

- This development version changes result/storage and transport contracts. Older
  incompatible records fail safely; no general migration or legacy reader is promised.
  Keep an existing data home intact if its evidence matters, and use a separate
  home when testing the candidate.
- Saved evidence preserves identity and policy, not execution grants. Reopening
  and materialization do not reacquire data or automatically replay providers.
- Large acquisition supports the declared native adapters and contracts; arbitrary
  streaming JSON Record/refinement ingestion and a whole-process RSS guarantee are
  outside this release scope. Full-value consumers can still return Capacity.
- Encryption protects result/Dataset payloads, not every definition, command or
  history entry in the home. The external storage key is required to reopen them.
- Terminal recovery is bounded, process-lifetime presentation state, not a durable
  terminal archive or a promise to reproduce every terminal extension.
- CI investigation definitions and the RabbitMQ study package are separate use
  cases and are not included. Core capabilities remain provider-independent.
- Local release acceptance is on macOS with isolated homes and synthetic services;
  it does not establish Windows/Linux packaging or live service behavior.

## 0.1.0

Initial tagged development baseline. See the
[v0.1.0 tag](https://github.com/xorfall/wes/tree/v0.1.0).
