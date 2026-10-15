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
            ]),
            "provider-result",
        ),
    );
    d.insert(
        "provider.output-format",
        choices(Text, &["json", "text", "bytes", "process"]),
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
}

pub(super) fn constraints() -> impl Iterator<Item = (&'static str, &'static str)> {
    [
        ("provider-definition", "Version 1; provider and operation/parameter names are bounded identifiers. Bounded strict YAML source and 1–256 distinct operations. Types default to an empty mapping and resolve within this document. Import captures declarations without launching the executable; operations are unsafe and require ordinary execution admission."),
        ("provider-process", "bin is an explicit executable path; relative paths resolve from the definition directory. Every parameter needs an argv mapping. Process targets use the captured environment execution boundary."),
        ("provider-session", "Local target only. credentials defaults to []; at most 16 distinct identifier slots (no hyphens), with material supplied through the native environment form. Operations require JSON results, literal argv and success_codes [0]. Versioned bounded duplex frames carry calls, readiness, events, controls and finite outcomes; import never starts a session."),
        ("provider-operation", "path has 1–16 identifier segments; summary defaults to empty (max 4096 bytes). parameters defaults to [] (max 256 distinct names). argv has fewer than 1024 members; success_codes defaults to [0] (1–256 unsigned 32-bit codes). streaming defaults to false and requires a session backend. Resolved input/output contracts must be materialized."),
        ("provider-argv", "A Text is one literal argument, never a shell program. Parameter mappings default prefix to [] and encoding to scalar. scalar accepts scalar types except Bytes; spread requires a list of those scalars; flag requires Bool and a nonempty prefix; json accepts materialized values. Optional missing parameters omit their mapping. Arguments/prefixes cannot contain NUL; final size/count budgets still apply."),
        ("provider-result", "json requires an explicit type. text defaults to Text and bytes to Bytes; overrides must retain that shape. process has the fixed ProcessOutput contract and forbids type. JSON Bytes fields use canonical base64 only where the captured contract declares Bytes."),
        ("provider-control", "Requires streaming on a session backend. input and output resolve to materialized contracts. output_policy defaults to public; native environment and input restrictions still apply. Controls belong to one admitted live connection; uncertain external outcomes must not be automatically replayed."),
        ("provider-read-access", "Opt-in managed-environment read domain. scope contains 1–16 distinct required Text parameters present on every operation. revalidate names one finite Bool operation whose only parameters are that scope. Only explicit native revalidation returning true opens captured reads. Expiry/revoke/material or binding changes close reads; retained bytes, re-enabling and new grants cannot revive old work. Import never probes."),
    ].into_iter()
}
