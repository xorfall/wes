# Local recovery additions to avt 0.18.0

This directory contains the Apache-2.0 source from the published
`avt` 0.18.0 crate (asciinema/avt). Its original license remains in LICENSE.
It is a library dependency, not a second terminal driver or a process launcher.

Wes needs a bounded canonical screen for reconnecting an existing PTY. The public
upstream API does not expose primary history while the alternate screen is active,
soft-wrap markers or an adjustable history limit. Local additions expose those
properties and keep collection/resize in the emulator that owns them. Cells also
retain a fixed bounded zero-width suffix, so combining marks do not advance the
cursor or disappear from recovery output. No terminal query responder is added.
Pending-wrap cursor restoration reprints the complete cell, including its suffix,
for both single-column cells and wide-character heads at the right margin.

The application recovery tests compare independently authored output and native
snapshots with the shipped xterm parser. Keep these additions small and explicit
when updating the upstream source; do not change this crate to add Wes session,
permission, provider or filesystem behavior.
