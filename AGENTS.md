# Contributing with an agent

Read the relevant source and tests before changing a contract. Keep an implementation
coherent across language declarations, runtime handling, transport and UI consumers.
Describe the intended behavior and acceptance cases with the change. Use isolated
homes and synthetic services for tests; never require a contributor's credentials.

Preserve ownership, permission and revision checks. An unknown execution outcome
must not cause automatic replay of external effects. Fix the responsible shared
layer instead of adding a caller-specific workaround. Historical compatibility is
not required at this development stage; malformed inputs must still fail safely.

Run the suites relevant to the completed change using docs/testing.md. Avoid
unrelated rewrites or generated output in commits. Keep examples executable and
update the documentation when behavior changes. Do not publish a release, contact
live services, or operate real external resources without explicit authorization.
