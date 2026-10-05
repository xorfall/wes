use super::ContractError;
use regex::Regex;
use std::{fmt, sync::LazyLock};

static NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[_\p{L}][_\p{L}\p{Nd}]*").expect("constant type-name pattern"));

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TypeExpression {
    pub name: String,
    pub arguments: Vec<TypeExpression>,
}

impl TypeExpression {
    pub fn parse(source: &str) -> Result<Self, ContractError> {
        if source.encode_utf16().count() > 4096 {
            return Err(error("type expression is too long", 0));
        }
        let mut reader = Reader { source, at: 0 };
        let expression = reader.expression(0)?;
        reader.space();
        if reader.at != source.len() {
            return Err(reader.error("unexpected character"));
        }
        Ok(expression)
    }
}

impl fmt::Display for TypeExpression {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name)?;
        if !self.arguments.is_empty() {
            f.write_str("<")?;
            for (i, arg) in self.arguments.iter().enumerate() {
                if i > 0 {
                    f.write_str(", ")?;
                }
                arg.fmt(f)?;
            }
            f.write_str(">")?;
        }
        Ok(())
    }
}

struct Reader<'a> {
    source: &'a str,
    at: usize,
}

impl Reader<'_> {
    fn space(&mut self) {
        while let Some(ch) = self.source[self.at..].chars().next() {
            if !matches!(ch,'\u{9}'..='\u{d}'|'\u{1c}'..='\u{20}'|'\u{1680}'|'\u{2000}'..='\u{2006}'|'\u{2008}'..='\u{200a}'|'\u{2028}'|'\u{2029}'|'\u{205f}'|'\u{3000}')
            {
                break;
            }
            self.at += ch.len_utf8();
        }
    }
    fn take(&mut self, ch: char) -> bool {
        self.space();
        if self.source[self.at..].starts_with(ch) {
            self.at += ch.len_utf8();
            true
        } else {
            false
        }
    }
    fn error(&self, message: &str) -> ContractError {
        error(message, self.source[..self.at].encode_utf16().count())
    }
    fn expression(&mut self, depth: usize) -> Result<TypeExpression, ContractError> {
        if depth > 64 {
            return Err(self.error("type expression is too deeply nested"));
        }
        self.space();
        let Some(found) = NAME.find(&self.source[self.at..]) else {
            return Err(self.error("expected a type name"));
        };
        let name = found.as_str().to_string();
        self.at += found.len();
        let mut arguments = Vec::new();
        if self.take('<') {
            loop {
                arguments.push(self.expression(depth + 1)?);
                if !self.take(',') {
                    break;
                }
            }
            if !self.take('>') {
                return Err(self.error("expected '>'"));
            }
        }
        Ok(TypeExpression { name, arguments })
    }
}

fn error(message: &str, offset: usize) -> ContractError {
    ContractError {
        code: "TYP001",
        message: format!("{message} at type-expression offset {offset}"),
    }
}

/// Constructor spellings and parameters used by resolution, reserved-name checks
/// and declaration-schema publication. Semantic construction remains exhaustive.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Constructor {
    List,
    Map,
    Option,
    Iter,
    Union,
}
#[derive(Clone, Copy, Debug)]
pub struct TypeConstructor {
    pub constructor: Constructor,
    pub name: &'static str,
    pub parameters: &'static [&'static str],
}
pub const TYPE_CONSTRUCTORS: &[TypeConstructor] = &[
    TypeConstructor {
        constructor: Constructor::Union,
        name: "Union",
        parameters: &["A", "B"],
    },
    TypeConstructor {
        constructor: Constructor::List,
        name: "List",
        parameters: &["T"],
    },
    TypeConstructor {
        constructor: Constructor::Map,
        name: "Map",
        parameters: &["Text", "T"],
    },
    TypeConstructor {
        constructor: Constructor::Option,
        name: "Option",
        parameters: &["T"],
    },
    TypeConstructor {
        constructor: Constructor::Iter,
        name: "Iter",
        parameters: &["T"],
    },
];
impl Constructor {
    pub fn named(name: &str) -> Option<&'static TypeConstructor> {
        TYPE_CONSTRUCTORS.iter().find(|spec| spec.name == name)
    }
}
