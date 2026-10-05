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

`gui` is the React/TypeScript client. `tools/describe` converts API documentation
into editable contracts. OpenAPI JSON can be converted without a model; prose
extraction requires explicitly configured model access. Tests use saved synthetic
answers instead of a live model. `tools/view-dev` is the isolated development
harness for result views; `tools/view-package` builds independent React packages.

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
