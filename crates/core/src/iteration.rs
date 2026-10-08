//! Serializable immutable traversal descriptions. Cursors and effects belong to the engine.
use crate::{
    Data, Primitive, Shape, Value,
    contracts::{Contract, ContractRegistry},
};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IterMode {
    Items,
    Lines,
    Chars,
    Words,
    Split,
    RegexSplit,
    Matches,
    Captures,
    Keys,
    Values,
    Entries,
    JsonLines,
}
impl IterMode {
    pub fn name(self) -> &'static str {
        match self {
            Self::Items => "items",
            Self::Lines => "lines",
            Self::Chars => "chars",
            Self::Words => "words",
            Self::Split => "split",
            Self::RegexSplit => "regex-split",
            Self::Matches => "matches",
            Self::Captures => "captures",
            Self::Keys => "keys",
            Self::Values => "values",
            Self::Entries => "entries",
            Self::JsonLines => "json-lines",
        }
    }
    pub fn capture_shape() -> Shape {
        Shape::Record(
            crate::RecordShape::new(
                "",
                [
                    ("match".into(), Shape::Primitive(Primitive::Text)),
                    (
                        "groups".into(),
                        Shape::List(Box::new(Shape::Option(Box::new(Shape::Primitive(
                            Primitive::Text,
                        ))))),
                    ),
                ],
            )
            .expect("capture fields"),
        )
    }
    pub fn argument_kind(self) -> Option<&'static str> {
        match self {
            Self::Split => Some("delimiter"),
            Self::RegexSplit | Self::Matches | Self::Captures => Some("pattern"),
            _ => None,
        }
    }
    pub fn expected_source(self) -> &'static str {
        match self {
            Self::Items => "List",
            Self::Keys | Self::Values | Self::Entries => "Record",
            _ => "Text",
        }
    }
    pub fn accepts_source(self, shape: &Shape) -> bool {
        matches!(shape, Shape::Unknown)
            || match self {
                Self::Items => matches!(shape, Shape::List(_)),
                Self::Keys | Self::Values | Self::Entries => matches!(shape, Shape::Record(_)),
                _ => *shape == Shape::Primitive(Primitive::Text),
            }
    }
    pub const ALL: &'static [Self] = &[
        Self::Items,
        Self::Lines,
        Self::Chars,
        Self::Words,
        Self::Split,
        Self::RegexSplit,
        Self::Matches,
        Self::Captures,
        Self::Keys,
        Self::Values,
        Self::Entries,
        Self::JsonLines,
    ];
    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.iter().copied().find(|mode| mode.name() == s)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ContractCapture {
    pub name: String,
    pub packages: Vec<String>,
}
impl ContractCapture {
    pub fn resolve(&self) -> Result<Arc<Contract>, String> {
        if self.packages.len() > 1000
            || self.packages.iter().map(String::len).sum::<usize>() > 1024 * 1024
        {
            return Err("captured type packages exceed Iter limits".into());
        }
        let mut registry = ContractRegistry::new();
        for source in &self.packages {
            registry.load(source).map_err(|e| e.message)?;
        }
        registry.resolve(&self.name).map_err(|e| e.message)
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum IterStage {
    Take(u64),
    Skip(u64),
    Field(String),
    Check(ContractCapture),
}
/// Typed traversal-plan rejection. Consumers classify the cause, never message text.
#[derive(Clone, Debug, thiserror::Error)]
pub enum IterPlanError {
    #[error("{0}")]
    Type(String),
    #[error("{0}")]
    Value(String),
    #[error("{0}")]
    Parse(String),
    #[error("{0}")]
    Contract(String),
    #[error("{0}")]
    Limit(String),
}
/// Calculation-owned cache: no global/private pattern retention across runs.
pub struct IterRegexCache {
    entries: std::collections::VecDeque<Arc<regex::Regex>>,
    capacity: usize,
    compilations: u64,
    hits: u64,
}
impl Default for IterRegexCache {
    fn default() -> Self {
        Self::with_capacity(4).expect("default cache capacity")
    }
}
/// Content-free counters scoped to one execution owner, never serialized with a value.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RegexCacheUsage {
    pub compilations: u64,
    pub hits: u64,
    pub entries: usize,
}
impl IterRegexCache {
    pub fn with_capacity(capacity: usize) -> Result<Self, IterPlanError> {
        if !(1..=64).contains(&capacity) {
            return Err(IterPlanError::Limit(
                "regex cache capacity must be within 1..=64".into(),
            ));
        }
        Ok(Self {
            entries: Default::default(),
            capacity,
            compilations: 0,
            hits: 0,
        })
    }
    /// VM estimate, not actual RSS or the regex library's peak allocator usage.
    pub const COMPILED_CHARGE: u64 = 1280 * 1024;
    pub fn usage(&self) -> RegexCacheUsage {
        RegexCacheUsage {
            compilations: self.compilations,
            hits: self.hits,
            entries: self.entries.len(),
        }
    }
    pub fn contains(&self, pattern: &str) -> bool {
        self.entries.iter().any(|r| r.as_str() == pattern)
    }
    /// The owner admits compilation work/memory before calling this bounded compiler.
    pub fn compile(&mut self, pattern: &str) -> Result<Arc<regex::Regex>, IterPlanError> {
        if pattern.len() > 16 * 1024 {
            return Err(IterPlanError::Limit(
                "Iter pattern exceeds its 16384-byte limit".into(),
            ));
        }
        if let Some(index) = self.entries.iter().position(|r| r.as_str() == pattern) {
            self.hits = self.hits.saturating_add(1);
            let found = self.entries.remove(index).expect("cache entry");
            self.entries.push_back(found.clone());
            return Ok(found);
        }
        self.compilations = self.compilations.saturating_add(1);
        let compiled = Arc::new(
            regex::RegexBuilder::new(pattern)
                .size_limit(1024 * 1024)
                .dfa_size_limit(256 * 1024)
                .build()
                .map_err(|error| match error {
                    regex::Error::Syntax(message)=>IterPlanError::Parse(format!("invalid Iter regex: {}",message.lines().rev().find(|line|!line.trim().is_empty()).unwrap_or("invalid syntax").chars().take(512).collect::<String>())),
                    regex::Error::CompiledTooBig(limit)=>IterPlanError::Limit(format!("Iter regex exceeds its compiled size limit ({limit} bytes); simplify the pattern")),
                    _=>IterPlanError::Parse("invalid Iter regex".into()),
                })?,
        );
        if self.entries.len() == self.capacity {
            self.entries.pop_front();
        }
        self.entries.push_back(compiled.clone());
        Ok(compiled)
    }
}
#[derive(Clone)]
pub struct IterValue {
    source: Value,
    mode: IterMode,
    argument: Option<String>,
    stages: Vec<IterStage>,
    item_shape: Shape,
    source_item_shape: Shape,
    regex: Option<std::sync::Weak<regex::Regex>>,
}
// Compiled state is not serialized identity/equality/debug evidence.
impl std::fmt::Debug for IterValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IterValue")
            .field("source", &self.source)
            .field("mode", &self.mode)
            .field("argument", &self.argument)
            .field("stages", &self.stages)
            .field("item_shape", &self.item_shape)
            .finish()
    }
}
impl PartialEq for IterValue {
    fn eq(&self, other: &Self) -> bool {
        self.source == other.source
            && self.mode == other.mode
            && self.argument == other.argument
            && self.stages == other.stages
            && self.item_shape == other.item_shape
    }
}
impl Eq for IterValue {}
impl IterValue {
    pub fn new(
        source: Value,
        mode: IterMode,
        argument: Option<String>,
        stages: Vec<IterStage>,
    ) -> Result<Self, IterPlanError> {
        Self::new_cached(
            source,
            mode,
            argument,
            stages,
            &mut IterRegexCache::default(),
        )
    }
    pub fn new_cached(
        source: Value,
        mode: IterMode,
        argument: Option<String>,
        stages: Vec<IterStage>,
        cache: &mut IterRegexCache,
    ) -> Result<Self, IterPlanError> {
        let mut regex = None;
        if stages.len() > 64 {
            return Err(IterPlanError::Limit(
                "Iter pipeline exceeds 64 stages".into(),
            ));
        }
        let pattern = mode.argument_kind().is_some();
        if pattern != argument.is_some() {
            return Err(IterPlanError::Value(
                "unexpected or missing Iter extraction argument".into(),
            ));
        }
        if let Some(s) = &argument {
            if mode == IterMode::Split && s.is_empty() {
                return Err(IterPlanError::Value("iter.split delimiter must not be empty; use iter.chars for individual characters".into()));
            }
            if s.len() > 16 * 1024 {
                return Err(IterPlanError::Limit(
                    "Iter pattern exceeds its 16384-byte limit".into(),
                ));
            }

            if mode != IterMode::Split {
                regex = Some(Arc::downgrade(&cache.compile(s)?));
            }
        }
        if !source.data().is_materialized() {
            return Err(IterPlanError::Type(
                "Iter source requires bounded materialized data".into(),
            ));
        }
        let text = Shape::Primitive(Primitive::Text);
        let mut item_shape = match (mode, source.data()) {
            (IterMode::Items, Data::List(_)) => match source.shape() {
                Shape::List(s) => *s.clone(),
                _ => Shape::Unknown,
            },
            (IterMode::Keys, Data::Record(_)) => text.clone(),
            (IterMode::Values | IterMode::Entries, Data::Record(_)) => Shape::Unknown,
            (
                IterMode::Lines
                | IterMode::Chars
                | IterMode::Words
                | IterMode::Split
                | IterMode::RegexSplit
                | IterMode::Matches,
                Data::Text(_),
            ) => text,
            (IterMode::JsonLines, Data::Text(_)) => Shape::Unknown,
            (IterMode::Captures, Data::Text(_)) => IterMode::capture_shape(),
            _ => {
                let received = match source.data() {
                    Data::Text(_) => "Text",
                    Data::Int(_) => "Int",
                    Data::Decimal(_) => "Decimal",
                    Data::Bool(_) => "Bool",
                    Data::Bytes(_) => "Bytes",
                    Data::List(_) => "List",
                    Data::Record(_) => "Record",
                    Data::Option(_) => "Option",
                    Data::Iter(_) => "Iter",
                    Data::Instant(_) => "Instant",
                    Data::Duration(_) => "Duration",
                    Data::Interval(_) => "Interval",
                };
                return Err(IterPlanError::Type(format!(
                    "iter.{} expects {}; received {received}",
                    mode.name(),
                    mode.expected_source()
                )));
            }
        };
        let source_item_shape = item_shape.clone();
        for stage in &stages {
            match stage {
                IterStage::Check(c) => {
                    item_shape = c.resolve().map_err(IterPlanError::Contract)?.shape()
                }
                IterStage::Field(name) => {
                    if name.len() > 4096 {
                        return Err(IterPlanError::Limit("Iter field name exceeds limit".into()));
                    }
                    item_shape = Shape::Unknown;
                }
                _ => {}
            }
        }
        Ok(Self {
            source,
            mode,
            argument,
            stages,
            item_shape,
            source_item_shape,
            regex,
        })
    }
    /// A plan never retains compiled memory beyond its execution owner. A deserialized or
    /// evicted plan reconstructs through the same bounded compiler when a cursor opens it.
    pub fn compiled_regex(&self) -> Result<Option<Arc<regex::Regex>>, String> {
        let Some(cached) = &self.regex else {
            return Ok(None);
        };
        if let Some(regex) = cached.upgrade() {
            return Ok(Some(regex));
        }
        IterRegexCache::default()
            .compile(self.argument.as_deref().expect("validated regex argument"))
            .map(Some)
            .map_err(|error| error.to_string())
    }
    pub fn item_contract(&self) -> Option<Arc<Contract>> {
        for stage in self.stages.iter().rev() {
            match stage {
                IterStage::Check(c) => return c.resolve().ok(),
                IterStage::Field(_) => return ContractRegistry::new().resolve("Unknown").ok(),
                _ => {}
            }
        }
        ContractRegistry::new()
            .resolve(&self.item_shape.to_string())
            .ok()
    }
    pub fn source(&self) -> &Value {
        &self.source
    }
    pub fn mode(&self) -> IterMode {
        self.mode
    }
    pub fn argument(&self) -> Option<&str> {
        self.argument.as_deref()
    }
    pub fn stages(&self) -> &[IterStage] {
        &self.stages
    }
    /// Shape before traversal stages; a cursor must not apply a later check early.
    pub fn source_item_shape(&self) -> &Shape {
        &self.source_item_shape
    }
    pub fn item_shape(&self) -> &Shape {
        &self.item_shape
    }
    pub fn with_stage(&self, stage: IterStage) -> Result<Self, IterPlanError> {
        let mut stages = self.stages.clone();
        stages.push(stage);
        Self::new(
            self.source.clone(),
            self.mode,
            self.argument.clone(),
            stages,
        )
    }
}
#[derive(Clone, Debug)]
pub struct IterRecipe {
    pub input: Arc<Contract>,
    pub output: Arc<Contract>,
    pub mode: IterMode,
    pub argument: Option<String>,
}
