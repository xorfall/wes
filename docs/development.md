# Development

The Rust workspace contains eight crates:

| Crate | Responsibility |
| --- | --- |
| budgets | Shared operating limits and policy validation |
| core | Values, types and provider contracts |
| views | View manifests, contracts and native package validation |
| language | Parsing, command vocabulary and calculation syntax |
| engine | Planning, dependencies, execution and workspace state |
| adapters | Persistence, HTTP/process/container integrations and codecs |
| app | CLI, web transport, terminal and agent interfaces |
| desktop | Native Tauri shell hosting the application |

`gui` is the React/TypeScript client. `tools/describe` deterministically converts
supported OpenAPI 3.0/3.1 JSON/YAML into native Wes contracts; it does not use a
model, interpret prose/HTML or fetch external references. `tools/view-dev` is the
isolated development harness for result views; `tools/view-package` builds
independent React packages.

## API imports

Use `:import spec` for a ready Wes JSON descriptor, or import supported OpenAPI
directly:

```wes
:import openapi file:api.yaml endpoint:"http://localhost:8080" as:orders
```

Both importers require exactly one of `file:`/`url:` and an explicit `endpoint:`.
Importer help and capture admission use the same registered metadata; required
arguments are checked before input I/O. Environment packages also accept
`source.kind: openapi`, with their destination supplied by `bind.endpoint`.
Documented servers never select an execution destination. For OpenAPI sources,
pin the captured environment revision. OpenAPI `source.sha256` is currently
rejected rather than compared against a converted artifact; a ready Wes
descriptor can still use `source.sha256` to pin its input bytes.

Direct OpenAPI import invokes the same bundled/configured `wes-extract` converter
as `:describe`, with partial conversion disabled. Unsupported documents fail
without installing a provider; use `:describe` and `/spec` to inspect or revise
them. Successful capture retains original UTF-8 input, its digest, the converted
Wes descriptor and its digest in a versioned recipe. Replay and environment locks
use that descriptor without source reads or compiler execution. Changes to the
source require an explicit new import; replacement still requires `replace:true`.
Imports never call API operations, grant credentials or enable external effects.
Authentication choices, credential grants, output policy and execution targets
remain governed by the existing environment and HTTP adapter rules.

Import diagnostics expose producer-authored safe causes. `IMP007` identifies an
unresolved authentication choice, `IMP008` an unavailable declared credential,
`IMP009` query-based credential leakage risk, and `IMP010` undocumented
authentication. `IMP002` remains an advisory whose detailed document/adapter text
is not automatically public. Source access does not authorize exporting inferred
private values or advisory text. MCP
`validate` still checks syntax/admission, not eventual import or runtime success.

## Agent discovery and View development

MCP `help` exposes required importer arguments and provider invocation examples.
`view_authoring` describes the supported type syntax, theme, SDK and layout
contracts. A View's input must be a named Record; wrap a list in a named field.
An `Instant` needs seconds and an explicit `Z` or numeric UTC offset. Wes does
not infer UTC for an API timestamp without a time zone.

MCP `view_toolchain` reports tools on the Wes backend host. Discovery does not
install dependencies or run discovered programs. To develop Views without a
repository checkout, explicitly export the embedded SDK, compiler and matching
native validator to a new directory:

```sh
wes --export-view-toolchain NEW_DIRECTORY
cd NEW_DIRECTORY
npm ci --ignore-scripts --no-audit --no-fund
node wes-view-package.mjs init SOURCE_DIRECTORY
node wes-view-package.mjs build SOURCE_DIRECTORY OUTPUT.wes-view.json
node wes-view-package.mjs check OUTPUT.wes-view.json
```

Node.js 20+ and npm are needed for this independent toolchain. The export refuses
an existing destination and includes no workspace data or credentials. Use a kit
for the machine's OS/architecture; the wrapper selects its validator without
requiring it on PATH. The desktop package includes that validator. Source builds
need `wes-view-build` beside `wes` before exporting.

MCP `view_render_status` reads delivery receipts for an existing live instance
on the requesting terminal's UI. It does not mount a View, start observation or
run a provider. Receipts must match the workspace generation, instance,
definition and acknowledged input revision; these are rechecked after the UI
reply. No connected UI or mount means unverified, not successful rendering.
A drawing acknowledgement verifies delivery only: visual correctness, linked
input freshness, nested members and other windows are not verified.

Terminal/UI requests use typed operations with pane ownership and generation
checks. Completion notices are producer metadata, independent of result data.
An agent with source access can learn that `:describe` completed and whether it
requested a file export; that does not grant access to its private draft. Share
a draft explicitly through `/spec`. `spec_list` lists shared drafts only.

## Building

Use the pinned Rust toolchain and dependency lockfiles. Node.js 22+ and Go 1.26+
are needed for the client and extraction helper. For local client development,
start the engine on port 8099 and run `npm run dev` in `gui`; its proxy preserves
the server's same-origin boundary. `WES_ENGINE` selects another loopback backend.

For a native build on macOS, install Xcode command-line tools and the Tauri CLI
major version 2. Build the extraction helper first, then:

```sh
npm ci
(cd tools/describe && go build -o wes-extract ./cmd/extract)
npm run build
(cd crates/desktop && cargo tauri build --bundles app)
```

The resulting bundle is `target/release/bundle/macos/WesDesk.app`. This builds a
local development bundle, not a notarized release. Linux and Windows desktop
packaging have not been verified by this checkout's acceptance run.

Public APIs between layers live in the source contracts: the language vocabulary,
engine ports, app protocol, and `gui/src/protocol.ts`. Result views implement the
common module interface under `gui/src/value-views`; rendering budgets belong to
the presentation host. Theme tokens and role rules are maintained directly in
`gui/src/surface/tokens.css` and `roles.css` and checked by their tests.
