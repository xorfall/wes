# View development toolchain

This directory was exported by Wes; no repository checkout is needed. It contains
the current SDK, independent compiler, locked npm dependencies and a matching
native contract validator. It does not install Node or npm, and it contains no
workspace results, credentials or grants.

Install Node.js 20+ and npm on the machine where you will build. From this
directory, explicitly install its dependencies:

```sh
npm ci --ignore-scripts --no-audit --no-fund
node wes-view-package.mjs describe
node wes-view-package.mjs init NEW_SOURCE_DIR
node wes-view-package.mjs build SOURCE_DIR OUTPUT.wes-view.json
node wes-view-package.mjs check OUTPUT.wes-view.json
```

The dependency installation can use the public npm registry; builds do not
download packages. The wrapper selects the included native validator without
changing PATH. The validator is specific to the export's OS/architecture; use a
matching kit or explicitly configure `WES_VIEW_CONTRACT_TOOL` when moving it.

Read Wes MCP `view_authoring` topics `types`, `sdk`, `theme` and `layout` before
coding. `view.json.input` must name a Record contract. A list goes in a named
field. Compile/check does not install a View or execute its sources. Install the
artifact using the existing package commands discovered through help, then
inspect a live instance with `view_render_status` on a connected UI. A drawing
acknowledgement does not establish visual correctness.
