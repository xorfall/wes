use crate::Span;
use std::borrow::Cow;

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Severity {
    Error,
    Warning,
    Info,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Diagnostic {
    pub code: Cow<'static, str>,
    pub message: String,
    pub span: Span,
    pub severity: Severity,
    pub hints: Vec<String>,
    /// Producer-authored, payload-independent explanation. Never copy inferred values here.
    pub public_message: Option<std::sync::Arc<String>>,
}

impl Diagnostic {
    pub fn error(
        code: impl Into<Cow<'static, str>>,
        span: Span,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code: code.into(),
            span,
            message: message.into(),
            severity: Severity::Error,
            hints: Vec::new(),
            public_message: None,
        }
    }
    /// Payload-independent explanation for external readers. Detailed messages can
    /// contain inferred private types or provider validation data; source access
    /// does not authorize exporting those inferred values.
    pub fn public_summary(&self) -> &str {
        if let Some(message) = &self.public_message
            && self.valid_public_message()
        {
            return message;
        }
        match self.severity {
            Severity::Info => return "Informational notice; this is not an error.",
            Severity::Warning => return "Warning; this diagnostic alone does not indicate failure.",
            Severity::Error => {}
        }
        match self.code.as_ref() {
            "PLN001" => {
                "Referenced name is not defined in this workspace. Check the name or values_list."
            }
            "PLN002" => "One execution cannot consume multiple output ports from the same node.",
            "PLN003" => "This action has no output to bind to a name.",
            "PLN004" => "This operation requires a bound workspace output reference.",
            "RES001" => {
                "Command name is ambiguous between a provider and a meta command; use the colon for meta commands."
            }
            "RES002" => "Unknown meta command. Use help to discover commands.",
            "RES003" => "This command is reserved but not implemented.",
            "CHK015" => {
                "Unknown subcommand, provider or registry. Check help and the selected environment."
            }
            "RES004" => {
                "Provider or command is absent from the selected catalogue. Check the name and selected environment."
            }
            "RES005" => {
                "Capability path is absent from this provider. Use provider help to discover operations."
            }
            "CHK001" => {
                "Unsupported parameter, subcommand or argument form. Use help for the command signature."
            }
            "CHK002" => "A required argument is missing. Use help for the command signature.",
            "AUT001" => {
                "Operation affects protected work or shared definitions; change scope is required."
            }
            code if code.starts_with("LEX") || code.starts_with("PAR") => {
                "Source syntax is invalid at this span. Use validate for syntax details."
            }
            code if code.starts_with("CAL") => {
                "Calculation analysis failed at this span. Check referenced names, fields, types and supported operations."
            }
            code if code.starts_with("TYP") => {
                "Type or contract validation failed at this span. Check the declared contract and supplied value."
            }
            code if code.starts_with("CHK") => {
                "Command arguments violate its signature or rules. Use help for the supported form."
            }
            code if code.starts_with("IMP") => {
                "Provider import could not be prepared. Check the spec path and import parameters."
            }
            code if code.starts_with("ENV") => {
                "Environment operation could not be applied. Check its selection, plan and authority."
            }
            _ => {
                "Operation was rejected at this span. Detailed workspace diagnostics may contain data that is not exportable."
            }
        }
    }
    /// The producer must supply payload-independent text; never interpolate source or inferred data.
    pub fn with_public_message(mut self, message: impl Into<String>) -> Self {
        self.public_message = Some(std::sync::Arc::new(message.into()));
        self
    }
    pub fn valid_public_message(&self) -> bool {
        self.public_message
            .as_ref()
            .is_none_or(|message| !message.trim().is_empty() && message.len() <= 512)
    }
    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hints.push(hint.into());
        self
    }
    pub fn with_severity(mut self, severity: Severity) -> Self {
        self.severity = severity;
        self
    }
}
