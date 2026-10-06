# Direct OpenAPI import

Import supported OpenAPI 3.0/3.1 JSON/YAML without creating an intermediate file:

```wes
:import openapi file:api.yaml endpoint:"http://localhost:8080" as:orders
```

`url:` can replace `file:`. The endpoint is explicit; documented servers do not
select the destination. Import converts and validates the contract, without
calling its operations or granting credentials. Unsupported constructs reject
the import; use `:describe` and `/spec` when the contract needs inspection or edits.

The offline check creates a synthetic API on loopback. It exercises JSON and
YAML, typed responses, environment packages and replay after the source and
usable converter are gone. No Docker, credentials or model client is needed.

Build the CLI and extractor as described in [testing](../../docs/testing.md), then:

```sh
python3 examples/openapi-import/check.py
```
