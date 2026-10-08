# Testing

Run from the repository root unless a working directory is shown. These checks
use synthetic inputs; no account, model key or application service is required.
Dependency installation uses public package registries.
Executable checks require Python 3.11 or newer (the assistant fixture uses tomllib).

```sh
npm ci
(cd tools/describe && go build -o wes-extract ./cmd/extract && go test ./... && go vet ./...)
(cd gui && npm run typecheck && npx vitest run)
node tools/desktop-build.mjs
cargo fmt --all --check
cargo test --workspace --locked -- --test-threads=4
```

`typecheck`, `dev` and `build` regenerate the ignored View contract files and
catalog from validated manifests. Run `npm run view:generate --workspace
wes-gui` before invoking Vitest directly on a fresh checkout. This also generates
the sample View contracts used by the GUI tests.

The first Rust build needs the platform prerequisites of the desktop crate.
macOS is the verified workspace-test platform. A smaller CLI/engine development
run can exclude the desktop crate with `--exclude wes-desktop`.

For declared enum choices in parameter completion, run the backend suites offline
(the application package is named `wes`). `CARGO_TARGET_DIR` may point at a shared
cache for compatible worktree builds:

```sh
cargo fmt --all --check
cargo test -p wes-core --offline -- --test-threads=4
cargo test -p wes-adapters --offline -- --test-threads=4
cargo test -p wes --offline -- --test-threads=4
```

Core capability tests cover scalar extraction, exact spelling, deduplication,
64/65/200-member previews, encoded byte bounds and full-domain intersections
with documented rules. Descriptor contract tests check imported metadata and
invalid arguments rejected before credential lookup or any loopback request.
Application projection tests check imported and `:def` choices, exact numeric
wire strings, empty domains, truncation and unchanged selector/legacy `allowed`.
These commands exclude desktop and GUI suites and use synthetic fixtures and
isolated data homes.

## Captured contracts and declared tones

Run the backend packages offline with the checkout's target directory. These
suites include browser web routes, MCP typed reads, codec rejection, source
replay, metadata accounting and View consumers. They use isolated homes and
synthetic providers/services.

```sh
cargo fmt --all --check
cargo test -p wes-core -p wes-adapters -p wes-engine -p wes-views -p wes --offline --target-dir target -- --test-threads=4
cargo build -p wes --offline --target-dir target
python3 examples/declared-tones/check.py --binary target/debug/wes
```

For focused contract acceptance and a real browser envelope printed by a test:

```sh
cargo test -p wes-core --test metadata --test enum_tones --offline --target-dir target
cargo test -p wes-adapters --test codec captured_metadata --offline --target-dir target -- --nocapture
cargo test -p wes-adapters --test value_selection --offline --target-dir target
cargo test -p wes-engine --test session captured_metadata --offline --target-dir target
cargo test -p wes-engine --test source hydrated_metadata --offline --target-dir target
cargo test -p wes --lib typed_value_read_carries --offline --target-dir target
cargo test -p wes --test web captured_contract --offline --target-dir target
```

The core cases cover empty lists, nested/optional records, options, escaped
schema keys, exact scalar spellings, 64/65/200-member previews, UTF-8 byte bounds,
aggregate depth/work limits and tone inheritance. Adapter and application cases
check format version 2 retention, malformed descriptors, selected/paged/shape-only
reads, root policy before path lookup, response-contract selection and revision
refusal. Reopening preserves older captured digests and tones without reading the
current registry or replaying a producer. Full captured-domain lookup for
incomplete previews is deferred; absence from a preview is unknown membership.

## Build cache size

Development and test builds use `line-tables-only` debug information: backtraces
keep source file and line numbers, without the type and variable information
needed to inspect locals in a debugger. For a debugging session that needs those
details, use `CARGO_PROFILE_DEV_DEBUG=2 cargo build` or
`CARGO_PROFILE_TEST_DEBUG=2 cargo test`.

Cargo keeps build variants for different settings and dependency graphs in
`target`; its automatic global-cache cleanup does not prune these build outputs.
Separate worktrees can also accumulate independent caches. Reuse a
target directory for compatible local worktree builds with `--target-dir`, and
remove temporary build directories when their checks are complete. Run builds
and cleanup sequentially; do not delete a cache used by a running build or app.

To reclaim development and test outputs while keeping release builds:

```sh
du -sh target
cargo clean --profile dev
```

The next development/test build recompiles its dependencies. This is manual
maintenance, not a disk quota; incremental compilation remains enabled to speed
up source edits. `cargo clean` without a profile removes release outputs too.

## Runnable examples

The runnable examples exercise actual files and transports. Build `wes` and the
extractor first:

```sh
cargo build -p wes --locked
(cd tools/describe && go build -o wes-extract ./cmd/extract)
python3 examples/quickstart/check.py
python3 examples/api-import/readme/check.py
python3 examples/openapi-import/check.py
python3 examples/editable-api-draft/check.py
python3 examples/schema-provenance/check.py
python3 examples/api-workflow/check.py
python3 examples/prometheus-workspace/check.py
python3 examples/prometheus-workspace/run.py --check
python3 examples/terminal/check.py
python3 examples/assistant/check.py
```

Terminal and assistant examples need a Unix PTY. They launch synthetic child
programs, not installed model clients. Each check owns its temporary data home
and loopback servers. Do not replace these fixtures with a real workspace.
Container examples default to a synthetic daemon; their explicit `--real` modes
operate Docker and are not part of the default offline checks.

On Windows a local terminal pane is Windows PowerShell behind a pseudoconsole.
Its tests start real `powershell.exe` processes and run with the Rust suites:

```sh
cargo test -p wes-adapters --lib conpty_tests --locked
cargo test -p wes --lib terminal::windows_tests --locked
cargo test -p wes --test terminal_windows --locked
```

Windows passes "Ctrl+C is disabled" from a launcher to everything below it. The
pane's shell accepts the interrupt again, and one test starts the host that way
to show it. The line editor is the system's PSReadLine with in-memory history.

`wesx`, `wes-value`, `wes-provider` and each provider name are the executable
itself under that name, never a batch file, so a caller's arguments reach the
bridge unchanged. The last suite passes quotes, shell metacharacters and
non-ASCII text to such a program directly, and runs the commands from a served
PowerShell pane. Windows PowerShell 5.1 itself drops double quotes inside an
argument it passes to any program; write them as `\"` there. Providers whose
names differ only by case get no command. When the executable is on another
volume than the pane's directory it is copied once instead of linked; a machine
with a second volume exercises that for real.

`claude`, `codex` and `opencode` in a pane are launchers that attach the
workspace server. The server is the real executable started as
`--assistant-mcp --bridge FILE`, so a client that passes on none of the pane's
environment still reaches the bridge. The suite starts it that way, requires
every output line to be a protocol message, and checks that its access ends
when the pane closes. A pane's history is kept in the same private records as on
Unix and offered to the next shell of that pane as text.

An SSH target uses the configured native client, such as the system's
`C:\Windows\System32\OpenSSH\ssh.exe`, for finite execution and for terminals.
The Windows client is given the two host values it cannot start without and
nothing of the user's environment. Its terminal tests put `cmd.exe` in the
client's place to exercise the pseudoconsole path; a real server is not part of
the offline suites.

For View extension checks, install the npm workspace dependencies from the
repository root. Node.js 22 or newer is required by the GUI workspace; the
independent compiler supports Node.js 20 or newer.
Build its native contract validator and select that exact binary:

```sh
npm ci
cargo build -p wes -p wes-views --bin wes --bin wes-view-build --locked
export WES_VIEW_CONTRACT_TOOL="$PWD/target/debug/wes-view-build"
node --test tools/view-package/compiler.test.mjs
node --test tools/desktop-build.test.mjs
node --test tools/view-toolchain/export.test.mjs
python3 examples/view-packages/check.py --binary target/debug/wes
python3 examples/view-instances/check.py --binary target/debug/wes
(cd gui && npm run view:generate)
(cd gui && npm run view:build)
```

The compiler suite builds independent synthetic packages outside the checkout
and checks structured diagnostics. Its packed-package test downloads public npm
dependencies into an empty temporary cache, then verifies a locked offline
reinstall of the SDK and compiler with networking disabled and an unreachable
proxy. It does not use the contributor's existing npm cache or npm configuration.
The toolchain export check invokes the actual CLI, installs the exported lockfile
with isolated npm configuration, repeats that installation offline, and builds
and checks a View with the included validator. It also verifies no-overwrite and
platform-mismatch refusals. `WES_VIEW_HOST_BINARY` can select a different built
host executable for this check.
The examples use temporary data homes; View discovery,
instance creation and connection do not run external providers. The builtin
development harness is `tools/view-dev`; `view:build` checks its production
bundle. Compiled package installation and workspace-scoped web discovery are
also covered by:

```sh
cargo test -p wes-engine --test source view_packages --locked
cargo test -p wes --test web view_packages --locked
```

Rust inline tests remain next to the behavior they protect; integration suites
exercise public boundaries. GUI tests cover session projection and interaction,
while Go tests cover extraction, evidence and contract validation. Recorded
benchmark output is not a fixture: use the performance example generators to
measure your own build. See the [examples index](../examples/README.md) for
runnable demos, prerequisites and regression fixtures.

## GitHub Actions

[CI](../.github/workflows/ci.yml) plans tests on pushes to `main` and pull
requests. It runs affected packages and suites, rather than every suite for
every change. The selection rules are in [ci/tests.toml](../ci/tests.toml), and
the planner is [tools/ci/impact.py](../tools/ci/impact.py). The job summary records
changed inputs, selected packages, suites and reasons. Selector and policy tests
always run, including for documentation-only changes.

Rust dependencies come from the workspace manifests, including path, inherited,
build, dev and target-specific dependencies. A crate change selects its own tests
and all transitive consumers, not every dependency's tests. Literal include and
module-path inputs outside crates are indexed too. The TOML
rules cover runtime reads and cross-language consumers: GUI/desktop assets, Go
extraction, builtin Views/SDK/compiler, operating budgets, acceptance fixtures
and executable examples. Examples deliberately have a broad rule because Rust,
Go and GUI tests import them. New runtime dependencies must update this manifest.

| Changed input | Selected checks |
| --- | --- |
| General documentation | Selector/policy tests only |
| GUI source | GUI checks and desktop package tests |
| Terminal input handler or app fixtures shared with the GUI | Application/desktop, GUI and examples |
| CLI/application source | Application and desktop tests, offline examples |
| Application web transport | Application/desktop, GUI and offline examples |
| Extractor source | Go tests/vet, application/desktop integration and examples |
| Core types | Core consumers and all cross-language contract suites |
| Dependency manifests, lockfiles, build scripts, workflow or test policy | Full verification |
| Unclassified file | Full verification |

Selection starts from a successful ancestor `CI` run on `main`. PRs also check
merge-base ancestry; pushes check the `before` commit's ancestry. Thus changes
from failed, cancelled or replaced pending runs remain in the comparison.
Missing history, unavailable GitHub API access, initial/force pushes, symlinks
and submodules select full verification. Renames consider both old and new paths;
deletions and executable-mode changes are real inputs. Policy tests currently
require no tracked links; adding a symlink/submodule needs an ownership-policy
update first. Unsupported workspace or policy definitions fail planning rather
than silently dropping tests.

Full verification also runs on version-tag pushes (`v*`), merge-queue events,
manual dispatch and Sundays at 03:00 UTC. These only run checks, not publishing.
Use **CI result** as the required branch-protection check: it always runs and
rejects a selected job that fails, is cancelled or is skipped. Unselected jobs
must agree with the plan. Branch-protection settings are not changed by this
repository. Main runs are not cancelled by newer pushes; superseded pending runs
are still accounted for by the last successful baseline.

Client/extractor checks use Ubuntu, and Rust/desktop and offline examples use
macOS. Jobs use the Rust pin, Node 22, Python 3.11+ and the extractor's Go pin.
CI disables incremental Rust caches because runners do not edit source between
checks; local development keeps incremental builds. Rust tests use four threads
per test binary to bound concurrent subprocess work. There are no automatic
test retries or silent exclusions of flaky tests.

Partial Cargo runs can enable different dependency features from full workspace
runs. Each test crate declares its own required test features; weekly full runs
also check the combined feature set. Selection is conservative at package level,
not a function-level coverage algorithm: low-level crate changes still select
most of the workspace.

To inspect the same selection locally, after the prerequisites above:

```sh
python3 -m unittest discover -s tools/ci -p 'test_*.py'
python3 tools/ci/impact.py --base origin/main --worktree > /tmp/wes-test-plan.json
python3 tools/ci/run.py rust --plan /tmp/wes-test-plan.json --dry-run
python3 tools/ci/run.py rust --plan /tmp/wes-test-plan.json
```

Use `--files gui/src/surface/Cell.tsx` instead of `--base` to inspect a hypothetical
change, or `--full` to explicitly select everything. `run.py` also accepts `gui`,
`extractor`, `compiler` and `examples`; omitted suites do nothing. It assumes the
documented dependency installation and required binaries/assets are prepared;
CI prepares those before running selected checks. The selector itself needs no
Rust build, external Python packages or contributor credentials.

## Timing and flake diagnosis

Finite source-command helpers wait for the submitted cell's current nodes to
complete using the engine's `sequential_step_state`, subscribed before admission.
The snapshot's `values` map also holds display-only previous results while a
refresh is running; presence in that map does not prove completion or live plan
authority. The workspace helper has a held-manager regression covering both
successful and failed replanning, without delays or external processes.

Keep readiness separate from deadlines. The SSH fixture publishes its completed
PID file atomically, and deadline tests first verify that the real OS child
started. Only then do they pause and advance Tokio's clock. The production SSH
deadline still includes queueing and launch, and uncertainty still forbids
automatic replay. Other OS interaction tests retain real time and explicit
readiness/cleanup checks; a paused clock is not a substitute for process startup.

Workspace deletion plans use the application's monotonic Tokio clock and one
120-second TTL. Boundary tests cover the exact deadline, and the public-command
expiry test advances time after plan creation instead of sleeping two minutes.
Production uses real monotonic time; no public test-only clock or command exists.

If a test fails, keep that result and inspect its assertion, readiness signals,
clock, process cleanup and concurrency. A diagnostic repetition is not a passing
CI run. Do not increase all timeouts or add retries to conceal the failure.
Regression checks should reproduce the causal sequence with isolated homes and
synthetic services. CI needs no Docker daemon or installed model client.

Workflow actions are pinned by commit; the token has read-only contents access.
CI does not publish packages, create releases or deploy anything. A passing run
does not verify Windows/Linux desktop packaging or real-world workload behavior.
