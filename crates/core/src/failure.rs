use crate::{Data, Primitive, Provenance, RecordShape, Shape, Value};
use std::{fmt, sync::Arc};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationIssue {
    pub path: String,
    pub code: String,
    pub message: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ErrorId(Arc<str>);
impl ErrorId {
    pub fn new(id: impl AsRef<str>) -> Result<Self, InvalidError> {
        if id.as_ref().trim().is_empty() {
            return Err(InvalidError("an error id must not be blank"));
        }
        Ok(Self(Arc::from(id.as_ref())))
    }
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
impl fmt::Display for ErrorId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Clone, Copy, Debug, Error, PartialEq, Eq)]
#[error("{0}")]
pub struct InvalidError(&'static str);

/// Portable failure data. Its identity is supplied by the execution boundary, not by observers.
#[derive(Clone, PartialEq, Eq)]
pub struct ErrorValue {
    policy: Arc<crate::flow::FlowPolicy>,
    id: ErrorId,
    code: String,
    message: String,
    issues: Vec<ValidationIssue>,
    cause: Option<ErrorId>,
    locations: Vec<crate::SourceLocation>,
}
impl ErrorValue {
    /// Remote execution may have started; transport loss cannot prove its final outcome.
    /// Shared by all execution drivers and retained even after node cancellation.
    pub const REMOTE_OUTCOME_UNKNOWN: &'static str = "ENV036";

    pub fn new(
        id: ErrorId,
        code: impl Into<String>,
        message: impl Into<String>,
        issues: Vec<ValidationIssue>,
        cause: Option<ErrorId>,
    ) -> Result<Self, InvalidError> {
        let code = code.into();
        if code.trim().is_empty() {
            return Err(InvalidError("an error code must not be blank"));
        }
        if cause.as_ref() == Some(&id) {
            return Err(InvalidError("an error cannot cause itself"));
        }
        for issue in &issues {
            if issue.code.trim().is_empty()
                || (!issue.path.is_empty() && !issue.path.starts_with('/'))
            {
                return Err(InvalidError(
                    "issues require a code and a JSON Pointer path",
                ));
            }
        }
        Ok(Self {
            policy: Arc::default(),
            id,
            code,
            message: message.into(),
            issues,
            cause,
            locations: vec![],
        })
    }
    pub fn id(&self) -> &ErrorId {
        &self.id
    }
    pub fn with_policy(mut self, policy: &crate::flow::FlowPolicy) -> Self {
        self.policy = Arc::new(self.policy.join(policy));
        if self.policy.is_confidential() {
            if self.code == Self::REMOTE_OUTCOME_UNKNOWN {
                self.message = "Private remote operation outcome is unknown; remote work may still be running.".into();
            } else {
                self.message = "Private operation failed; details withheld.".into();
                self.code = "ENV021".into();
            }
            self.locations.clear();
            self.issues.clear();
            self.cause = None;
        }
        self
    }
    pub fn code(&self) -> &str {
        &self.code
    }
    pub fn policy(&self) -> &crate::flow::FlowPolicy {
        &self.policy
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn locations(&self) -> &[crate::SourceLocation] {
        &self.locations
    }
    pub fn with_locations(
        mut self,
        locations: Vec<crate::SourceLocation>,
    ) -> Result<Self, InvalidError> {
        if locations.len() > 17 || locations.iter().any(|location| !location.valid()) {
            return Err(InvalidError("invalid or excessive source locations"));
        }
        if !self.policy.is_confidential() {
            self.locations = locations;
        }
        Ok(self)
    }
    pub fn issues(&self) -> &[ValidationIssue] {
        &self.issues
    }
    pub fn cause(&self) -> Option<&ErrorId> {
        self.cause.as_ref()
    }
    pub fn issue_shape() -> Shape {
        Shape::Record(
            RecordShape::new(
                "ValidationIssue",
                ["path", "code", "message"]
                    .map(|name| (name.into(), Shape::Primitive(Primitive::Text))),
            )
            .expect("constant issue fields"),
        )
    }
    pub fn shape() -> Shape {
        Self::named_shape("Error")
    }
    pub fn cancellation_shape() -> Shape {
        Self::named_shape("Cancellation")
    }
    fn named_shape(name: &str) -> Shape {
        Shape::Record(
            RecordShape::new(
                name,
                [
                    ("id".into(), Shape::Primitive(Primitive::Text)),
                    ("code".into(), Shape::Primitive(Primitive::Text)),
                    ("message".into(), Shape::Primitive(Primitive::Text)),
                    ("issues".into(), Shape::List(Box::new(Self::issue_shape()))),
                    (
                        "locations".into(),
                        Shape::List(Box::new(crate::SourceLocation::shape())),
                    ),
                    ("causeId".into(), Shape::Primitive(Primitive::Text)),
                ],
            )
            .expect("constant error fields"),
        )
    }
    pub fn to_value(&self) -> Value {
        self.value(Self::shape())
    }
    pub fn to_cancellation_value(&self) -> Value {
        self.value(Self::cancellation_shape())
    }
    fn value(&self, shape: Shape) -> Value {
        let data = Data::Record(
            [
                (
                    "locations".into(),
                    Data::List(
                        self.locations
                            .iter()
                            .map(crate::SourceLocation::data)
                            .collect(),
                    ),
                ),
                ("id".into(), Data::Text(self.id.to_string().into())),
                ("code".into(), Data::Text(self.code.as_str().into())),
                ("message".into(), Data::Text(self.message.as_str().into())),
                (
                    "issues".into(),
                    Data::List(
                        self.issues
                            .iter()
                            .map(|issue| {
                                Data::Record(
                                    [
                                        ("path".into(), Data::Text(issue.path.as_str().into())),
                                        ("code".into(), Data::Text(issue.code.as_str().into())),
                                        (
                                            "message".into(),
                                            Data::Text(issue.message.as_str().into()),
                                        ),
                                    ]
                                    .into(),
                                )
                            })
                            .collect(),
                    ),
                ),
                (
                    "causeId".into(),
                    Data::Text(
                        self.cause
                            .as_ref()
                            .map_or_else(String::new, ToString::to_string)
                            .into(),
                    ),
                ),
            ]
            .into(),
        );
        Value::new(shape, data, Provenance::default().with_policy(&self.policy))
            .expect("error data matches its fixed schema")
    }
}
impl fmt::Debug for ErrorValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut d = f.debug_struct("ErrorValue");
        d.field("id", &self.id)
            .field("code", &self.code)
            .field("message", &self.message)
            .field("issues", &self.issues)
            .field("cause", &self.cause)
            .field("locations", &self.locations);
        if !self.policy.is_empty() {
            d.field("policy", &self.policy);
        }
        d.finish()
    }
}
