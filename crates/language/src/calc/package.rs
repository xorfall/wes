use crate::{Diagnostic, Span};
use indexmap::IndexMap;
use std::sync::{Arc, LazyLock};
use wes_core::contracts::{PackageNode as Node, PackageScalarKind as Scalar, read_package};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Keyword {
    Const,
    Let,
    Function,
    If,
    Else,
    While,
    For,
    Of,
    Return,
    Break,
    Continue,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Binary {
    Or,
    And,
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
    Add,
    Sub,
    Mul,
    Divide,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BinarySpec {
    pub operation: Binary,
    pub precedence: u8,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Operation {
    Instant,
    Duration,
    Interval,
    Around,
    FromEpochSeconds,
    FromEpochMillis,
    FromEpochNanos,
    ToEpochSeconds,
    ToEpochMillis,
    ToEpochNanos,
    DurationSeconds,
    DurationMillis,
    DurationNanos,
    ToSeconds,
    ToMillis,
    ToNanos,
    UtcParts,

    Some,
    IsSome,
    UnwrapOr,
    Has,
    Length,
    Keys,
    Map,
    Filter,
    Reduce,
    Join,
    WithFields,
    Slice,
    Concat,
    SortBy,
    Range,
    Decimal,
    Int,
    Text,
    Div,
    Rem,
    RoundDiv,
    ParseJson,
    HttpStatus,
    HttpError,
    HttpCatalogue,
    HttpAnalysis,
    Check,
    Call,
    IterItems,
    IterLines,
    IterChars,
    IterWords,
    IterSplit,
    IterRegexSplit,
    IterCaptures,
    IterMatches,
    IterKeys,
    IterValues,
    IterEntries,
    IterJsonLines,
    IterUse,
    IterChecked,
    Take,
    Skip,
    Collect,
    Count,
    Field,
}
/// The only lexical productions a package may declare. A package naming anything else is refused,
/// so these four pairs describe every package this language accepts.
const LEXICAL: &[(&str, &str)] = &[
    ("identifier", "unicode"),
    ("strings", "quoted"),
    ("comments", "slash"),
    ("numbers", "exact"),
];
/// One row per semantics: the written id, the meaning, and the arity the meaning allows.
/// Reading a package and naming a package back both go through this table, so the two can not drift.
const SEMANTICS: &[(&str, Operation, u8, u8)] = &[
    ("instant", Operation::Instant, 1, 1),
    ("duration", Operation::Duration, 1, 1),
    ("interval", Operation::Interval, 2, 2),
    ("around", Operation::Around, 2, 2),
    ("fromEpochSeconds", Operation::FromEpochSeconds, 1, 1),
    ("fromEpochMillis", Operation::FromEpochMillis, 1, 1),
    ("fromEpochNanos", Operation::FromEpochNanos, 1, 1),
    ("toEpochSeconds", Operation::ToEpochSeconds, 1, 1),
    ("toEpochMillis", Operation::ToEpochMillis, 1, 1),
    ("toEpochNanos", Operation::ToEpochNanos, 1, 1),
    ("durationSeconds", Operation::DurationSeconds, 1, 1),
    ("durationMillis", Operation::DurationMillis, 1, 1),
    ("durationNanos", Operation::DurationNanos, 1, 1),
    ("toSeconds", Operation::ToSeconds, 1, 1),
    ("toMillis", Operation::ToMillis, 1, 1),
    ("toNanos", Operation::ToNanos, 1, 1),
    ("utcParts", Operation::UtcParts, 1, 1),
    ("some", Operation::Some, 1, 1),
    ("is-some", Operation::IsSome, 1, 1),
    ("unwrap-or", Operation::UnwrapOr, 2, 2),
    ("has", Operation::Has, 2, 2),
    ("length", Operation::Length, 1, 1),
    ("keys", Operation::Keys, 1, 1),
    ("map", Operation::Map, 2, 2),
    ("filter", Operation::Filter, 2, 2),
    ("reduce", Operation::Reduce, 3, 3),
    ("join", Operation::Join, 2, 2),
    ("with-fields", Operation::WithFields, 2, 2),
    ("slice", Operation::Slice, 2, 3),
    ("concat", Operation::Concat, 2, 2),
    ("sort-by", Operation::SortBy, 2, 2),
    ("range", Operation::Range, 1, 3),
    ("decimal", Operation::Decimal, 1, 1),
    ("int", Operation::Int, 1, 1),
    ("text", Operation::Text, 1, 1),
    ("div", Operation::Div, 2, 2),
    ("rem", Operation::Rem, 2, 2),
    ("round-div", Operation::RoundDiv, 3, 3),
    ("parse-json", Operation::ParseJson, 1, 2),
    ("http-status", Operation::HttpStatus, 1, 1),
    ("http-error", Operation::HttpError, 1, 1),
    ("http-catalogue", Operation::HttpCatalogue, 1, 1),
    ("http-analysis", Operation::HttpAnalysis, 1, 1),
    ("check", Operation::Check, 2, 2),
    ("call", Operation::Call, 3, 3),
    ("iter-items", Operation::IterItems, 1, 1),
    ("iter-lines", Operation::IterLines, 1, 1),
    ("iter-chars", Operation::IterChars, 1, 1),
    ("iter-words", Operation::IterWords, 1, 1),
    ("iter-split", Operation::IterSplit, 2, 2),
    ("iter-regex-split", Operation::IterRegexSplit, 2, 2),
    ("iter-captures", Operation::IterCaptures, 2, 2),
    ("iter-matches", Operation::IterMatches, 2, 2),
    ("iter-keys", Operation::IterKeys, 1, 1),
    ("iter-values", Operation::IterValues, 1, 1),
    ("iter-entries", Operation::IterEntries, 1, 1),
    ("iter-json-lines", Operation::IterJsonLines, 1, 1),
    ("iter-use", Operation::IterUse, 2, 2),
    ("iter-checked", Operation::IterChecked, 2, 2),
    ("take", Operation::Take, 2, 2),
    ("skip", Operation::Skip, 2, 2),
    ("collect", Operation::Collect, 1, 1),
    ("count", Operation::Count, 1, 1),
    ("field", Operation::Field, 2, 2),
];
impl Operation {
    pub fn iter_mode(self) -> Option<wes_core::IterMode> {
        use wes_core::IterMode;
        Some(match self {
            Self::IterItems => IterMode::Items,
            Self::IterLines => IterMode::Lines,
            Self::IterChars => IterMode::Chars,
            Self::IterWords => IterMode::Words,
            Self::IterSplit => IterMode::Split,
            Self::IterRegexSplit => IterMode::RegexSplit,
            Self::IterMatches => IterMode::Matches,
            Self::IterCaptures => IterMode::Captures,
            Self::IterKeys => IterMode::Keys,
            Self::IterValues => IterMode::Values,
            Self::IterEntries => IterMode::Entries,
            Self::IterJsonLines => IterMode::JsonLines,
            _ => return None,
        })
    }

    fn read(id: &str) -> Option<(Self, u8, u8)> {
        SEMANTICS
            .iter()
            .find(|(written, ..)| *written == id)
            .map(|(_, operation, min, max)| (*operation, *min, *max))
    }
    /// The id this semantics is written as in a package file.
    pub fn id(self) -> &'static str {
        SEMANTICS
            .iter()
            .find(|(_, operation, ..)| *operation == self)
            .map_or("", |(written, ..)| written)
    }
    pub fn effectful(self) -> bool {
        self == Self::Call
    }
}
impl Keyword {
    /// The production this keyword is written as in a package file.
    pub fn id(self) -> &'static str {
        match self {
            Self::Const => "binding",
            Self::Let => "binding-mut",
            Self::Function => "function",
            Self::If => "if",
            Self::Else => "else",
            Self::While => "while",
            Self::For => "for",
            Self::Of => "of",
            Self::Return => "return",
            Self::Break => "break",
            Self::Continue => "continue",
        }
    }
}
impl Binary {
    /// The operation this operator is written as in a package file.
    pub fn id(self) -> &'static str {
        match self {
            Self::Or => "or",
            Self::And => "and",
            Self::Eq => "eq",
            Self::Ne => "ne",
            Self::Lt => "lt",
            Self::Le => "le",
            Self::Gt => "gt",
            Self::Ge => "ge",
            Self::Add => "add",
            Self::Sub => "sub",
            Self::Mul => "mul",
            Self::Divide => "divide",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct OperationSpec {
    pub operation: Operation,
    pub min: u8,
    pub max: u8,
}

/// Immutable full source is the capture identity; only the current semantics are accepted.
#[derive(Clone, Debug)]
pub struct Package {
    source: Arc<str>,
    version: u32,
    keywords: IndexMap<String, Keyword>,
    binary: IndexMap<String, BinarySpec>,
    operations: IndexMap<String, OperationSpec>,
}
fn error(message: impl Into<String>) -> Diagnostic {
    Diagnostic::error("CAL001", Span::at(0), message)
}
fn mapping(node: Node) -> Result<IndexMap<String, Node>, Diagnostic> {
    if let Node::Mapping(m) = node {
        Ok(m)
    } else {
        Err(error("expected a YAML mapping"))
    }
}
fn take(map: &mut IndexMap<String, Node>, key: &str) -> Result<Node, Diagnostic> {
    map.shift_remove(key)
        .ok_or_else(|| error(format!("missing '{key}'")))
}
fn text(node: Node) -> Result<String, Diagnostic> {
    if let Node::Scalar(Scalar::Text, s) = node {
        Ok(s)
    } else {
        Err(error("expected text"))
    }
}
fn integer(node: Node) -> Result<u8, Diagnostic> {
    if let Node::Scalar(Scalar::Int, s) = node {
        s.parse()
            .map_err(|_| error("expected small nonnegative integer"))
    } else {
        Err(error("expected integer"))
    }
}
fn finished(map: &IndexMap<String, Node>) -> Result<(), Diagnostic> {
    if map.is_empty() {
        Ok(())
    } else {
        Err(error("unknown language package field"))
    }
}
pub(super) fn identifier(s: &str) -> bool {
    let mut chars = s.chars();
    chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
        && chars.all(|c| c == '_' || c.is_alphanumeric())
}
impl Package {
    pub fn standard() -> Arc<Self> {
        static DEFAULT: LazyLock<Arc<Package>> = LazyLock::new(|| {
            Arc::new(Package::load(super::DEFAULT_PACKAGE).expect("bundled calc package"))
        });
        DEFAULT.clone()
    }
    pub fn load(source: &str) -> Result<Self, Diagnostic> {
        if source.len() > 64 * 1024 {
            return Err(error("language package exceeds 64 KiB"));
        }
        let root = read_package(source).map_err(|e| error(e.message))?;
        let mut root = mapping(root)?;
        let version = u32::from(integer(take(&mut root, "version")?)?);
        if version != 1 {
            return Err(error("unsupported calculation semantics version"));
        }
        let mut lexical = mapping(take(&mut root, "lexical")?)?;
        for (key, expected) in LEXICAL {
            if text(take(&mut lexical, key)?)? != *expected {
                return Err(error("unsupported lexical production"));
            }
        }
        finished(&lexical)?;
        let mut keywords = IndexMap::new();
        for (word, node) in mapping(take(&mut root, "statements")?)? {
            if !identifier(&word) || matches!(word.as_str(), "none" | "true" | "false") {
                return Err(error("invalid statement keyword"));
            }
            let keyword = match text(node)?.as_str() {
                "binding" => Keyword::Const,
                "binding-mut" => Keyword::Let,
                "function" => Keyword::Function,
                "if" => Keyword::If,
                "else" => Keyword::Else,
                "while" => Keyword::While,
                "for" => Keyword::For,
                "of" => Keyword::Of,
                "return" => Keyword::Return,
                "break" => Keyword::Break,
                "continue" => Keyword::Continue,
                _ => return Err(error("unknown statement/AST production")),
            };
            if keywords.values().any(|v| *v == keyword) {
                return Err(error("ambiguous statement mapping"));
            }
            keywords.insert(word, keyword);
        }
        if keywords.len() != 11 {
            return Err(error("incomplete statement grammar"));
        }
        let mut binary = IndexMap::new();
        for (symbol, node) in mapping(take(&mut root, "operators")?)? {
            if symbol.is_empty()
                || symbol.len() > 3
                || symbol == "="
                || symbol == "=>"
                || !symbol.chars().all(|c| "+-*/!=<>|&".contains(c))
                || symbol.starts_with("//")
                || symbol.starts_with("/*")
            {
                return Err(error("invalid operator token"));
            }
            let mut node = mapping(node)?;
            let operation = match text(take(&mut node, "operation")?)?.as_str() {
                "or" => Binary::Or,
                "and" => Binary::And,
                "eq" => Binary::Eq,
                "ne" => Binary::Ne,
                "lt" => Binary::Lt,
                "le" => Binary::Le,
                "gt" => Binary::Gt,
                "ge" => Binary::Ge,
                "add" => Binary::Add,
                "sub" => Binary::Sub,
                "mul" => Binary::Mul,
                "divide" => Binary::Divide,
                _ => return Err(error("unknown binary operation")),
            };
            let precedence = integer(take(&mut node, "precedence")?)?;
            if !(1..=32).contains(&precedence) {
                return Err(error("operator precedence must be 1..32"));
            }
            finished(&node)?;
            binary.insert(
                symbol,
                BinarySpec {
                    operation,
                    precedence,
                },
            );
        }
        if binary.len() > 64 {
            return Err(error("too many binary operators"));
        }
        let mut operations = IndexMap::new();
        for (name, node) in mapping(take(&mut root, "operations")?)? {
            if !(identifier(&name) || name.strip_prefix("iter.").is_some_and(identifier))
                || keywords.contains_key(&name)
                || matches!(name.as_str(), "none" | "true" | "false")
            {
                return Err(error("invalid operation name"));
            }
            let mut node = mapping(node)?;
            let (operation, lower, upper) = Operation::read(&text(take(&mut node, "operation")?)?)
                .ok_or_else(|| error("unknown operation/semantics ID"))?;
            let min = integer(take(&mut node, "min")?)?;
            let max = integer(take(&mut node, "max")?)?;
            if min < lower || max > upper || min > max {
                return Err(error("signature exceeds operation semantics"));
            }
            finished(&node)?;
            operations.insert(
                name,
                OperationSpec {
                    operation,
                    min,
                    max,
                },
            );
        }
        if operations.len() > 128 {
            return Err(error("too many operations"));
        }
        finished(&root)?;
        Ok(Self {
            version,
            source: source.into(),
            keywords,
            binary,
            operations,
        })
    }
    pub fn source(&self) -> &str {
        &self.source
    }
    pub fn version(&self) -> u32 {
        self.version
    }
    pub fn iter_namespace(&self) -> bool {
        self.operations.keys().any(|name| name.starts_with("iter."))
    }
    pub fn keyword(&self, name: &str) -> Option<Keyword> {
        self.keywords.get(name).copied()
    }
    pub fn binary(&self, symbol: &str) -> Option<BinarySpec> {
        self.binary.get(symbol).copied()
    }
    pub fn operation(&self, name: &str) -> Option<OperationSpec> {
        self.operations.get(name).copied()
    }
    pub fn operators(&self) -> impl Iterator<Item = &str> {
        self.binary.keys().map(String::as_str)
    }
    /// The lexical productions this package declares; every accepted package declares these four.
    pub fn lexical(&self) -> impl Iterator<Item = (&'static str, &'static str)> {
        LEXICAL.iter().copied()
    }
    /// Each statement keyword with the production it stands for, in the order the package writes them.
    pub fn statements(&self) -> impl Iterator<Item = (&str, Keyword)> {
        self.keywords.iter().map(|(word, k)| (word.as_str(), *k))
    }
    /// Each operator symbol with its operation and precedence, in the order the package writes them.
    pub fn binaries(&self) -> impl Iterator<Item = (&str, BinarySpec)> {
        self.binary.iter().map(|(symbol, s)| (symbol.as_str(), *s))
    }
    pub fn operations(&self) -> impl Iterator<Item = (&str, OperationSpec)> {
        self.operations.iter().map(|(n, s)| (n.as_str(), *s))
    }
    pub fn keywords(&self) -> impl Iterator<Item = &str> {
        self.keywords.keys().map(String::as_str)
    }
}
