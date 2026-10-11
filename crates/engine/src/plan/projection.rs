//! One field-resolution contract for captured execution and presentation values.
use crate::{graph::OutputRef, runtime::RuntimeCode};
use std::borrow::Cow;
use wes_core::{Data, ErrorValue, ValidationIssue, Value, flow::FlowPolicy};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InputProblem {
    SourceUnavailable,
    FieldNotGuaranteed,
    FieldMissing,
    TypeMismatch,
    ConstructorRejected,
    ChargeLimit,
}
impl InputProblem {
    pub fn code(self) -> &'static str {
        match self {
            Self::SourceUnavailable => "INP001",
            Self::FieldNotGuaranteed => "INP002",
            Self::FieldMissing => "INP003",
            Self::TypeMismatch => "INP004",
            Self::ConstructorRejected => "INP005",
            Self::ChargeLimit => "INP006",
        }
    }
    fn explanation(self) -> &'static str {
        match self {
            Self::SourceUnavailable => {
                "The captured source value is unavailable; reading it does not rerun its producer."
            }
            Self::FieldNotGuaranteed => {
                "The declared type does not guarantee this field. Check the whole value against a record contract before selecting it."
            }
            Self::FieldMissing => "The field is absent from the captured value.",
            Self::TypeMismatch => "The selected data does not satisfy its declared field type.",
            Self::ChargeLimit => {
                "The selected input exceeds its captured charge or structural limit; no selected containers were copied."
            }
            Self::ConstructorRejected => {
                "Structured arguments require materialized data within the value budget; management values are not ordinary data."
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct InputResolutionError {
    pub problem: InputProblem,
    pub source: Option<OutputRef>,
    pub fields: Vec<String>,
    policy: FlowPolicy,
    argument_path: Vec<String>,
}
impl InputResolutionError {
    pub(super) fn at_argument_field(mut self, field: &str) -> Self {
        self.argument_path.insert(0, field.to_owned());
        self
    }
    pub(super) fn with_policy(mut self, policy: &FlowPolicy) -> Self {
        self.policy = self.policy.join(policy);
        self
    }

    pub(super) fn constructor(value: &Value) -> Self {
        Self::projection(InputProblem::ConstructorRejected, value, &[])
    }
    pub(super) fn unavailable(source: &OutputRef, fields: &[String]) -> Self {
        Self {
            problem: InputProblem::SourceUnavailable,
            source: Some(source.clone()),
            fields: fields.to_vec(),
            argument_path: vec![],
            policy: FlowPolicy::default(),
        }
    }
    fn projection(problem: InputProblem, value: &Value, fields: &[String]) -> Self {
        Self {
            problem,
            source: None,
            fields: fields.to_vec(),
            argument_path: vec![],
            policy: value.provenance().policy().clone(),
        }
    }
    pub(super) fn with_source(mut self, source: &OutputRef) -> Self {
        self.source = Some(source.clone());
        self
    }
    pub fn message(&self) -> String {
        if self.policy.is_confidential() || self.policy.is_unknown() {
            return "The input is unavailable under its private data policy.".into();
        }
        if self.fields.is_empty() {
            return self.problem.explanation().into();
        }
        let path = self
            .fields
            .iter()
            .map(|field| field.replace('~', "~0").replace('/', "~1"))
            .collect::<Vec<_>>()
            .join("/");
        format!(
            "Field path {}: {}",
            wes_language::calc::diagnostics::label(&format!("/{path}")),
            self.problem.explanation()
        )
    }
    pub fn issue(&self, argument: &str) -> Option<ValidationIssue> {
        if self.policy.is_confidential() || self.policy.is_unknown() {
            return None;
        }
        let escape = |s: &str| s.replace('~', "~0").replace('/', "~1");
        let mut path = format!("/arguments/{}", escape(argument));
        for field in self.argument_path.iter().chain(&self.fields) {
            path.push('/');
            path.push_str(&escape(field));
        }
        Some(ValidationIssue {
            path,
            code: self.problem.code().into(),
            message: self.message(),
        })
    }
    pub fn error(&self, code: RuntimeCode, argument: &str) -> ErrorValue {
        let base = code.error(self.message(), None);
        ErrorValue::new(
            base.id().clone(),
            base.code(),
            self.message(),
            self.issue(argument).into_iter().collect(),
            None,
        )
        .expect("input resolution issue is valid")
        .with_policy(&self.policy)
    }
    pub(crate) fn policy(&self) -> &FlowPolicy {
        &self.policy
    }
}

/// Retain existing admission: Unknown stays dynamic, concrete records never gain fields.
pub fn project_value(value: &Value, fields: &[String]) -> Result<Value, InputResolutionError> {
    project_value_with_limit(value, fields, u64::MAX)
}
/// Preflight the selected immutable payload and its attribution before cloning
/// any record/list containers. The consumer supplies its captured admission.
pub(super) fn project_value_with_limit(
    value: &Value,
    fields: &[String],
    limit: u64,
) -> Result<Value, InputResolutionError> {
    if fields.is_empty() {
        return Ok(value.clone());
    }
    let shape = super::field_shape(value.shape(), fields).ok_or_else(|| {
        InputResolutionError::projection(InputProblem::FieldNotGuaranteed, value, fields)
    })?;
    let mut data = Cow::Borrowed(value.data());
    for field in fields {
        data = match data {
            Cow::Borrowed(Data::Record(values)) => {
                Cow::Borrowed(values.get(field).ok_or_else(|| {
                    InputResolutionError::projection(InputProblem::FieldMissing, value, fields)
                })?)
            }
            Cow::Borrowed(receiver @ Data::Interval(_)) => receiver
                .project_path(std::slice::from_ref(field))
                .ok_or_else(|| {
                    InputResolutionError::projection(InputProblem::FieldMissing, value, fields)
                })?,
            Cow::Owned(receiver @ Data::Interval(_)) => Cow::Owned(
                receiver
                    .project_path(std::slice::from_ref(field))
                    .ok_or_else(|| {
                        InputResolutionError::projection(InputProblem::FieldMissing, value, fields)
                    })?
                    .into_owned(),
            ),
            _ => {
                return Err(InputResolutionError::projection(
                    InputProblem::TypeMismatch,
                    value,
                    fields,
                ));
            }
        };
    }
    let shell = crate::value_size::value_shell_charge(value, limit);
    let payload = crate::value_size::data_charge(&data, limit);
    if shell
        .zip(payload)
        .and_then(|(shell, payload)| shell.checked_add(payload))
        .is_none_or(|charge| charge > limit)
    {
        return Err(InputResolutionError::projection(
            InputProblem::ChargeLimit,
            value,
            fields,
        ));
    }
    Value::new(shape, data.into_owned(), value.provenance().clone())
        .map(|v| {
            v.with_metadata(value.metadata().and_then(|m| {
                m.project(
                    &fields
                        .iter()
                        .map(|f| wes_core::contracts::metadata::field_segment(f))
                        .collect::<String>(),
                )
            }))
        })
        .map_err(|_| InputResolutionError::projection(InputProblem::TypeMismatch, value, fields))
}
