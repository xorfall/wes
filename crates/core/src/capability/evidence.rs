//! Deterministic execution identity; presentation-only resource metadata is excluded.
//! Explicit fields keep identity independent of runtime Debug implementations.
use super::*;
use crate::{Primitive, RecordShape};
use std::fmt::{self, Debug};

impl ProviderDescription {
    pub fn revision_evidence(&self) -> String {
        format!("{:?}", ProviderEvidence(self))
    }
}
struct ProviderEvidence<'a>(&'a ProviderDescription);
struct CapabilityEvidence<'a>(&'a Capability);
struct ParameterEvidence<'a>(&'a Parameter);
struct ShapeEvidence<'a>(&'a Shape);
struct RuleEvidence<'a>(&'a Rule);
struct BasisEvidence<'a>(&'a RuleBasis);
struct DeclaredEvidence<'a>(&'a DeclaredRule);
struct SortEvidence<'a>(&'a Sort);
// Explicit ordered map/list encoding, independent of model Debug.
struct List<I>(I);
impl<I> Debug for List<I>
where
    I: Clone + IntoIterator,
    I::Item: Debug,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_list().entries(self.0.clone()).finish()
    }
}
struct Map<I>(I);
impl<I, K: Debug, V: Debug> Debug for Map<I>
where
    I: Clone + IntoIterator<Item = (K, V)>,
{
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_map().entries(self.0.clone()).finish()
    }
}
impl Debug for ProviderEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.0;
        f.debug_struct("ProviderDescription")
            .field("name", &p.name)
            .field(
                "capabilities",
                &Map(p
                    .capabilities
                    .iter()
                    .map(|(path, c)| (path, CapabilityEvidence(c)))),
            )
            .field("secrets", &p.secrets)
            .finish()
    }
}
impl Debug for CapabilityEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let c = self.0;
        let mut out = f.debug_struct("Capability");
        out.field("path", &c.path)
            .field("summary", &c.summary)
            .field(
                "parameters",
                &List(c.parameters.iter().map(ParameterEvidence)),
            )
            .field("result", &ShapeEvidence(&c.result))
            .field("rules", &List(c.rules.iter().map(DeclaredEvidence)))
            .field(
                "safety",
                &Word(match c.safety {
                    Safety::Safe => "Safe",
                    Safety::Unsafe => "Unsafe",
                }),
            )
            .field("provenance_arguments", &c.provenance_arguments)
            .field("streaming", &c.streaming)
            .finish()
    }
}
struct Word(&'static str);
impl Debug for Word {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0)
    }
}
impl Debug for ParameterEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let p = self.0;
        f.debug_struct("Parameter")
            .field("name", &p.name)
            .field("shape", &ShapeEvidence(&p.shape))
            .field("required", &p.required)
            .field("content", &p.content)
            .field("sort", &SortEvidence(&p.sort))
            .finish()
    }
}
impl Debug for SortEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Sort::Plain => f.write_str("Plain"),
            Sort::Selector(s) => f.debug_tuple("Selector").field(s).finish(),
            Sort::Resource(s) => f.debug_tuple("Resource").field(s).finish(),
            Sort::Fresh(s) => f.debug_tuple("Fresh").field(s).finish(),
        }
    }
}
impl Debug for ShapeEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Shape::Meta(t) => f.debug_tuple("Meta").field(t).finish(),
            Shape::Unknown => f.write_str("Unknown"),
            Shape::Primitive(p) => f
                .debug_tuple("Primitive")
                .field(&Word(match p {
                    Primitive::Text => "Text",
                    Primitive::Int => "Int",
                    Primitive::Decimal => "Decimal",
                    Primitive::Bool => "Bool",
                    Primitive::Instant => "Instant",
                    Primitive::Duration => "Duration",
                    Primitive::Interval => "Interval",
                    Primitive::Bytes => "Bytes",
                }))
                .finish(),
            Shape::List(s) => f.debug_tuple("List").field(&ShapeEvidence(s)).finish(),
            Shape::Option(s) => f.debug_tuple("Option").field(&ShapeEvidence(s)).finish(),
            Shape::Iter(s) => f.debug_tuple("Iter").field(&ShapeEvidence(s)).finish(),
            Shape::Dataset(s) => f.debug_tuple("Dataset").field(&ShapeEvidence(s)).finish(),
            Shape::Record(r) => f.debug_tuple("Record").field(&RecordEvidence(r)).finish(),
        }
    }
}
struct RecordEvidence<'a>(&'a RecordShape);
impl Debug for RecordEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // A Vec of references gives this formatting iterator a stable Clone contract.
        let fields: Vec<_> = self.0.fields().collect();
        f.debug_struct("RecordShape")
            .field("name", &self.0.name())
            .field(
                "fields",
                &Map(fields.iter().map(|(k, v)| (*k, ShapeEvidence(v)))),
            )
            .finish()
    }
}
impl Debug for DeclaredEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DeclaredRule")
            .field("rule", &RuleEvidence(&self.0.rule))
            .field("basis", &BasisEvidence(&self.0.basis))
            .finish()
    }
}
impl Debug for RuleEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            Rule::MutuallyExclusive(keys) => {
                f.debug_tuple("MutuallyExclusive").field(keys).finish()
            }
            Rule::Requires { key, needs } => f
                .debug_struct("Requires")
                .field("key", key)
                .field("needs", needs)
                .finish(),
            Rule::OneOf { key, values } => f
                .debug_struct("OneOf")
                .field("key", key)
                .field("values", values)
                .finish(),
            Rule::ProvenanceFact {
                key,
                fact,
                expected,
            } => f
                .debug_struct("ProvenanceFact")
                .field("key", key)
                .field("fact", fact)
                .field("expected", expected)
                .finish(),
        }
    }
}
impl Debug for BasisEvidence<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.0 {
            RuleBasis::Documented { note } => {
                f.debug_struct("Documented").field("note", note).finish()
            }
            RuleBasis::Inferred { reason } => {
                f.debug_struct("Inferred").field("reason", reason).finish()
            }
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn execution_identity_ignores_resource_presentation() {
        let capability = Capability::new(["read"], Shape::Unknown, Safety::Safe);
        let make = |c| ProviderDescription::new("qa", [c], vec![]).unwrap();
        let expected = r#"ProviderDescription { name: "qa", capabilities: {["read"]: Capability { path: ["read"], summary: "", parameters: [], result: Unknown, rules: [], safety: Safe, provenance_arguments: {}, streaming: false }}, secrets: [] }"#;
        assert_eq!(make(capability.clone()).revision_evidence(), expected);
        let mut displayed = capability.clone();
        displayed.resources = Some(ResourceProjection {
            registry: "resource".into(),
            rows: "rows".into(),
            key: "id".into(),
            label: "name".into(),
            detail: "state".into(),
            observed_at: "at".into(),
        });
        assert_eq!(make(displayed.clone()).revision_evidence(), expected);
        for changed in [
            Capability {
                safety: Safety::Unsafe,
                ..capability.clone()
            },
            Capability {
                result: Shape::Primitive(Primitive::Int),
                ..capability.clone()
            },
            Capability {
                streaming: true,
                ..capability.clone()
            },
            Capability {
                parameters: vec![Parameter::new("required", Shape::Unknown, true)],
                ..capability
            },
        ] {
            assert_ne!(make(changed).revision_evidence(), expected);
        }
    }
    #[test]
    fn execution_identity_covers_nested_contract_variants() {
        let mut c = Capability::new(["read"], Shape::Unknown, Safety::Unsafe);
        c.result = Shape::Record(
            RecordShape::new(
                "response",
                [
                    (
                        "list".into(),
                        Shape::List(Box::new(Shape::Primitive(Primitive::Text))),
                    ),
                    (
                        "option".into(),
                        Shape::Option(Box::new(Shape::Primitive(Primitive::Int))),
                    ),
                    (
                        "iter".into(),
                        Shape::Iter(Box::new(Shape::Primitive(Primitive::Decimal))),
                    ),
                    ("bool".into(), Shape::Primitive(Primitive::Bool)),
                    ("instant".into(), Shape::Primitive(Primitive::Instant)),
                    ("duration".into(), Shape::Primitive(Primitive::Duration)),
                    ("bytes".into(), Shape::Primitive(Primitive::Bytes)),
                ],
            )
            .unwrap(),
        );
        c.parameters = vec![
            Parameter::new("plain", Shape::Unknown, false),
            Parameter::new("selector", Shape::Unknown, true).selecting("registry"),
            Parameter::new("fresh", Shape::Unknown, true).naming("namespace"),
        ];
        c.summary = "quoted \" and newline\n and Türkçe".into();
        c.rules = vec![
            DeclaredRule {
                rule: Rule::MutuallyExclusive(BTreeSet::from(["a".into(), "b".into()])),
                basis: RuleBasis::Documented { note: None },
            },
            DeclaredRule {
                rule: Rule::Requires {
                    key: "a".into(),
                    needs: "b".into(),
                },
                basis: RuleBasis::Inferred {
                    reason: "qa".into(),
                },
            },
            DeclaredRule {
                rule: Rule::OneOf {
                    key: "a".into(),
                    values: BTreeSet::from(["a".into()]),
                },
                basis: RuleBasis::Documented {
                    note: Some("qa".into()),
                },
            },
            DeclaredRule {
                rule: Rule::ProvenanceFact {
                    key: "a".into(),
                    fact: "b".into(),
                    expected: "c".into(),
                },
                basis: RuleBasis::Documented { note: None },
            },
        ];
        let description = ProviderDescription::new("fixture", [c], vec!["key".into()]).unwrap();
        // Explicit encoding covers nested execution semantics.
        let expected = r##"ProviderDescription { name: "fixture", capabilities: {["read"]: Capability { path: ["read"], summary: "quoted \" and newline\n and Türkçe", parameters: [Parameter { name: "plain", shape: Unknown, required: false, content: None, sort: Plain }, Parameter { name: "selector", shape: Unknown, required: true, content: None, sort: Selector("registry") }, Parameter { name: "fresh", shape: Unknown, required: true, content: None, sort: Fresh("namespace") }], result: Record(RecordShape { name: "response", fields: {"list": List(Primitive(Text)), "option": Option(Primitive(Int)), "iter": Iter(Primitive(Decimal)), "bool": Primitive(Bool), "instant": Primitive(Instant), "duration": Primitive(Duration), "bytes": Primitive(Bytes)} }), rules: [DeclaredRule { rule: MutuallyExclusive({"a", "b"}), basis: Documented { note: None } }, DeclaredRule { rule: Requires { key: "a", needs: "b" }, basis: Inferred { reason: "qa" } }, DeclaredRule { rule: OneOf { key: "a", values: {"a"} }, basis: Documented { note: Some("qa") } }, DeclaredRule { rule: ProvenanceFact { key: "a", fact: "b", expected: "c" }, basis: Documented { note: None } }], safety: Unsafe, provenance_arguments: {}, streaming: false }}, secrets: ["key"] }"##;
        assert_eq!(description.revision_evidence(), expected);
        assert_eq!(
            description
                .with_information(crate::Data::Text("inert note".into()))
                .revision_evidence(),
            expected
        );
    }
}
