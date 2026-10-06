//! Application-owned document conversion, scheduled through the normal finite-work lifecycle.
use crate::{
    driver::{CancellationToken, ExecutionFuture},
    plan::{Input, MetaTask},
    providers::{InvocationError, InvocationFuture},
    runtime::{Outcome, RuntimeCode},
};
use std::sync::Arc;
use wes_core::Data;
use wes_language::{Diagnostic, Span};

#[derive(Clone, Debug)]
pub struct DescribeRequest {
    pub location: String,
    pub is_url: bool,
    pub provider: String,
    pub out: Option<String>,
}
pub trait DescribeService: Send + Sync + 'static {
    /// Own and join physical work on every return path. Never register or invoke a provider.
    fn describe(
        &self,
        request: DescribeRequest,
        cancellation: CancellationToken,
    ) -> InvocationFuture;
}
#[derive(Clone)]
pub struct BoundDescribe {
    request: DescribeRequest,
    service: Arc<dyn DescribeService>,
}
impl std::fmt::Debug for BoundDescribe {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundDescribe").finish_non_exhaustive()
    }
}
impl BoundDescribe {
    /// Payload-independent workflow information for a caller who may read this source.
    /// It is meaningful only after successful completion; never inspect private result data.
    pub fn public_completion(&self) -> &'static str {
        if self.request.out.is_some() {
            "Description completed, including the requested export. Its result remains private. Review the saved draft in /spec and explicitly share that draft before using MCP spec_list/spec_read."
        } else {
            "Description completed. Its result remains private. Review the saved draft in /spec and explicitly share that draft before using MCP spec_list/spec_read. No export was requested."
        }
    }
    pub(crate) fn bind(
        task: MetaTask,
        service: Option<Arc<dyn DescribeService>>,
        span: Span,
    ) -> Result<Self, Diagnostic> {
        let fail = |message| Diagnostic::error("DSC001", span, message);
        let text = |name: &str| -> Result<Option<String>, Diagnostic> {
            match task.inputs.get(name) {
                None => Ok(None),
                Some(Input::Literal(value)) => match value.data() {
                    Data::Text(value)
                        if !value.is_empty() && !value.chars().any(char::is_control) =>
                    {
                        Ok(Some(value.to_string()))
                    }
                    _ => Err(fail("describe arguments must be nonempty literal text")),
                },
                _ => Err(fail(
                    "describe takes explicit literal sources, not workspace values",
                )),
            }
        };
        let url = text("url")?;
        let file = text("file")?;
        if url.is_some() == file.is_some() {
            return Err(fail("describe needs exactly one of url: or file:"));
        }
        let provider = text("provider")?.ok_or_else(|| fail("describe needs provider:"))?;
        if !provider
            .bytes()
            .enumerate()
            .all(|(i, b)| b.is_ascii_alphabetic() || b == b'_' || (i > 0 && b.is_ascii_digit()))
            || provider.len() > 100
        {
            return Err(fail(
                "provider must be an identifier of at most 100 ASCII characters",
            ));
        }
        Ok(Self {
            request: DescribeRequest {
                is_url: url.is_some(),
                location: url.or(file).expect("one source"),
                provider,
                out: text("out")?,
            },
            service: service.ok_or_else(|| fail("describe service is unavailable in this host"))?,
        })
    }
    pub(crate) fn execute(self, cancellation: CancellationToken) -> ExecutionFuture {
        Box::pin(async move {
            match self.service.describe(self.request, cancellation).await {
                // Privacy is the operation contract, independent of which host implements
                // conversion. Host output cannot implicitly declassify this artifact.
                Ok(value) => Outcome::Produced(
                    value.with_provenance(
                        value
                            .provenance()
                            .clone()
                            .with_policy(&wes_core::flow::FlowPolicy::default().private()),
                    ),
                ),
                Err(InvocationError::Failed(error)) => Outcome::Failed(error),
                Err(InvocationError::Cancelled) => Outcome::Cancelled(
                    RuntimeCode::Cancelled
                        .error("API import cancelled; no draft was published.", None),
                ),
            }
            .into()
        })
    }
}
