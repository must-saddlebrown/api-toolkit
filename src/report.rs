//! Report model and writer (stub; S7 adds the rest). Only `TransportErrorCode` exists
//! so far, because the transport (S6) needs it.

use std::fmt;

use serde::Serialize;

/// Sanitized reason a request produced no usable response. It never carries a URL,
/// header value or library message (PLAN §10). Serialized as stable snake_case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TransportErrorCode {
    Timeout,
    ConnectFailed,
    TlsFailed,
    BodyTooLarge,
    BodyReadFailed,
    RequestInvalid,
    Other,
}

impl TransportErrorCode {
    /// The same string serde produces; used for `Display` so messages stay stable.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportErrorCode::Timeout => "timeout",
            TransportErrorCode::ConnectFailed => "connect_failed",
            TransportErrorCode::TlsFailed => "tls_failed",
            TransportErrorCode::BodyTooLarge => "body_too_large",
            TransportErrorCode::BodyReadFailed => "body_read_failed",
            TransportErrorCode::RequestInvalid => "request_invalid",
            TransportErrorCode::Other => "other",
        }
    }
}

impl fmt::Display for TransportErrorCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
