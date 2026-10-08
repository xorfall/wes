# Confidential evidence storage

Some results need to remain confidential without disappearing when Wes closes.
Examples include a captured service response used to investigate an incident,
recorded events and the analysis derived from them. Previously, `output: private`
made all of these memory-only. Allowing that same declaration to write to disk
would break its promise. Confidential disk storage therefore has an explicit,
separate declaration.

## Output policy

An environment import's `bind.output` classifies future acquisitions:

| Declaration | Confidential | Permitted residence | Keep |
| --- | --- | --- | --- |
| `public` (default) | No | Ordinary storage | Existing automatic/explicit rules |
| `private` | Yes | Memory only | Refused |
| `confidential-temporary` | Yes | Memory or encrypted temporary storage | Refused |
| `confidential` | Yes | Memory or encrypted storage, eligible for retention | Explicit only |

Residence is a ceiling, not a retention receipt. Naming a result with `> name`
does not Keep it. Confidential results are never automatically kept, including
small results below the ordinary auto-Keep threshold. Use the result's Keep
action to retain an eligible result; its normal persistence receipt still applies.
Temporary results remain subject to ordinary live-storage eviction and cleanup.
Temporary does not promise erasure at an exact time or secure deletion of disk blocks.

For example, change the binding of a reviewed environment package:

```yaml
version: 1
targets:
  local: {kind: local}
environments:
  incident:
    imports:
      service:
        source: {kind: spec, file: service.json}
        bind:
          target: local
          endpoint: http://127.0.0.1:8080
          output: confidential
```

Here `service.json` is a native Wes API descriptor. Planning and applying this
package does not call the service. Use the installed operation as usual, then
derive values from the response:

```wes
:env plan file:environment.yaml > plan
:env apply $plan
:env use "incident"
service read > response
:calc pure { return $response.body; } > body
```

`read` must be an operation declared by that descriptor. Before, a `private`
binding made both results memory-only and Keep failed. With `confidential` and
an encrypted home, both results can be displayed locally and explicitly kept.
Reopening a kept result reads its original evidence; it does not call the service.
Changing a binding never relabels an already acquired result.

Pure transformations join the policies of their inputs. Confidentiality propagates
and the strictest residence wins: memory-only plus retainable stays memory-only;
temporary plus retainable stays temporary. Selecting a field does not remove its
root's restrictions. Credentials, field names and payload text do not implicitly
classify output.

## Starting an encrypted home

The initial key provider is a separately provisioned file containing exactly
32 random bytes. Keep it outside the data home. On Unix, only its owner may have
file permissions. The final path component must be a regular file, not a symlink.
For example, provision a new key and a new home on Unix:

```sh
umask 077
openssl rand -out incident-storage.key 32
wes --home ./incident-home --storage-key-file ./incident-storage.key \
  --serve 8099 --site gui/dist
```

Use an unused key filename: the provisioning command overwrites that file if it
already exists. `gui/dist` must have been built first. Supply the same key on
every reopen. Wes never creates, replaces or guesses a missing key. A wrong key,
missing key, foreign home identity or invalid authentication tag refuses access;
there is no plaintext fallback. Keep a separately protected backup of the key
if the evidence must remain recoverable. Losing it makes that evidence unreadable.

The storage mode is selected when the result and Dataset directories are first
initialized. An ordinary home cannot be silently converted by adding this flag,
and removing it cannot downgrade an encrypted home. This delivery has no migration,
key rotation or in-process lock command. To lock, shut down the owning runtime
and its sessions; reopening requires the original key. Removing the key file
while a runtime is running does not revoke its already loaded key.

This setup is available through the CLI/browser host and `RuntimeOptions` for
embedding. The desktop launcher does not yet offer a key-selection interface.
Windows uses the same encryption format, but this change does not add Windows
ACL verification for the key or data home.

## Storage and authorization boundaries

The core policy is independent of execution grants. The engine admits storage
and retention; adapters own authenticated physical I/O; the application supplies
the startup key. Keys never enter workspace source, result transport or history.

In an encrypted home, all result envelopes and Dataset schemas, segments,
indexes, manifests, checkpoints and catalog frames are encrypted, including
ordinary results. XChaCha20-Poly1305 uses independent random nonces and binds
each object to the home identity, storage domain, object identity and format.
Plaintext encoding is operation-local memory, never a staging file. Physical
encryption overhead counts toward disk budgets. Catalog recovery preserves
existing transaction and reconciliation rules: uncertainty never automatically
replays an external operation. Encryption does not provide an independent
anti-rollback witness against replacement of the entire home with an older copy.

This is not whole-home encryption. User-authored commands, workspace definitions,
API contracts and ordinary history metadata retain their existing storage format.
Do not place secrets in source literals or filenames expecting this feature to
hide them. Object sizes, identifiers and filesystem activity remain observable.
It also does not protect plaintext in a running process, screenshots, manually
copied visible data or a compromised host.

Keep preserves evidence; it does not declassify it or authorize export. Local
workspace display is allowed under existing ownership and revision checks.
Terminal, MCP/agent and file result-export paths refuse confidential payloads.
There is no destination-grant or declassification feature in this delivery.
Interactive provider conversations also remain unsupported for confidential output.
Ordinary provider execution and its explicit authority checks are otherwise unchanged.

Acquisition permission and retained-read authorization remain separate. Revoking
a service credential does not erase captured evidence. Existing Dataset withdrawal
and exact read-gate checks still apply to stored and derived results.

## Recording and analysis

Mandatory EventLog recording checks encrypted-storage availability before entering
the producer. Without it, admission fails instead of capturing plaintext and
trying to encrypt later. Optional finite-result publication can fail after a call
has completed; that failure does not undo the call or grant retention.

Confidential recording, paging and derived Dataset analysis preserve their policy
through checkpoints and reopening. Memory-only output cannot enter a disk sink.
An explicit recording or durable analysis command can protect eligible evidence
through its existing roots; this is separate from automatic finite-result Keep.
Those internal roots also respect the residence ceiling: temporary confidential
EventLogs have temporary roots. They are supported, but a durable `:scan sink:dataset`
currently requires a retainable captured source and refuses a temporary-only input;
use an in-memory sink for that input. No read, Keep or reconciliation action
restarts a producer.

See [Testing](testing.md#confidential-storage) for synthetic acceptance cases.
