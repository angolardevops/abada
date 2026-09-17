//! Turning an HTTP request into the gRPC request message, as the code
//! `protoc-gen-grpc-gateway` v2.27.3 generates does it, and the part of
//! `runtime.ServeMux` that runs before a handler (`X-HTTP-Method-Override`
//! and the `POST` → `GET` path-length fallback).
//!
//! The order is the generated handler's, and it is observable:
//!
//! 1. the body (`body: "*"` or `body: "<field>"`), with `PATCH` field-mask
//!    computation when the rule allows it;
//! 2. path variables, in template order — they overwrite what the body set;
//! 3. `Request.ParseForm` and the query parameters, skipping the path fields
//!    and the body field, only when the request message has a top-level field
//!    that is neither.
//!
//! Proven against the generated Go code by `tests/request.rs` over
//! `conformance/vectors/request-*.json`; see `docs/DESIGN.md`.

mod binding;
mod field_mask;
mod fields;
mod form;
mod gotime;
mod isprint;
mod json;
mod mux;
mod strconv;

use std::fmt;

pub use binding::{BindingError, BindingOptions, BodySelector, HttpRequest, RequestBinding};
pub use form::Form;
pub use mux::{Dispatch, DispatchOutcome, Incoming, MuxOptions, dispatch, raw_query};

/// gRPC `INVALID_ARGUMENT`.
const CODE_INVALID_ARGUMENT: i32 = 3;
/// gRPC `INTERNAL`.
const CODE_INTERNAL: i32 = 13;

/// Who wrote the text of a [`RequestError`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorOrigin {
    /// grpc-gateway, or the Go standard library under it (`strconv`, `time`,
    /// `net/url`, `mime`); abada reproduces the text, and the conformance
    /// suite compares it.
    Gateway,
    /// The JSON codec ([`crate::json::Marshaler`]): protojson, or
    /// `encoding/json` decoding a non-message body field into a Go type (whose
    /// messages name Go types). The code is grpc-gateway's; the text is not
    /// claimed to be.
    Decoder,
}

/// Why a request could not become a request message. Every one of these is
/// answered by grpc-gateway's error handler; [`RequestError::status`] is what
/// it is handed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestError {
    code: i32,
    message: String,
    origin: ErrorOrigin,
}

impl RequestError {
    fn invalid(message: impl Into<String>) -> Self {
        Self {
            code: CODE_INVALID_ARGUMENT,
            message: message.into(),
            origin: ErrorOrigin::Gateway,
        }
    }

    fn decoder(message: impl Into<String>) -> Self {
        Self {
            code: CODE_INVALID_ARGUMENT,
            message: message.into(),
            origin: ErrorOrigin::Decoder,
        }
    }

    /// A body or `Struct`/`Value` query value the codec refused: the
    /// generated handler answers `codes.InvalidArgument` with the error text.
    fn codec(error: crate::json::JsonError) -> Self {
        Self::decoder(error.to_string())
    }

    fn with_origin(mut self, origin: ErrorOrigin) -> Self {
        self.origin = origin;
        self
    }

    /// A string field holding bytes that are not UTF-8. grpc-gateway puts them
    /// in the Go message, and the gRPC client refuses to marshal it; abada's
    /// `String`s cannot hold them, so it refuses at the same point.
    fn invalid_utf8() -> Self {
        Self {
            code: CODE_INTERNAL,
            message: "grpc: error while marshaling: string field contains invalid UTF-8".into(),
            origin: ErrorOrigin::Gateway,
        }
    }

    pub fn code(&self) -> i32 {
        self.code
    }

    pub fn message(&self) -> &str {
        &self.message
    }

    pub fn origin(&self) -> ErrorOrigin {
        self.origin
    }

    /// The status grpc-gateway's `HTTPError` receives.
    pub fn status(&self) -> crate::error::Status {
        crate::error::Status::new(self.code, self.message.clone())
    }
}

impl fmt::Display for RequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "code {}: {}", self.code, self.message)
    }
}

impl std::error::Error for RequestError {}

#[doc(hidden)]
pub fn strconv_not_printable() -> &'static [(u32, u32)] {
    isprint::NOT_PRINTABLE
}
