# Contributing to Wes

Wes is an experimental project. Reproducible bug reports, small fixes and
feedback from real workflows help shape it. For a substantial architectural
change, open an issue to explain the problem and proposed behavior before
starting a large pull request.

## Reporting an issue

Include the version or commit, operating system, and whether you used the CLI,
browser interface or WesDesk. Show the smallest command sequence that reproduces
the problem, what you expected, and what happened. Include relevant diagnostics
or screenshots when useful.

Use synthetic inputs or a local test service. Remove credentials, private
endpoints and sensitive result data from commands, logs and screenshots. For
streams or saved workspaces, mention whether the problem follows a restart,
reconnection or change of input.

For a feature request, describe the workflow you want to complete and where the
current behavior gets in the way. A concrete example is more useful than a list
of controls or syntax alone.

## Preparing a pull request

Start with [development setup](docs/development.md) and the
[testing guide](docs/testing.md). Use the pinned toolchain and lockfiles. Keep
the change focused; avoid unrelated formatting, generated build output and
local data homes.

Read the relevant implementation and tests before changing a contract. Keep
language declarations, runtime handling, transport and UI consumers consistent.
Fix the responsible shared layer rather than adding a workaround for one caller.
Preserve ownership, permission and revision checks. An unknown execution outcome
must not cause automatic replay of an external effect.

Describe the problem, intended behavior and acceptance cases in the pull
request. Add or update tests where the behavior needs coverage, and run the
relevant suites from the testing guide. State which checks passed and which you
could not run. Keep examples executable and update documentation when their
behavior changes.

Tests must use isolated data homes and synthetic services; they must not require
a contributor's credentials or operate their real external resources. AI-assisted
contributions follow the same expectations. Agents should also read
[AGENTS.md](AGENTS.md).
