//! The `200 OK` a successful unary call becomes: `ForwardResponseMessage`
//! (`runtime/handler.go`) with the default marshaler. Header and trailer
//! writing is the same code [`crate::error::ErrorResponse`] uses
//! (`crate::error::write_metadata`) — grpc-gateway shares
//! `handleForwardResponseServerMetadata`/`handleForwardResponseTrailerHeader`
//! between the two, and so does abada.
//!
//! `response_body` (rendering one field of the message instead of the whole
//! thing) is not wired in yet — `RequestBinding::response_body` and
//! `Marshaler::encode_field` exist, but nothing here calls them; see
//! `docs/DESIGN.md`.

use http::{HeaderMap, HeaderValue, StatusCode, header};
use prost_reflect::DynamicMessage;

use crate::error::{self, ServerMetadata};
use crate::json::{JsonError, Marshaler};

/// The response to write for a call that succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// Sent after the body; non-empty only when the request accepts trailers.
    pub trailers: HeaderMap,
}

impl Response {
    /// `ForwardResponseMessage` for a message already fully populated —
    /// `message` is encoded whole, as if no `response_body` rule applied.
    pub fn success(
        marshaler: &Marshaler,
        message: &DynamicMessage,
        metadata: Option<&ServerMetadata>,
        accepts_trailers: bool,
    ) -> Result<Self, JsonError> {
        let body = marshaler.encode(message)?;
        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        let trailers = error::write_metadata(&mut headers, metadata, accepts_trailers);
        Ok(Self {
            status: StatusCode::OK,
            headers,
            body,
            trailers,
        })
    }
}
