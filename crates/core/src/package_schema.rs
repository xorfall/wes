//! Authoritative structural grammar of inert YAML declarations.
//!
//! Both package validation and editor publication consume these objects. Resolution
//! (inheritance, references and contract compatibility) remains in the domain parsers.
use crate::contracts::{PackageNode as Node, PackageScalarKind as Scalar};
use indexmap::IndexMap;
use std::sync::LazyLock;

#[derive(Clone, Copy, Debug)]
pub enum ScalarType {
    Text,
    Int,
    Decimal,
    Bool,
    TypeExpression,
}
#[derive(Clone, Debug)]
pub struct Field {
    pub schema: &'static str,
    pub required: bool,
}
#[derive(Clone, Debug)]
pub struct Exclusive {
    pub fields: Vec<&'static str>,
    pub min: usize,
    pub max: usize,
}
#[derive(Clone, Debug)]
pub struct Condition {
    pub field: &'static str,
    pub values: Vec<&'static str>,
    pub prefix: Option<&'static str>,
    pub allowed: Vec<&'static str>,
    pub required: Vec<&'static str>,
}
#[derive(Clone, Debug)]
pub enum Kind {
    Any,
    Scalar {
        scalar: ScalarType,
        choices: Vec<&'static str>,
    },
    Object {
        fields: IndexMap<&'static str, Field>,
        exclusive: Vec<Exclusive>,
        conditions: Vec<Condition>,
    },
    Map {
        values: &'static str,
    },
    List {
        items: &'static str,
    },
    Union {
        variants: Vec<&'static str>,
    },
    Discriminated {
        field: &'static str,
        variants: IndexMap<&'static str, &'static str>,
    },
}
#[derive(Clone, Debug)]
pub struct Schema {
    pub kind: Kind,
    pub hint: Option<&'static str>,
    pub constraints: Vec<&'static str>,
    pub diagnostic: Option<&'static str>,
}
#[derive(Clone, Debug)]
pub struct Registry {
    pub roots: IndexMap<&'static str, &'static str>,
    pub definitions: IndexMap<&'static str, Schema>,
    pub constraints: IndexMap<&'static str, &'static str>,
}
#[derive(Clone, Debug)]
pub struct Issue {
    pub code: &'static str,
    pub message: String,
}

pub fn declarations() -> &'static Registry {
    static REGISTRY: LazyLock<Registry> = LazyLock::new(build);
    &REGISTRY
}
impl Registry {
    pub fn validate(&self, schema: &str, value: &Node) -> Result<(), Issue> {
        self.check(schema, value, "TYP002", 0)
    }
    fn check(
        &self,
        id: &str,
        value: &Node,
        owner: &'static str,
        depth: usize,
    ) -> Result<(), Issue> {
        let schema = self
            .definitions
            .get(id)
            .expect("registered declaration reference");
        let owner = schema.diagnostic.unwrap_or(owner);
        let error = |message: &str| Issue {
            code: owner,
            message: message.into(),
        };
        if depth > 128 {
            return Err(error("declaration nesting exceeds limit"));
        }
        let child = |id: &str, value: &Node| self.check(id, value, owner, depth + 1);
        match &schema.kind {
            Kind::Any => (),
            Kind::Scalar { scalar, choices } => {
                let Node::Scalar(kind, raw) = value else {
                    return Err(error("expected scalar declaration value"));
                };
                let matches = matches!(
                    (scalar, kind),
                    (ScalarType::Text | ScalarType::TypeExpression, Scalar::Text)
                        | (ScalarType::Int, Scalar::Int)
                        | (ScalarType::Decimal, Scalar::Int | Scalar::Decimal)
                        | (ScalarType::Bool, Scalar::Bool)
                );
                if !matches {
                    return Err(error("declaration value has the wrong scalar type"));
                }
                let chosen = choices.is_empty()
                    || choices.iter().any(|choice| {
                        if matches!(scalar, ScalarType::Int) {
                            raw.parse::<i64>()
                                .ok()
                                .zip(choice.parse::<i64>().ok())
                                .is_some_and(|(a, b)| a == b)
                        } else if matches!(scalar, ScalarType::Bool) {
                            raw.eq_ignore_ascii_case(choice)
                        } else {
                            raw == choice
                        }
                    });
                if !chosen {
                    return Err(error("unsupported declaration value"));
                }
            }
            Kind::Object {
                fields,
                exclusive,
                conditions,
            } => {
                let Node::Mapping(map) = value else {
                    return Err(error("expected declaration mapping"));
                };
                if map.keys().any(|key| !fields.contains_key(key.as_str())) {
                    return Err(error("unknown declaration field"));
                }
                for (key, field) in fields {
                    match map.get(*key) {
                        Some(value) => child(field.schema, value)?,
                        None if field.required => {
                            return Err(error(&format!("missing declaration field: {key}")));
                        }
                        None => (),
                    }
                }
                for group in exclusive {
                    let count = group
                        .fields
                        .iter()
                        .filter(|key| map.contains_key(**key))
                        .count();
                    if count < group.min || count > group.max {
                        return Err(error("declaration requires an exclusive field choice"));
                    }
                }
                for condition in conditions {
                    let written = map.get(condition.field).and_then(|n| n.text().ok());
                    if written.is_some_and(|v| {
                        condition.values.contains(&v)
                            || condition.prefix.is_some_and(|p| v.starts_with(p))
                    }) {
                        if map
                            .keys()
                            .any(|key| !condition.allowed.contains(&key.as_str()))
                        {
                            return Err(error("field is not allowed for this declaration variant"));
                        }
                        if condition.required.iter().any(|key| !map.contains_key(*key)) {
                            return Err(error("missing field required by declaration variant"));
                        }
                    }
                }
            }
            Kind::Map { values } => {
                let Node::Mapping(map) = value else {
                    return Err(error("expected named declaration mapping"));
                };
                for value in map.values() {
                    child(values, value)?;
                }
            }
            Kind::List { items } => {
                let Node::Sequence(list) = value else {
                    return Err(error("expected declaration list"));
                };
                for value in list {
                    child(items, value)?;
                }
            }
            Kind::Union { variants } => {
                if !variants.iter().any(|variant| child(variant, value).is_ok()) {
                    return Err(error("value does not match an accepted declaration form"));
                }
            }
            Kind::Discriminated { field, variants } => {
                let Node::Mapping(map) = value else {
                    return Err(error("expected declaration mapping"));
                };
                let tag = map
                    .get(*field)
                    .and_then(|n| n.text().ok())
                    .ok_or_else(|| error("missing or invalid declaration discriminator"))?;
                child(
                    variants
                        .get(tag)
                        .ok_or_else(|| error("unsupported declaration discriminator"))?,
                    value,
                )?;
            }
        }
        Ok(())
    }
}

fn schema(kind: Kind) -> Schema {
    Schema {
        kind,
        hint: None,
        constraints: vec![],
        diagnostic: None,
    }
}
fn scalar(kind: ScalarType) -> Schema {
    schema(Kind::Scalar {
        scalar: kind,
        choices: if matches!(kind, ScalarType::Bool) {
            vec!["true", "false"]
        } else {
            vec![]
        },
    })
}
fn choices(kind: ScalarType, values: &[&'static str]) -> Schema {
    schema(Kind::Scalar {
        scalar: kind,
        choices: values.to_vec(),
    })
}
fn object(fields: &[(&'static str, &'static str, bool)]) -> Schema {
    schema(Kind::Object {
        fields: fields
            .iter()
            .map(|(key, schema, required)| {
                (
                    *key,
                    Field {
                        schema,
                        required: *required,
                    },
                )
            })
            .collect(),
        exclusive: vec![],
        conditions: vec![],
    })
}
fn named(values: &'static str) -> Schema {
    schema(Kind::Map { values })
}
fn list(items: &'static str) -> Schema {
    schema(Kind::List { items })
}
fn union(variants: &[&'static str]) -> Schema {
    schema(Kind::Union {
        variants: variants.to_vec(),
    })
}
fn variants(field: &'static str, cases: &[(&'static str, &'static str)]) -> Schema {
    schema(Kind::Discriminated {
        field,
        variants: cases.iter().copied().collect(),
    })
}
fn hint(mut schema: Schema, text: &'static str) -> Schema {
    schema.hint = Some(text);
    schema
}
fn constrained(mut schema: Schema, id: &'static str) -> Schema {
    schema.constraints.push(id);
    schema
}
fn owned(mut schema: Schema, code: &'static str) -> Schema {
    schema.diagnostic = Some(code);
    schema
}
fn exclusive(mut schema: Schema, fields: &[&'static str]) -> Schema {
    if let Kind::Object { exclusive, .. } = &mut schema.kind {
        exclusive.push(Exclusive {
            fields: fields.to_vec(),
            min: 1,
            max: 1,
        });
    }
    schema
}
fn conditional(
    mut schema: Schema,
    field: &'static str,
    values: &[&'static str],
    prefix: Option<&'static str>,
    allowed: &[&'static str],
    required: &[&'static str],
) -> Schema {
    if let Kind::Object { conditions, .. } = &mut schema.kind {
        conditions.push(Condition {
            field,
            values: values.to_vec(),
            prefix,
            allowed: allowed.to_vec(),
            required: required.to_vec(),
        });
    }
    schema
}
fn build() -> Registry {
    use ScalarType::*;
    let mut d = IndexMap::new();
    d.insert("text", scalar(Text));
    d.insert("int", scalar(Int));
    d.insert("decimal", scalar(Decimal));
    d.insert("bool", scalar(Bool));
    d.insert(
        "type",
        constrained(scalar(TypeExpression), "type-resolution"),
    );
    d.insert("version", choices(Int, &["1"]));
    d.insert(
        "identifier",
        hint(
            constrained(scalar(Text), "environment-identifier"),
            "Text (identifier)",
        ),
    );
    d.insert(
        "count",
        hint(
            constrained(scalar(Int), "count-range"),
            "Int (0…2147483647)",
        ),
    );
    d.insert(
        "numeric",
        hint(union(&["int", "decimal"]), "Int | Decimal (base type)"),
    );
    d.insert("datum", hint(schema(Kind::Any), "Scalar | List | Object"));
    d.insert("config.value", union(&["text", "int", "bool"]));
    d.insert("text.map", named("text"));
    d.insert("config.map", named("config.value"));
    d.insert(
        "env.package",
        owned(
            object(&[
                ("version", "version", true),
                ("package", "identifier", false),
                ("targets", "env.targets", false),
                ("environments", "env.environments", true),
            ]),
            "ENV001",
        ),
    );
    d.insert("env.targets", named("env.target"));
    d.insert(
        "env.target",
        variants(
            "kind",
            &[
                ("local", "env.target.local"),
                ("docker", "env.target.docker"),
                ("ssh", "env.target.ssh"),
            ],
        ),
    );
    d.insert("env.kind.local", choices(Text, &["local"]));
    d.insert("env.kind.docker", choices(Text, &["docker"]));
    d.insert("env.inherit", choices(Text, &["container"]));
    d.insert("env.kind.ssh", choices(Text, &["ssh"]));
    d.insert("env.ssh.shell", choices(Text, &["posix"]));
    d.insert("env.ssh.inherit", choices(Text, &["remote"]));
    d.insert(
        "env.target.ssh",
        object(&[
            ("kind", "env.kind.ssh", true),
            ("client", "text", true),
            ("host", "text", true),
            ("user", "text", true),
            ("port", "int", false),
            ("identity_file", "text", true),
            ("known_hosts", "text", true),
            ("shell", "env.ssh.shell", true),
            ("inherit", "env.ssh.inherit", true),
            ("cwd", "text", false),
            ("env", "text.map", false),
        ]),
    );
    d.insert(
        "env.target.local",
        object(&[
            ("kind", "env.kind.local", true),
            ("cwd", "text", false),
            ("env", "text.map", false),
        ]),
    );
    d.insert(
        "env.target.docker",
        constrained(
            object(&[
                ("kind", "env.kind.docker", true),
                ("socket", "text", true),
                ("container", "text", false),
                ("compose", "env.target.compose", false),
                ("shell", "text", false),
                ("image", "text", false),
                ("inherit", "env.inherit", true),
                ("cwd", "text", false),
                ("env", "text.map", false),
            ]),
            "docker-target",
        ),
    );
    d.insert(
        "env.target.compose",
        object(&[
            ("project", "text", true),
            ("service", "text", true),
            ("replica", "int", false),
        ]),
    );
    d.insert("env.environments", named("env.environment"));
    d.insert("env.target.names", list("identifier"));
    d.insert(
        "env.environment",
        constrained(
            object(&[
                ("targets", "env.target.names", false),
                ("owner", "identifier", false),
                ("drift", "bool", false),
                ("id", "identifier", false),
                ("retired", "bool", false),
                ("protected", "bool", false),
                ("abstract", "bool", false),
                ("extends", "env.extends", false),
                ("parameters", "env.parameters", false),
                ("config", "config.map", false),
                ("secretSlots", "env.secretSlots", false),
                ("secretRefs", "text.map", false),
                ("imports", "env.imports", false),
                ("overrides", "env.overrides", false),
                ("hide", "env.hide", false),
            ]),
            "environment-resolution",
        ),
    );
    d.insert(
        "env.extends",
        exclusive(
            object(&[
                ("env", "identifier", true),
                ("track", "env.track", false),
                ("revision", "text", false),
            ]),
            &["track", "revision"],
        ),
    );
    d.insert("env.track", choices(Text, &["latest"]));
    d.insert("env.parameters", named("env.parameter"));
    d.insert(
        "env.parameter",
        variants(
            "type",
            &[
                ("Text", "env.parameter.text"),
                ("Int", "env.parameter.int"),
                ("Bool", "env.parameter.bool"),
            ],
        ),
    );
    for (id, choice, value, choice_id) in [
        ("env.parameter.text", "Text", "text", "env.type.text"),
        ("env.parameter.int", "Int", "int", "env.type.int"),
        ("env.parameter.bool", "Bool", "bool", "env.type.bool"),
    ] {
        d.insert(choice_id, choices(Text, &[choice]));
        // A well-shaped default that disagrees with its parameter preserves ENV003.
        let default_id = match value {
            "text" => "env.default.text",
            "int" => "env.default.int",
            _ => "env.default.bool",
        };
        let default_type = match value {
            "text" => Text,
            "int" => Int,
            _ => Bool,
        };
        d.insert(default_id, owned(scalar(default_type), "ENV003"));
        d.insert(
            id,
            object(&[("type", choice_id, true), ("default", default_id, false)]),
        );
    }
    d.insert("env.secretSlots", named("env.secretSlot"));
    d.insert("env.secretSlot", object(&[("required", "bool", true)]));
    d.insert("env.imports", named("env.import"));
    d.insert(
        "env.import",
        object(&[("source", "env.source", true), ("bind", "env.bind", false)]),
    );
    d.insert(
        "env.source",
        variants(
            "kind",
            &[
                ("spec", "env.source.spec"),
                ("openapi", "env.source.openapi"),
                ("process", "env.source.process"),
                ("docker", "env.source.docker"),
                ("builtin", "env.source.builtin"),
            ],
        ),
    );
    d.insert("env.kind.spec", choices(Text, &["spec"]));
    d.insert("env.kind.openapi", choices(Text, &["openapi"]));
    d.insert("env.kind.process", choices(Text, &["process"]));
    d.insert("env.kind.docker", choices(Text, &["docker"]));
    d.insert("env.kind.builtin", choices(Text, &["builtin"]));
    d.insert("env.builtin.name", choices(Text, &["sh", "http", "docker"]));
    d.insert(
        "env.source.builtin",
        object(&[
            ("kind", "env.kind.builtin", true),
            ("name", "env.builtin.name", true),
        ]),
    );
    d.insert(
        "env.source.docker",
        object(&[("kind", "env.kind.docker", true), ("socket", "text", true)]),
    );
    d.insert(
        "env.source.spec",
        constrained(
            exclusive(
                object(&[
                    ("kind", "env.kind.spec", true),
                    ("file", "text", false),
                    ("url", "text", false),
                    ("sha256", "text", false),
                ]),
                &["file", "url"],
            ),
            "spec-source",
        ),
    );
    d.insert(
        "env.source.openapi",
        constrained(
            exclusive(
                object(&[
                    ("kind", "env.kind.openapi", true),
                    ("file", "text", false),
                    ("url", "text", false),
                ]),
                &["file", "url"],
            ),
            "spec-source",
        ),
    );
    d.insert(
        "env.source.process",
        object(&[("kind", "env.kind.process", true), ("bin", "text", true)]),
    );
    d.insert("env.bind", union(&["env.bind.explicit", "env.bind.auto"]));
    d.insert("env.bind.auto", choices(Text, &["auto"]));
    d.insert("env.transport", choices(Text, &["internal", "curl"]));
    d.insert(
        "env.bind.explicit",
        constrained(
            object(&[
                ("target", "identifier", true),
                ("endpoint", "env.endpoint", false),
                ("timeout_ms", "env.timeout", false),
                ("transport", "env.transport", false),
                ("credentials", "env.credentials", false),
                ("auth", "env.auth", false),
                ("output", "env.output", false),
            ]),
            "environment-binding",
        ),
    );
    d.insert(
        "env.endpoint",
        hint(
            union(&["config.value", "env.setting"]),
            "Text | Object {config}",
        ),
    );
    d.insert(
        "env.timeout",
        hint(
            union(&["config.value", "env.setting"]),
            "Int > 0 | Object {config}",
        ),
    );
    d.insert("env.setting", object(&[("config", "identifier", true)]));
    d.insert("env.auth", named("env.auth.schemes"));
    d.insert("env.auth.schemes", list("text"));
    d.insert("env.credentials", named("env.credential"));
    d.insert("env.credential", object(&[("secret", "identifier", true)]));
    d.insert("env.output", choices(Text, &["public", "private"]));
    d.insert("env.overrides", object(&[("imports", "env.imports", true)]));
    d.insert("env.hide", object(&[("imports", "env.hidden", true)]));
    d.insert("env.hidden", list("identifier"));
    d.insert(
        "contract.package",
        owned(
            object(&[
                ("version", "contract.version", false),
                ("types", "contract.types", true),
                ("iterators", "contract.iterators", false),
            ]),
            "TYP002",
        ),
    );
    d.insert("contract.version", choices(Int, &["1", "2"]));
    d.insert(
        "contract.display",
        object(&[("enumTones", "contract.enumTones", true)]),
    );
    d.insert("contract.enumTones", named("contract.enumTone"));
    d.insert(
        "contract.enumTone",
        choices(Text, crate::contracts::EnumTone::NAMES),
    );
    d.insert("contract.types", named("contract.type"));
    let mut contract = constrained(
        object(&[
            ("base", "type", true),
            ("description", "text", false),
            ("enum", "contract.enum", false),
            ("display", "contract.display", false),
            ("min", "numeric", false),
            ("max", "numeric", false),
            ("minLength", "count", false),
            ("maxLength", "count", false),
            ("minItems", "count", false),
            ("maxItems", "count", false),
            ("pattern", "text", false),
            ("fields", "contract.fields", false),
        ]),
        "contract-subtype",
    );
    for (base, mut allowed) in [
        ("Record", vec!["base", "fields"]),
        (
            "Text",
            vec!["base", "enum", "minLength", "maxLength", "pattern"],
        ),
        ("Int", vec!["base", "enum", "min", "max"]),
        ("Decimal", vec!["base", "enum", "min", "max"]),
        ("Bool", vec!["base", "enum"]),
    ] {
        allowed.push("description");
        if base != "Record" {
            allowed.push("display");
        }
        contract = conditional(contract, "base", &[base], None, &allowed, &[]);
    }
    contract = conditional(
        contract,
        "base",
        &[],
        Some("List<"),
        &["base", "description", "minItems", "maxItems"],
        &[],
    );
    d.insert("contract.type", contract);
    d.insert(
        "contract.enum",
        hint(list("config.orDecimal"), "List<Scalar> (base type)"),
    );
    d.insert(
        "config.orDecimal",
        union(&["text", "int", "decimal", "bool"]),
    );
    d.insert("contract.fields", named("contract.field"));
    d.insert("contract.field", union(&["type", "contract.field.object"]));
    d.insert(
        "contract.field.object",
        object(&[
            ("type", "type", true),
            ("optional", "bool", false),
            ("description", "text", false),
        ]),
    );
    d.insert("contract.iterators", named("contract.iterator"));
    d.insert(
        "iterator.mode",
        choices(
            Text,
            &crate::IterMode::ALL
                .iter()
                .map(|mode| mode.name())
                .collect::<Vec<_>>(),
        ),
    );
    d.insert(
        "iterator.output",
        hint(
            constrained(scalar(TypeExpression), "iterator-output"),
            "Type expression (Iter<T>)",
        ),
    );
    let mut iterator = constrained(
        object(&[
            ("input", "type", true),
            ("output", "iterator.output", true),
            ("mode", "iterator.mode", true),
            ("delimiter", "text", false),
            ("pattern", "text", false),
        ]),
        "iterator-input",
    );
    iterator = conditional(
        iterator,
        "mode",
        &["split"],
        None,
        &["input", "output", "mode", "delimiter"],
        &["delimiter"],
    );
    iterator = conditional(
        iterator,
        "mode",
        &crate::IterMode::ALL
            .iter()
            .filter(|mode| mode.argument_kind() == Some("pattern"))
            .map(|mode| mode.name())
            .collect::<Vec<_>>(),
        None,
        &["input", "output", "mode", "pattern"],
        &["pattern"],
    );
    let simple = crate::IterMode::ALL
        .iter()
        .filter(|mode| mode.argument_kind().is_none())
        .map(|mode| mode.name())
        .collect::<Vec<_>>();
    iterator = conditional(
        iterator,
        "mode",
        &simple,
        None,
        &["input", "output", "mode"],
        &[],
    );
    d.insert("contract.iterator", iterator);
    Registry { roots: [("env", "env.package"), ("types", "contract.package")].into_iter().collect(), definitions: d,
        constraints: [
            ("environment-identifier", "Environment names follow the core identifier rules."),
            ("count-range", "Counts are integers from 0 through 2147483647."),
            ("type-resolution", "Type expressions resolve against the workspace registry and constructor arity."),
            ("docker-target", "Docker target socket, container and image constraints are checked by the environment parser."),
            ("environment-resolution", "Inheritance, configuration, ownership and import conflicts are resolved by the engine."),
            ("spec-source", "Spec file/url exclusivity, location scheme and SHA-256 are checked by the environment parser."),
            ("environment-binding", "Endpoint requires Text; timeout requires positive Int; references must resolve to compatible slots. Process imports cannot set endpoint."),
            ("contract-subtype", "Constraints must match the resolved base kind and cannot weaken inherited limits or fields."),
            ("iterator-output", "Iterator output must resolve to Iter<T>."),
            ("iterator-input", "Iterator mode must match the resolved input shape; extraction patterns and limits are checked by core."),
        ].into_iter().collect() }
}
