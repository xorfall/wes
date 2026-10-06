//! Public categories are producer-authored. Never classify untrusted messages by their text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImportWarningKind {
    Advisory,
    AuthenticationChoice,
    CredentialUnavailable,
    QueryCredential,
    AuthenticationUndocumented,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportWarning {
    pub kind: ImportWarningKind,
    pub message: String,
}
impl ImportWarning {
    pub fn new(kind: ImportWarningKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }
    pub fn diagnostic(&self, span: wes_language::Span) -> wes_language::Diagnostic {
        use ImportWarningKind::*;
        let (code, message) = match self.kind {
            Advisory => (
                "IMP002",
                "Import includes advisory notes. Read provider information or review the spec for permitted details; this warning alone does not mean failure.",
            ),
            AuthenticationChoice => (
                "IMP007",
                "An operation requires an explicit authentication choice. Inspect provider authentication information and select its schemes in environment bind.auth before calling it.",
            ),
            CredentialUnavailable => (
                "IMP008",
                "A declared credential is unavailable. Configure its credential reference and the required grant before calling a protected operation; import does not grant credential access.",
            ),
            QueryCredential => (
                "IMP009",
                "Authentication uses a query parameter; credential material can appear in server or proxy URL logs. Prefer header authentication when the API supports it.",
            ),
            AuthenticationUndocumented => (
                "IMP010",
                "Authentication is not documented. No credentials are attached; this does not establish anonymous or public access. Review the API authentication requirements before using protected operations.",
            ),
        };
        wes_language::Diagnostic::error(code, span, &self.message)
            .with_severity(wes_language::Severity::Warning)
            .with_public_message(message)
    }
}
impl From<String> for ImportWarning {
    fn from(message: String) -> Self {
        Self::new(ImportWarningKind::Advisory, message)
    }
}
impl std::ops::Deref for ImportWarning {
    type Target = str;
    fn deref(&self) -> &str {
        &self.message
    }
}
impl PartialEq<&str> for ImportWarning {
    fn eq(&self, other: &&str) -> bool {
        self.message == *other
    }
}
