# From an API call to a shared workspace

This is a small synthetic example of the main Wes workflow: convert an OpenAPI
contract, import its operation, send an HTTP request, and continue with the
response. It uses the actual extractor, engine and interface.

The fixture returns three fixed per-service request rates in a
[Prometheus instant-vector response](https://prometheus.io/docs/prometheus/latest/querying/api/#instant-vectors).
It is **not a Prometheus server or a PromQL evaluator**. The OpenAPI file describes
only the successful vector response used here; it is not a complete Prometheus
contract. No Docker daemon, real monitoring system or API account is required.

## Build and check

From the checkout root, after installing the prerequisites in the
[examples index](../README.md):

```sh
npm ci
cargo build -p wes --locked
(cd tools/describe && go build -o wes-extract ./cmd/extract)
python3 examples/prometheus-workspace/check.py
```

The check owns a loopback server and a temporary data home. It verifies the
generated operation, the query parameters, HTTP body validation, the three
derived rows, and reading the kept rows after restart without another HTTP call.
It also checks that conversion and import do not send a query.

## Open the workspace

```sh
npm run build
python3 examples/prometheus-workspace/run.py
```

Open the printed loopback URL. The launcher prepares the example in an isolated
temporary data home. Ctrl-C stops both servers and removes that home. Nothing
is imported into your regular workspace. `run.py --check` verifies the same live
setup and stops immediately, without starting an AI client.

The four source files correspond to the four steps. Run them in order, waiting
for each step to finish before starting the next:

1. [01-describe.wes](01-describe.wes) converts the small OpenAPI contract into an
   editable draft and exports a new `prom.json` contract. This does not call the
   API. The draft's result is private and memory-only; the exported file is
   separate from result retention.
2. [02-import.wes](02-import.wes) imports the operation as `prom`, with an explicit
   endpoint. The launcher substitutes its random loopback port for `19092`.
3. [03-query.wes](03-query.wes) calls `prom query` once and names its typed HTTP
   response `$response`.
4. [04-rates.wes](04-rates.wes) reads `$response.body.data.result` and creates
   `$rates`. The sample pairs contain numeric readings as strings; `decimal`
   converts them for further calculations.

| service | requestsPerSecond |
| --- | ---: |
| orders-api | 12.5 |
| payments-api | 3.25 |
| billing-worker | 0.8 |

You can inspect the original response as JSON and the derived rows as a table.
Neither inspection nor the calculation sends another request. Repeat the query
explicitly when you want another response.

To run the files yourself in an existing scratch interface instead, start
`python3 examples/prometheus-workspace/server.py`, make the source OpenAPI file
available in the interface's working directory, and execute the four commands
there. The API library settings must point to the built `wes-extract` executable.
Use a fresh output path for conversion; export does not overwrite an existing
file. The prepared launcher avoids these setup steps.

## Continue with an AI agent

This part is optional. Install and sign in to Claude Code or Codex separately;
the synthetic check does not start a model or use credentials.

In the Wes command input, run:

```text
/rsplit xterm
```

Click the terminal and launch `claude` or `codex` normally. Wes attaches its MCP
tools to these installed clients. Ask, for example:

> Use the Wes workspace tools to read `$rates` and its type. Create
> `$rate_summary` with the total request rate and busiest service, derived from
> `$rates`. Run it and verify the result. Do not rerun HTTP or change existing
> work. Explain briefly in English.

The expected total is **16.55 requests/s**, and the busiest service is
**orders-api** at **12.5 requests/s**. The agent's new command and result appear
in the session beside the terminal. You can inspect that result and use
`$rate_summary` in another calculation yourself.

Agent responses and generated source can vary. Check the resulting data and
commands rather than assuming a successful chat response means the work ran.
Closing an agent does not undo work it already executed in Wes.
