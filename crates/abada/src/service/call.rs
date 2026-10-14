//! Calling the RPC: [`unary`] takes what `RequestBinding::decode` and
//! [`metadata::incoming`](super::metadata::incoming) already produced, calls
//! it through the one path [ADR 0002] decided (`tonic::client::Grpc<S>`,
//! generic over `S: GrpcService` — the same call whether `S` is a generated
//! `<Service>Server<T>`, in-process, or a real `tonic::transport::Channel`),
//! and turns the result into a [`response::Response`] or an
//! [`ErrorResponse`].
//!
//! The call goes through `Grpc::server_streaming`, not `Grpc::unary`, and
//! the reason is [ADR 0003]: `unary` merges the response trailers into the
//! response metadata, and grpc-gateway writes the two under different
//! prefixes (`Grpc-Metadata-*` vs `Grpc-Trailer-*`). `server_streaming`
//! hands back the headers and a `Streaming` whose `trailers()` is separate —
//! the same split grpc-gateway's generated proxy handler gets from
//! `grpc.Header`/`grpc.Trailer`. On the wire a unary RPC and a
//! single-message server-streaming RPC are the same exchange.
//!
//! [ADR 0002]: ../../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md
//! [ADR 0003]: ../../../../docs/adr/0003-cabecalhos-e-trailers-de-uma-chamada.md

use prost_reflect::{DynamicMessage, FieldDescriptor, Value};
use tonic::metadata::{Ascii, Binary, KeyAndValueRef, MetadataKey, MetadataMap, MetadataValue};

use super::codec::DynamicCodec;
use super::metadata::Incoming;
use super::response;
use crate::error::{Any, ErrorResponse, ServerMetadata, Status};
use crate::json::Marshaler;

const CODE_UNKNOWN: i32 = 2;
const CODE_INTERNAL: i32 = 13;

/// Calls one unary RPC and returns the response to write, either way.
///
/// `codec` must be built from the request and response message
/// descriptors, in that order (`DynamicCodec::new(request_desc,
/// response_desc)`) — the same rule `RequestBinding::decode` already
/// follows for which descriptor decodes what. `response_body` is the field
/// path the binding selected, which [`response::Response::success`] applies
/// to the answer.
pub async fn unary<S>(
    grpc: &mut tonic::client::Grpc<S>,
    rpc_path: &http::uri::PathAndQuery,
    codec: DynamicCodec,
    message: DynamicMessage,
    metadata: &Incoming,
    marshaler: &Marshaler,
    response_body: &[FieldDescriptor],
    accepts_trailers: bool,
) -> Result<response::Response, Box<ErrorResponse>>
where
    S: tonic::client::GrpcService<tonic::body::Body>,
    S::ResponseBody: tonic::codegen::Body<Data = bytes::Bytes> + Send + 'static,
    <S::ResponseBody as tonic::codegen::Body>::Error: Into<tonic::codegen::StdError> + Send,
{
    let mut request = tonic::Request::new(message);
    set_outgoing_metadata(&mut request, metadata);
    if let Some(timeout) = metadata.timeout {
        request.set_timeout(timeout);
    }

    let fail = |status: Status, md: ServerMetadata| {
        Err(Box::new(ErrorResponse::from_status_with(
            marshaler,
            &status,
            Some(&md),
            accepts_trailers,
        )))
    };

    if let Err(e) = grpc.ready().await {
        let status = Status::new(CODE_UNKNOWN, e.into().to_string());
        return Err(Box::new(ErrorResponse::from_status_with(
            marshaler,
            &status,
            None,
            accepts_trailers,
        )));
    }

    let response = match grpc
        .server_streaming(request, rpc_path.clone(), codec)
        .await
    {
        Ok(response) => response,
        // A trailers-only answer: grpc-go puts those fields in the TRAILER
        // metadata (`operateHeaders` with `endStream` set), never in the
        // header metadata, so they are trailers here too.
        Err(status) => {
            let md = ServerMetadata {
                headers: Vec::new(),
                trailers: forwarded(status.metadata()),
            };
            return fail(abada_status(marshaler, &status), md);
        }
    };

    let (headers, mut stream, _extensions) = response.into_parts();
    let headers = forwarded(&headers);

    let message = match stream.message().await {
        Ok(Some(message)) => message,
        Ok(None) => {
            let trailers = match stream.trailers().await {
                Ok(md) => md.as_ref().map(forwarded).unwrap_or_default(),
                Err(status) => forwarded(status.metadata()),
            };
            let md = ServerMetadata { headers, trailers };
            return fail(Status::new(CODE_INTERNAL, "Missing response message."), md);
        }
        Err(status) => {
            let md = ServerMetadata {
                headers,
                trailers: forwarded(status.metadata()),
            };
            return fail(abada_status(marshaler, &status), md);
        }
    };

    // The trailers come after the message. A failing status here is the call
    // failing after it already sent one — grpc-go reports the error and the
    // generated handler still forwards both metadata sets, because it
    // captured them before looking at the error.
    let (trailers, late) = match stream.trailers().await {
        Ok(md) => (md.as_ref().map(forwarded).unwrap_or_default(), None),
        Err(status) => (forwarded(status.metadata()), Some(status)),
    };
    let server_metadata = ServerMetadata { headers, trailers };
    if let Some(status) = late {
        return fail(abada_status(marshaler, &status), server_metadata);
    }

    response::Response::success(
        marshaler,
        &message,
        response_body,
        Some(&server_metadata),
        accepts_trailers,
    )
    .map_err(|e| {
        let status = Status::new(CODE_INTERNAL, format!("encode: {e}"));
        Box::new(ErrorResponse::from_status_with(
            marshaler,
            &status,
            Some(&server_metadata),
            accepts_trailers,
        ))
    })
}

/// `tonic::Status` as [`Status`], applying grpc-go's own rule for
/// `grpc-status-details-bin` (`internal/status.NewWithProto`, read at
/// grpc-go v1.75.1, the version grpc-gateway v2.27.3 resolves):
///
/// - no details bytes → the code and message of the headers, no details;
/// - bytes that do not decode as `google.rpc.Status` → the same;
/// - bytes that decode and whose code matches → **the decoded proto whole**,
///   so the message may differ from `grpc-message` and the details come from
///   it;
/// - bytes that decode and whose code does not match → `INTERNAL` with a
///   "grpc-status-details-bin mismatch" message. abada does not reproduce
///   Go's `%+v` rendering of the offending proto (it is not stable across Go
///   builds, `internal/detrand`); it writes the fixed prefix and the two
///   codes. Written deviation, `docs/DESIGN.md`.
fn abada_status(marshaler: &Marshaler, status: &tonic::Status) -> Status {
    let code = status.code() as i32;
    let plain = || Status::new(code, status.message().to_string());
    let bytes = status.details();
    if bytes.is_empty() {
        return plain();
    }
    let Some(decoded) = decode_status_proto(marshaler, bytes) else {
        return plain();
    };
    if decoded.code != code {
        return Status::new(
            CODE_INTERNAL,
            format!(
                "grpc-status-details-bin mismatch: grpc-status={code}, \
                 grpc-status-details-bin code={}",
                decoded.code
            ),
        );
    }
    decoded
}

/// `google.rpc.Status` from the `grpc-status-details-bin` bytes, through the
/// marshaler's own pool — the same registry that later decides whether a
/// detail's type URL can be rendered, as the linked Go types do in Go.
fn decode_status_proto(marshaler: &Marshaler, bytes: &[u8]) -> Option<Status> {
    let desc = marshaler
        .registry()
        .pool()
        .get_message_by_name("google.rpc.Status")?;
    let msg = DynamicMessage::decode(desc, bytes).ok()?;
    let code = msg.get_field_by_number(1).and_then(|v| v.as_i32())?;
    let message = msg
        .get_field_by_number(2)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();
    let mut details = Vec::new();
    if let Some(list) = msg
        .get_field_by_number(3)
        .and_then(|v| v.as_list().map(<[Value]>::to_vec))
    {
        for item in list {
            let Value::Message(any) = item else { continue };
            let type_url = any
                .get_field_by_number(1)
                .and_then(|v| v.as_str().map(str::to_string))
                .unwrap_or_default();
            let value = any
                .get_field_by_number(2)
                .and_then(|v| v.as_bytes().map(|b| b.to_vec()))
                .unwrap_or_default();
            details.push(Any { type_url, value });
        }
    }
    Some(Status {
        code,
        message,
        details,
    })
}

/// Copies `metadata`'s pairs into the outgoing request — `metadata::incoming`
/// already validated every key and value against grpc-gateway's own rules,
/// so a pair that still fails to become a `tonic` metadata entry is dropped
/// rather than treated as a bug in the request.
fn set_outgoing_metadata<T>(request: &mut tonic::Request<T>, metadata: &Incoming) {
    let md = request.metadata_mut();
    for (key, value) in &metadata.pairs {
        if key.ends_with("-bin") {
            if let Ok(k) = MetadataKey::<Binary>::from_bytes(key.as_bytes()) {
                md.append_bin(k, MetadataValue::<Binary>::from_bytes(value));
            }
        } else if let (Ok(k), Ok(v)) = (
            MetadataKey::<Ascii>::from_bytes(key.as_bytes()),
            MetadataValue::<Ascii>::try_from(value.as_slice()),
        ) {
            md.append(k, v);
        }
    }
}

/// The pairs a gRPC client hands to the application, as grpc-go decides them
/// (the header switch of `internal/transport/http2_client.go` and
/// `isReservedHeader`/`isWhitelistedHeader` in `http_util.go`): a reserved
/// name is dropped unless it is whitelisted, and `content-type` is kept by a
/// branch of its own — which is why every grpc-gateway response carries
/// `Grpc-Metadata-Content-Type: application/grpc`.
///
/// `grpc-status`, `grpc-message` and `grpc-status-details-bin` never reach
/// here: tonic removes them before building the map. grpc-go keeps the last
/// of those in the metadata, so grpc-gateway also writes it as a trailer —
/// see `docs/DESIGN.md`'s written deviation.
fn forwarded(md: &MetadataMap) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    for kv in md.iter() {
        let (key, value) = match kv {
            KeyAndValueRef::Ascii(k, v) => (k.as_str().to_string(), v.as_bytes().to_vec()),
            KeyAndValueRef::Binary(k, v) => match v.to_bytes() {
                Ok(bytes) => (k.as_str().to_string(), bytes.to_vec()),
                Err(_) => continue,
            },
        };
        if is_reserved(&key) {
            continue;
        }
        out.push((key, value));
    }
    out
}

/// `isReservedHeader` minus `isWhitelistedHeader` (`user-agent` and
/// `:authority` are whitelisted; `content-type` never reaches the reserved
/// test because the switch handles it first and keeps it).
fn is_reserved(key: &str) -> bool {
    if key.starts_with(':') {
        return true;
    }
    matches!(
        key,
        "grpc-message-type"
            | "grpc-encoding"
            | "grpc-message"
            | "grpc-status"
            | "grpc-timeout"
            | "te"
    )
}
