use indexmap::IndexMap;
use yaml_rust2::{
    Yaml,
    parser::{Event, Parser, Tag},
    scanner::TScalarStyle,
};

use super::ContractError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScalarKind {
    Text,
    Int,
    Decimal,
    Bool,
}

#[derive(Clone, Debug)]
pub enum Node {
    Scalar(ScalarKind, String),
    Sequence(Vec<Node>),
    Mapping(IndexMap<String, Node>),
}

impl Node {
    /// Canonical YAML flow syntax, preserving scalar kinds and exact numeric tokens.
    pub(super) fn source(&self) -> String {
        fn quote(value: &str) -> String {
            let mut out = String::from("\"");
            for c in value.chars() {
                match c {
                    '\\' => out.push_str("\\\\"),
                    '\"' => out.push_str("\\\""),
                    c if c.is_control() => out.push_str(&format!("\\u{:04x}", c as u32)),
                    c => out.push(c),
                }
            }
            out.push('\"');
            out
        }
        match self {
            Self::Scalar(ScalarKind::Text, value) => quote(value),
            Self::Scalar(_, value) => value.clone(),
            Self::Sequence(values) => format!(
                "[{}]",
                values
                    .iter()
                    .map(Self::source)
                    .collect::<Vec<_>>()
                    .join(",")
            ),
            Self::Mapping(values) => format!(
                "{{{}}}",
                values
                    .iter()
                    .map(|(key, value)| format!("{}:{}", quote(key), value.source()))
                    .collect::<Vec<_>>()
                    .join(",")
            ),
        }
    }
    pub fn text(&self) -> Result<&str, ContractError> {
        match self {
            Self::Scalar(ScalarKind::Text, text) => Ok(text),
            _ => Err(problem("expected text")),
        }
    }
}

pub fn read(source: &str) -> Result<Node, ContractError> {
    read_mode(source, false)
}

/// Environment packages deliberately exclude all YAML tags and anchors, including unused anchors.
pub fn read_strict(source: &str) -> Result<Node, ContractError> {
    read_mode(source, true)
}

fn read_mode(source: &str, strict: bool) -> Result<Node, ContractError> {
    if source.len() > 1_048_576 {
        return Err(problem("type package exceeds 1 MiB of UTF-8 input"));
    }
    let mut reader = Reader {
        parser: Parser::new_from_str(source),
        events: 0,
        strict,
    };
    if reader.next()? != Event::StreamStart || reader.next()? != Event::DocumentStart {
        return Err(problem("type package requires one YAML document"));
    }
    let event = reader.next()?;
    let root = reader.node(event, 0)?;
    if reader.next()? != Event::DocumentEnd || reader.next()? != Event::StreamEnd {
        return Err(problem("type package requires exactly one YAML document"));
    }
    Ok(root)
}

struct Reader<'a> {
    parser: Parser<std::str::Chars<'a>>,
    events: usize,
    strict: bool,
}

impl Reader<'_> {
    fn next(&mut self) -> Result<Event, ContractError> {
        self.events += 1;
        if self.events > 20_000 {
            return Err(problem("too many YAML events"));
        }
        self.parser
            .next_token()
            .map(|(event, _)| event)
            .map_err(|e| problem(format!("invalid YAML: {e}")))
    }
    fn node(&mut self, event: Event, depth: usize) -> Result<Node, ContractError> {
        if self.strict
            && matches!(&event,
                Event::Scalar(_, _, anchor, tag)
                | Event::SequenceStart(anchor, tag)
                | Event::MappingStart(anchor, tag) if *anchor != 0 || tag.is_some())
        {
            return Err(problem("YAML tags and anchors are not allowed"));
        }
        match event {
            Event::Scalar(value, style, _, tag) => scalar(value, style, tag),
            Event::Alias(_) => Err(problem("YAML aliases are not allowed in type packages")),
            Event::SequenceStart(_, tag) => {
                collection_tag(tag, "seq")?;
                if depth >= 64 {
                    return Err(problem("YAML nesting exceeds 64"));
                }
                let mut entries = Vec::new();
                loop {
                    let next = self.next()?;
                    if next == Event::SequenceEnd {
                        break;
                    }
                    entries.push(self.node(next, depth + 1)?);
                }
                Ok(Node::Sequence(entries))
            }
            Event::MappingStart(_, tag) => {
                collection_tag(tag, "map")?;
                if depth >= 64 {
                    return Err(problem("YAML nesting exceeds 64"));
                }
                let mut entries = IndexMap::new();
                loop {
                    let next = self.next()?;
                    if next == Event::MappingEnd {
                        break;
                    }
                    let key = self.node(next, depth + 1)?.text()?.to_string();
                    if entries.contains_key(&key) {
                        return Err(problem(format!("duplicate YAML key: {key}")));
                    }
                    let next = self.next()?;
                    let value = self.node(next, depth + 1)?;
                    entries.insert(key, value);
                }
                Ok(Node::Mapping(entries))
            }
            _ => Err(problem("expected a YAML value")),
        }
    }
}

fn collection_tag(tag: Option<Tag>, expected: &str) -> Result<(), ContractError> {
    if let Some(tag) = tag
        && (tag.handle != "tag:yaml.org,2002:" || tag.suffix != expected)
    {
        return Err(problem("unsupported YAML collection tag"));
    }
    Ok(())
}

fn scalar(value: String, style: TScalarStyle, tag: Option<Tag>) -> Result<Node, ContractError> {
    let kind = if let Some(tag) = tag {
        if tag.handle != "tag:yaml.org,2002:" {
            return Err(problem("unsupported YAML scalar tag"));
        }
        match tag.suffix.as_str() {
            "str" => ScalarKind::Text,
            "int" => ScalarKind::Int,
            "float" => ScalarKind::Decimal,
            "bool" => ScalarKind::Bool,
            _ => return Err(problem("unsupported YAML scalar tag")),
        }
    } else if style != TScalarStyle::Plain {
        ScalarKind::Text
    } else {
        // The library resolves YAML scalars, but all numeric content remains its original text.
        // Core-schema null case variants are rejected even when the library treats them as text.
        if matches!(value.as_str(), "Null" | "NULL") {
            return Err(problem("null schema values are not supported"));
        }
        match Yaml::from_str(&value) {
            Yaml::Integer(_) => ScalarKind::Int,
            Yaml::Real(_) => ScalarKind::Decimal,
            Yaml::Boolean(_) => ScalarKind::Bool,
            Yaml::String(_) => ScalarKind::Text,
            _ => return Err(problem("null schema values are not supported")),
        }
    };
    if kind == ScalarKind::Bool
        && !value.eq_ignore_ascii_case("true")
        && !value.eq_ignore_ascii_case("false")
    {
        return Err(problem("expected a true/false boolean"));
    }
    Ok(Node::Scalar(kind, value))
}

fn problem(message: impl Into<String>) -> ContractError {
    ContractError::declaration(message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn library_parses_nested_block_and_flow_yaml_without_losing_decimal_text() {
        let node =
            read("types:\n  Price: {base: Decimal, min: 0.123456789012345678901234567890}\n")
                .unwrap();
        let Node::Mapping(root) = node else {
            panic!("mapping")
        };
        let Node::Mapping(types) = &root["types"] else {
            panic!("mapping")
        };
        let Node::Mapping(price) = &types["Price"] else {
            panic!("mapping")
        };
        let Node::Scalar(ScalarKind::Decimal, decimal) = &price["min"] else {
            panic!("decimal")
        };
        assert_eq!(decimal, "0.123456789012345678901234567890");
    }

    #[test]
    fn yaml12_plain_words_and_quoted_numbers_remain_text() {
        let Node::Mapping(root) = read("items: [yes, no, 'true', '12']").unwrap() else {
            panic!("mapping")
        };
        let Node::Sequence(items) = &root["items"] else {
            panic!("sequence")
        };
        assert!(items.iter().all(|node| node.text().is_ok()));
    }
}
