//! The `200 OK` a successful unary call becomes: `ForwardResponseMessage`
//! (`runtime/handler.go`) with the default marshaler, and the
//! `response_body` rule of the binding — the wrapper type the generator
//! emits around the response message (`response_<Svc>_<Method>_<i>` with its
//! `XXX_ResponseBody`, `protoc-gen-grpc-gateway/internal/gengateway/
//! template.go`).
//!
//! Header and trailer writing is the same code [`crate::error::ErrorResponse`]
//! uses (`crate::error::write_metadata`) — grpc-gateway shares
//! `handleForwardResponseServerMetadata`/`handleForwardResponseTrailerHeader`
//! between the two, and so does abada.
//!
//! Two things here are not obvious and are measured, not assumed
//! (`conformance/vectors/e2e-abada-conformance-response-v1.json`):
//!
//! - **`Content-Type` is decided by what is handed to `ForwardResponseMessage`,
//!   not by what is written.** The default marshaler is
//!   `runtime.HTTPBodyMarshaler{&runtime.JSONPb{…}}`, whose `ContentType` is
//!   the message's own when the message IS a `google.api.HttpBody`. With a
//!   `response_body`, what it sees is the generator's wrapper struct, never
//!   an `HttpBody` — so the answer is `application/json` even when the
//!   selected field is an `HttpBody`, whose raw bytes `Marshal` then writes.
//! - **A nested `response_body` over an unset intermediate message kills the
//!   Go handler.** `FieldPath.AssignableExpr` emits plain field access
//!   (`response.Inner.Text`) for proto3, so a nil `Inner` is a nil
//!   dereference; `net/http` recovers it and closes the connection with no
//!   reply at all. abada answers as if the intermediate were the empty
//!   message — a written deviation, see `docs/DESIGN.md`.

use http::{HeaderMap, HeaderValue, StatusCode, header};
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, ReflectMessage, Value};

use crate::error::{self, ServerMetadata};
use crate::json::{JsonError, Marshaler};

/// `google.api.HttpBody`, the one message the default outbound marshaler
/// treats specially (`runtime/marshal_httpbodyproto.go`).
const HTTP_BODY: &str = "google.api.HttpBody";

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
    /// `ForwardResponseMessage` for one message.
    ///
    /// `response_body` is the field path the binding selected
    /// ([`RequestBinding::response_body`](crate::request::RequestBinding::response_body)),
    /// outermost first; empty means the whole message, which is what both an
    /// absent `response_body` and `response_body: "*"` produce
    /// (`Registry.newResponse` returns `nil` for both).
    pub fn success(
        marshaler: &Marshaler,
        message: &DynamicMessage,
        response_body: &[FieldDescriptor],
        metadata: Option<&ServerMetadata>,
        accepts_trailers: bool,
    ) -> Result<Self, JsonError> {
        let (body, content_type) = render(marshaler, message, response_body)?;
        let mut headers = HeaderMap::new();
        headers.insert(header::CONTENT_TYPE, content_type);
        let trailers = error::write_metadata(&mut headers, metadata, accepts_trailers);
        Ok(Self {
            status: StatusCode::OK,
            headers,
            body,
            trailers,
        })
    }
}

/// The body bytes and the `Content-Type` for them, in the order
/// `ForwardResponseMessage` decides them: the content type comes from what
/// the handler passed in (the message, or the generator's wrapper), the body
/// from what `XXX_ResponseBody` returns.
fn render(
    marshaler: &Marshaler,
    message: &DynamicMessage,
    response_body: &[FieldDescriptor],
) -> Result<(Vec<u8>, HeaderValue), JsonError> {
    let Some((last, parents)) = response_body.split_last() else {
        // No selection: the message itself is what ContentType and Marshal
        // both see.
        if message.descriptor().full_name() == HTTP_BODY {
            return Ok((http_body_data(message), http_body_content_type(message)));
        }
        return Ok((marshaler.encode(message)?, json_content_type()));
    };

    // `AssignableExpr` walks the intermediate components with plain field
    // access. abada reads the field value instead of dereferencing, so an
    // unset message reads as the empty one (the deviation above).
    let mut owner = message.clone();
    for field in parents {
        let next = match owner.get_field(field).as_ref() {
            Value::Message(m) => m.clone(),
            // A non-message intermediate cannot happen: `resolveFieldPath`
            // refuses a path through anything else ("not an aggregate type"),
            // and `RequestBinding::new` resolves the path the same way.
            _ => {
                return Err(JsonError::unsupported(
                    "response_body: a component of the path is not a message",
                ));
            }
        };
        owner = next;
    }

    // `HTTPBodyMarshaler.Marshal` is handed the FIELD, so an HttpBody field
    // writes its raw data — while ContentType saw only the wrapper.
    if !last.is_list() && !last.is_map() {
        if let Kind::Message(md) = last.kind() {
            if md.full_name() == HTTP_BODY {
                let value = owner.get_field(last);
                let body = match value.as_ref() {
                    Value::Message(m) => http_body_data(m),
                    _ => Vec::new(),
                };
                return Ok((body, json_content_type()));
            }
        }
    }
    Ok((marshaler.encode_field(&owner, last)?, json_content_type()))
}

fn json_content_type() -> HeaderValue {
    HeaderValue::from_static("application/json")
}

/// `HttpBody.GetData()`. A field that is not there reads as the Go zero
/// value, which is what `GetData()` on a nil pointer returns.
fn http_body_data(message: &DynamicMessage) -> Vec<u8> {
    message
        .descriptor()
        .get_field_by_name("data")
        .map(|f| match message.get_field(&f).as_ref() {
            Value::Bytes(b) => b.to_vec(),
            _ => Vec::new(),
        })
        .unwrap_or_default()
}

/// `HttpBody.GetContentType()`, as `net/http` writes it — including the empty
/// string, which Go still writes as a present, empty `Content-Type` header
/// (the key is in the map, so `net/http` does not sniff the body).
fn http_body_content_type(message: &DynamicMessage) -> HeaderValue {
    let raw = message
        .descriptor()
        .get_field_by_name("content_type")
        .map(|f| match message.get_field(&f).as_ref() {
            Value::String(s) => s.clone(),
            _ => String::new(),
        })
        .unwrap_or_default();
    HeaderValue::from_str(&raw).unwrap_or_else(|_| HeaderValue::from_static(""))
}
