# Declared service tones

These three service rows are synthetic. The typed output declares every status
and its tone, including statuses absent from an empty result. The sample requires
no provider, account, CI system or GUI build.

From the repository root:

```sh
cargo build -p wes --offline --target-dir target
python3 examples/declared-tones/check.py --binary target/debug/wes
```

The check copies the example into a temporary directory and uses its own data
home. It checks the actual retained JSON envelopes, then saves and reopens the
workspace after removing the type source. The captured metadata still contains
the original aliases, digests, domains and tones.

Submit `sample.wes` in a new workspace whose file base is this directory; its
first command loads `types.yaml`. Browser values carry a separate
optional `meta` envelope; row data contains only the declared fields. A consumer
can use `/e/f:status` to find the status descriptor for every row. Presentation
entries and rendering are implemented independently by the UI.

`ok`, `warn`, `bad`, `dim`, `meta` and `ink` are semantic theme tones. `ink` requests
neutral foreground. Type declarations do not accept `inherit` or raw colours.
The example's `titleAmbiguous` is a producer-supplied fact available to separate
presentation rules; the backend does not derive facts from neighbouring rows.
