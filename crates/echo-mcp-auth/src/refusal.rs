//! What one refused link request may say about itself: never a body, id or URL.
use crate::AuthError;
use reqwest::StatusCode;

/// Sanitized facts about the link step that failed last, for diagnostics only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LinkFailure {
    /// `registration`, `device_authorization` or `token`.
    pub step: &'static str,
    /// The HTTP status of the reply; absent when no reply arrived.
    pub http_status: Option<u16>,
    /// The OAuth `error` code of a refusal, or `unrecognized` for unusual text.
    pub oauth_error: Option<String>,
}

#[derive(Debug)]
pub(crate) struct Refusal {
    pub error: AuthError,
    status: Option<u16>,
    code: Option<String>,
}

impl Refusal {
    pub fn unsent(error: AuthError) -> Self {
        Self {
            error,
            status: None,
            code: None,
        }
    }

    pub fn status(error: AuthError, status: StatusCode) -> Self {
        Self {
            error,
            status: Some(status.as_u16()),
            code: None,
        }
    }

    pub fn coded(error: AuthError, status: StatusCode, code: &str) -> Self {
        Self {
            error,
            status: Some(status.as_u16()),
            code: Some(sanitize(code)),
        }
    }

    pub fn at(self, step: &'static str) -> (LinkFailure, AuthError) {
        let failure = LinkFailure {
            step,
            http_status: self.status,
            oauth_error: self.code,
        };
        (failure, self.error)
    }
}

impl From<Refusal> for AuthError {
    fn from(refusal: Refusal) -> Self {
        refusal.error
    }
}

/// OAuth error codes are short lower-case words; anything else could carry
/// server text, so it is replaced rather than recorded.
fn sanitize(code: &str) -> String {
    let word = !code.is_empty() && code.len() <= 48 && code.bytes().all(|b| b.is_ascii_lowercase() || b == b'_');
    if word { code.into() } else { "unrecognized".into() }
}
