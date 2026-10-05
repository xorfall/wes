//! Bind the fixed native operator set once, before any source executes.
use super::*;
impl Analysis<'_> {
    pub(super) fn stream_operator(
        &self,
        statement: &Statement,
        call: &wes_language::Call,
    ) -> Result<Preparation, WorkspaceError> {
        use crate::stream_ops::{BoundOperator, Operation};
        use wes_language::Value as Literal;
        let invalid = |message| rejected("STR001", statement.span, message);
        self.validate_names(statement)?;
        if !(1..=2).contains(&call.path.len()) || !call.operands.is_empty() {
            return Err(invalid(
                "expected :stream filter or :stream map with named arguments",
            ));
        }
        let input = self
            .pipe_input
            .cloned()
            .ok_or_else(|| invalid("stream operators require a preceding pipeline input"))?;
        let mut args = IndexMap::new();
        for arg in &call.arguments {
            if args.insert(arg.key.text.as_str(), &arg.value).is_some() {
                return Err(invalid("duplicate stream argument"));
            }
        }
        let operation = if call.path.len() == 1 {
            let (key, value) = args
                .first()
                .filter(|_| args.len() == 1)
                .ok_or_else(|| invalid("use :stream limit:<count> or :stream skip:<count>"))?;
            let Literal::Word(value) = value else {
                return Err(invalid("stream count must be a literal integer"));
            };
            let Some(wes_core::Data::Int(n)) =
                wes_core::literals::read(&value.text, &Shape::Primitive(wes_core::Primitive::Int))
            else {
                return Err(invalid("stream count must be an integer"));
            };
            if !(0..=1_000_000_000).contains(&n) {
                return Err(invalid("stream count must be from 0 to 1000000000"));
            }
            match *key {
                "limit" if n > 0 => Operation::Limit(n as u64),
                "skip" => Operation::Skip(n as u64),
                _ => return Err(invalid("use positive limit: or nonnegative skip:")),
            }
        } else {
            match call.path[1].text.as_str() {
                "filter" if args.len() == 1 && args.contains_key("condition") => {
                    let Literal::Reference(name) = args["condition"] else {
                        return Err(invalid("condition requires a Bool result reference"));
                    };
                    let reference = self
                        .resolve(&name.text)
                        .ok_or_else(|| invalid("unknown predicate result"))?;
                    Operation::Condition(reference)
                }
                "filter" | "map" => {
                    let Some(Literal::Word(field) | Literal::Text(field)) =
                        args.get("field").copied()
                    else {
                        return Err(invalid("field requires a literal field path"));
                    };
                    let path: Vec<_> = field.text.split('.').map(str::to_owned).collect();
                    if path.len() > 64 || path.iter().any(String::is_empty) {
                        return Err(invalid("field path must contain 1..64 nonempty segments"));
                    }
                    if call.path[1].text == "map" {
                        if args.len() != 1 {
                            return Err(invalid("map field accepts only field:"));
                        }
                        Operation::Project(path)
                    } else {
                        if args.len() != 2 {
                            return Err(invalid(
                                "filter field requires exactly field: and equals:",
                            ));
                        }
                        let Some(value) = args.get("equals") else {
                            return Err(invalid("filter requires equals:"));
                        };
                        let equals = match value {
                            Literal::Text(text) => wes_core::Data::Text(text.text.as_str().into()),
                            Literal::Word(text) => match text.text.as_str() {
                                "true" => wes_core::Data::Bool(true),
                                "false" => wes_core::Data::Bool(false),
                                other => wes_core::literals::read(
                                    other,
                                    &Shape::Primitive(wes_core::Primitive::Int),
                                )
                                .unwrap_or_else(|| wes_core::Data::Text(other.into())),
                            },
                            _ => return Err(invalid("equals requires a scalar literal")),
                        };
                        Operation::Filter {
                            field: path,
                            equals,
                        }
                    }
                }
                _ => return Err(invalid("unknown native stream operator")),
            }
        };
        let mut typing = self
            .typing(&input)
            .unwrap_or_else(|| Typing::new(Shape::Unknown));
        if let Operation::Condition(reference) = &operation {
            if let Some(condition) = self.typing(reference) {
                typing.provenance = typing.provenance.inheriting(&condition.provenance);
            }
        }
        if matches!(operation, Operation::Project(_)) {
            typing.shape = Shape::Unknown;
        }
        self.node(
            statement,
            BoundTask::Stream(BoundOperator {
                input,
                operation,
                delivery: None,
            }),
            Arc::new(typing),
            vec![],
        )
    }
}
