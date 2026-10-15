//! Inert executable-provider structure. Execution and resolved contract compatibility
//! remain adapter-owned semantic checks, described alongside this grammar.
use super::*;

pub(super) fn register(d: &mut IndexMap<&'static str, Schema>) {
    use ScalarType::Text;
    d.insert(
        "provider.document",
        owned(
            constrained(
                object(&[
                    ("version", "version", true),
                    ("provider", "text", true),
                    ("types", "contract.types", false),
                    ("backend", "provider.backend", true),
                    ("operations", "provider.operations", true),
                    ("read_access", "provider.read-access", false),
                ]),
                "provider-definition",
            ),
            "IMP001",
        ),
    );
    d.insert(
        "provider.backend",
        variants(
            "kind",
            &[
                ("process", "provider.backend.process"),
                ("session", "provider.backend.session"),
            ],
        ),
    );
    d.insert("provider.kind.process", choices(Text, &["process"]));
    d.insert("provider.kind.session", choices(Text, &["session"]));
    d.insert(
        "provider.backend.process",
        constrained(
            object(&[
                ("kind", "provider.kind.process", true),
                ("bin", "text", true),
            ]),
            "provider-process",
        ),
    );
    d.insert(
        "provider.backend.session",
        constrained(
            object(&[
                ("kind", "provider.kind.session", true),
                ("bin", "text", true),
                ("credentials", "provider.text-list", false),
            ]),
            "provider-session",
        ),
    );
    d.insert("provider.text-list", list("text"));
    d.insert("provider.operations", list("provider.operation"));
    d.insert(
        "provider.operation",
        constrained(
            object(&[
                ("path", "provider.text-list", true),
                ("summary", "text", false),
                ("parameters", "provider.parameters", false),
                ("argv", "provider.argv", true),
                ("result", "provider.result", true),
                ("success_codes", "provider.success-codes", false),
                ("streaming", "bool", false),
                ("control", "provider.control", false),
                ("finite_profile", "provider.finite-profile", false),
                ("source_profile", "provider.source-profile", false),
                ("fixed_arguments", "provider.fixed-arguments", false),
                ("exclusive", "provider.exclusive-list", false),
                ("byte_input", "provider.byte-input", false),
                ("recording", "provider.recording", false),
            ]),
            "provider-operation",
        ),
    );
    d.insert("provider.parameters", list("provider.parameter"));
    d.insert(
        "provider.parameter",
        hint(
            object(&[
                ("name", "text", true),
                ("type", "type", true),
                ("required", "bool", false),
            ]),
            "required defaults to true; types must resolve to materialized contracts.",
        ),
    );
    d.insert("provider.argv", list("provider.argument"));
    d.insert(
        "provider.argument",
        union(&["text", "provider.argument.parameter"]),
    );
    d.insert(
        "provider.argument.parameter",
        constrained(
            object(&[
                ("parameter", "text", true),
                ("prefix", "provider.text-list", false),
                ("encoding", "provider.encoding", false),
            ]),
            "provider-argv",
        ),
    );
    d.insert(
        "provider.encoding",
        choices(Text, &["scalar", "spread", "flag", "json"]),
    );
    d.insert(
        "provider.success-codes",
        hint(list("int"), "Defaults to [0]."),
    );
    d.insert(
        "provider.result",
        constrained(
            object(&[
                ("format", "provider.output-format", true),
                ("type", "type", false),
                ("byte_field", "provider.text-list", false),
            ]),
            "provider-result",
        ),
    );
    d.insert(
        "provider.output-format",
        choices(Text, &["json", "text", "bytes", "process", "record-bytes"]),
    );
    d.insert(
        "provider.control",
        constrained(
            object(&[
                ("input", "type", true),
                ("output", "type", true),
                ("output_policy", "provider.output-policy", false),
            ]),
            "provider-control",
        ),
    );
    d.insert(
        "provider.output-policy",
        choices(
            Text,
            &[
                "public",
                "private",
                "confidential-temporary",
                "confidential",
            ],
        ),
    );
    d.insert(
        "provider.read-access",
        constrained(
            object(&[
                ("scope", "provider.text-list", true),
                ("revalidate", "provider.text-list", true),
            ]),
            "provider-read-access",
        ),
    );
    d.insert("provider.recording", hint(object(&[
        ("type", "type", true),
        ("byte_field", "provider.text-list", true),
        ("max_bytes", "int", true),
        ("max_duration_ms", "int", true),
    ]), "Streaming session only. Captured materialized record type and a required unconstrained Bytes leaf. max_bytes is positive and at most the native acquisition budget; duration is 1–86400000 ms. Native storage supplies placement/policy. Ordered byte credits, an initial shell before readiness and a final shell plus exact End distinguish complete data from an acknowledged incomplete prefix. Physical join is required; no peer-selected paths or replay."));
    d.insert("provider.byte-input", hint(object(&[("parameter", "text", true),("max_bytes", "int", true)]), "Finite local process only; parameter names one required Bytes input without an argv mapping. max_bytes is positive and at most the native acquisition ceiling. Native entry captures the acknowledged exact source and scope; bounded stdin reads retain ownership and join cleanup. No implicit producer dependency or replay."));
    d.insert("provider.exclusive-list", list("provider.exclusive"));

    d.insert("provider.field-paths", list("provider.text-list"));

    d.insert("provider.exclusive", hint(object(&[("namespace", "text", true),("fields", "provider.field-paths", true)]), "Streaming session only; at most four distinct resources. namespace is a bounded literal physical-resource namespace shared by aliases. fields has 1–8 bounded static paths through required records ending at required Text parameters. Captured argument values form exact lexical resource keys; reservation is atomic and held until physical producer/controller join. This grants no invocation or read authority."));
    d.insert("provider.fixed-arguments", hint(named("datum"), "Defaults to an empty map. Captured typed constants for declared parameters, at most 256 entries, 65536 encoded bytes and 10000 decoding nodes. Validate exact native literals against captured contracts. Omit fixed fields from caller parameters and refuse overrides. Fixed native byte-input sources are forbidden. Import/reconstruction never rereads mutable configuration."));
    d.insert("provider.profile-options", named("provider.profile-option"));

    d.insert(
        "provider.profile-option",
        variants(
            "kind",
            &[
                ("maximum", "provider.profile-option.maximum"),
                ("backend-condition", "provider.profile-option.condition"),
            ],
        ),
    );

    d.insert(
        "provider.kind.maximum",
        choices(ScalarType::Text, &["maximum"]),
    );

    d.insert(
        "provider.kind.condition",
        choices(ScalarType::Text, &["backend-condition"]),
    );

    d.insert("provider.profile-option.maximum", hint(object(&[("kind", "provider.kind.maximum", true),("parameter", "text", true),("maximum_from", "provider.text-list", true)]), "maximum_from selects a positive Int through a captured fixed argument. The mapped optional Int caller parameter may only narrow that maximum."));

    d.insert("provider.profile-option.condition", hint(object(&[("kind", "provider.kind.condition", true),("parameter", "text", true)]), "An optional Text caller condition is separate from fixed configuration. The external backend must validate/compose its semantics as a further restriction; the engine promises no protocol-specific intersection."));

    d.insert("provider.source-profile", hint(object(&[("name", "text", true),("kind", "text", true),("default", "bool", false),("stop", "datum", true),("ready_wait_ms", "int", false),("tail_ms", "int", false),("control_ms", "int", false),("grace_ms", "int", false),("narrowing", "provider.profile-options", false)]), "Streaming session with typed controls and fixed required arguments only. name is bounded trimmed Text without control characters; kind is a bounded ASCII identifier. default is false. stop is a captured typed literal, never a live capability. ready_wait_ms defaults to 10000, control_ms/grace_ms to 5000, tail_ms to zero; positive waits and bounded tail cannot exceed native MAX_READY_WAIT. At most 16 distinct typed optional narrowing mappings. Lookup is bounded and target-aware; ambiguous/missing profiles refuse before dispatch."));
    d.insert("provider.profile-arguments", named("text"));

    d.insert("provider.finite-profile", hint(object(&[("name", "text", true),("kind", "text", true),("arguments", "provider.profile-arguments", true)]), "Finite operation only. Bounded name/kind identifiers and at most 32 canonical-to-declared parameter mappings; the map covers exactly all nonfixed caller parameters without duplicate targets. Frozen types, required flags, input limits, target admission and native byte/egress checks remain authoritative. No executable or credential lookup during resolution."));
}

pub(super) fn constraints() -> impl Iterator<Item = (&'static str, &'static str)> {
    [
        ("provider-definition", "Version 1; provider and operation/parameter names are bounded identifiers. Bounded strict YAML source and 1–256 distinct operations. Types default to an empty mapping and resolve within this document. Import captures declarations without launching the executable; operations are unsafe and require ordinary execution admission."),
        ("provider-process", "bin is an explicit executable path; relative paths resolve from the definition directory. Every parameter needs an argv mapping. Process targets use the captured environment execution boundary."),
        ("provider-session", "Local target only. credentials defaults to []; at most 16 distinct identifier slots (no hyphens), with material supplied through the native environment form. Operations require JSON results, literal argv and success_codes [0]. Versioned bounded duplex frames carry calls, readiness, events, controls and finite outcomes; import never starts a session."),
        ("provider-operation", "path has 1–16 identifier segments; summary defaults to empty (max 4096 bytes). parameters defaults to [] (max 256 distinct names). argv has fewer than 1024 members; success_codes defaults to [0] (1–256 unsigned 32-bit codes). streaming defaults to false and requires a session backend. Resolved input/output contracts must be materialized."),
        ("provider-argv", "A Text is one literal argument, never a shell program. Parameter mappings default prefix to [] and encoding to scalar. scalar accepts scalar types except Bytes; spread requires a list of those scalars; flag requires Bool and a nonempty prefix; json accepts materialized values. Optional missing parameters omit their mapping. Arguments/prefixes cannot contain NUL; final size/count budgets still apply."),
        ("provider-result", "json requires an explicit type. text defaults to Text and bytes to Bytes; overrides must retain that shape. process has the fixed ProcessOutput contract and forbids type. JSON Bytes fields use canonical base64 only where the captured contract declares Bytes. record-bytes requires a finite process operation, explicit type and byte_field selecting one required unconstrained Bytes leaf. stdout is a 4-byte big-endian header length (1–262144), a complete typed JSON record with an empty Bytes placeholder, then the exact payload to EOF. Successful exit and bounded headers are required; transient acquisition can retain nonexpanding bytes beyond the ordinary output cap. byte_field is forbidden for other formats."),
        ("provider-control", "Requires streaming on a session backend. input and output resolve to materialized contracts. output_policy defaults to public; native environment and input restrictions still apply. Controls belong to one admitted live connection; uncertain external outcomes must not be automatically replayed."),
        ("provider-read-access", "Opt-in managed-environment read domain. scope contains 1–16 distinct required Text parameters present on every operation. revalidate names one finite Bool operation whose only parameters are that scope. Only explicit native revalidation returning true opens captured reads. Expiry/revoke/material or binding changes close reads; retained bytes, re-enabling and new grants cannot revive old work. Import never probes."),
    ].into_iter()
}
