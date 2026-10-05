//! Bounded resumable calculation. Local instructions never write history or invoke providers.
mod machine;
mod operations;
mod temporal;
mod value;
pub use machine::{Machine, Request, Step};
use wes_core::{ErrorValue, ValidationIssue};
use wes_language::Span;

#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub work: u64,
    pub bytes: u64,
    pub frames: usize,
    pub calls: u64,
    pub quantum: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            work: wes_budgets::get("calc.work") as u64,
            bytes: wes_budgets::get("calc.bytes") as u64,
            frames: wes_budgets::get("calc.frames") as usize,
            calls: wes_budgets::get("calc.calls") as u64,
            quantum: wes_budgets::get("calc.quantum") as usize,
        }
    }
}
#[derive(Clone, Debug)]
pub struct Failure {
    pub code: &'static str,
    pub message: String,
    pub span: Span,
    pub issues: Vec<ValidationIssue>,
    pub cause: Option<Box<ErrorValue>>,
    pub cancelled: bool,
    pub trace: Vec<Span>,
    pub policy: wes_core::flow::FlowPolicy,
}
impl std::fmt::Display for Failure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}
impl std::error::Error for Failure {}
impl Failure {
    pub fn new(code: &'static str, span: Span, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            span,
            issues: vec![],
            cause: None,
            cancelled: false,
            trace: vec![],
            policy: wes_core::flow::FlowPolicy::default(),
        }
    }
    pub fn iteration(error: wes_core::IterPlanError, span: Span) -> Self {
        use wes_core::IterPlanError;
        use wes_language::calc::diagnostics::Category;
        let code = match &error {
            IterPlanError::Type(_) => "CAL004",
            IterPlanError::Value(_) => "CAL005",
            IterPlanError::Limit(_) => "CAL006",
            IterPlanError::Parse(_) => Category::Parse.code(),
            IterPlanError::Contract(_) => Category::Contract.code(),
        };
        Self::new(code, span, error.to_string())
    }
    pub fn categorized(
        category: wes_language::calc::diagnostics::Category,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Self::new(category.code(), span, message)
    }
    pub fn cancelled(span: Span) -> Self {
        Self {
            cancelled: true,
            ..Self::new("CAL007", span, "calculation cancelled")
        }
    }
    pub fn provider(span: Span, error: ErrorValue) -> Self {
        // The cause notice retains its identity. Carry its safe public explanation as
        // well, so consumers of the outer failure do not need a second lookup.
        let message = format!(
            "calculation provider call failed: {}: {}",
            error.code(),
            error.message().chars().take(4096).collect::<String>()
        );
        Self {
            issues: error.issues().iter().take(12).cloned().collect(),
            cause: Some(Box::new(error)),
            ..Self::new("CAL008", span, message)
        }
    }
}
struct Budget {
    token: crate::driver::CancellationToken,
    left: u64,
    work_limit: u64,
    charged: u64,
    limit: u64,
}
impl Budget {
    fn work(&mut self, n: u64, span: Span) -> Result<(), Failure> {
        if self.token.is_cancelled() {
            return Err(Failure::cancelled(span));
        }
        self.left = self
            .left
            .checked_sub(n)
            .ok_or_else(|| Failure::new("CAL006", span, format!("calculation work limit reached ({} work units). Reduce the input or split the calculation; work units count evaluated operations, not loop iterations.",self.work_limit)))?;
        Ok(())
    }
    fn allocate(&mut self, n: u64, span: Span) -> Result<(), Failure> {
        self.charged = self
            .charged
            .checked_add(n)
            .filter(|n| *n <= self.limit)
            .ok_or_else(|| {
                Failure::new(
                    "CAL006",
                    span,
                    format!("calculation retained allocation limit reached ({} bytes). Bound the input or split the calculation.", self.limit),
                )
            })?;
        Ok(())
    }
}

mod host;
pub use host::{BoundCalculation, HttpOperation, LocalServices};
mod iteration;

/// Safe metadata only. Never export source spans, literals, provider or dependency names.
pub(crate) fn describe_purity(
    compiled: &wes_language::calc::Compiled,
) -> indexmap::IndexMap<String, wes_core::Data> {
    use wes_core::Data;
    indexmap::IndexMap::from([
        (
            "purity".into(),
            Data::Text(
                if compiled.effectful() {
                    "effectful"
                } else {
                    "pure"
                }
                .into(),
            ),
        ),
        (
            "requiresPure".into(),
            Data::Bool(compiled.program.requires_pure),
        ),
        (
            "purityReason".into(),
            Data::Text(compiled.purity_reason().into()),
        ),
    ])
}

mod local;
pub use local::LocalError;
