//! Calling the RPC: [`unary`] takes what `RequestBinding::decode` and
//! [`metadata::incoming`](super::metadata::incoming) already produced, calls
//! it through the one path [ADR 0002] decided (`tonic::client::Grpc<S>`,
//! generic over `S: GrpcService` — the same call whether `S` is a generated
//! `<Service>Server<T>`, in-process, or a real `tonic::transport::Channel`),
//! and turns the result into a [`response::Response`] or an
//! [`ErrorResponse`].
//!
//! **Scope of this first version**: the success path is complete; a failing
//! call becomes only a code and a message (`tonic::Status`'s `details` are
//! not decoded into `error::Any`s yet), and there is no support for
//! response trailers on success (ADR 0002 found no direct tonic hook for a
//! handler to set them) — see `docs/DESIGN.md`'s "Call" section for the
//! full list.
//!
//! [ADR 0002]: ../../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md

use prost_reflect::DynamicMessage;
use tonic::metadata::{Ascii, Binary, KeyAndValueRef, MetadataKey, MetadataValue};

use super::codec::DynamicCodec;
use super::metadata::Incoming;
use super::response;
use crate::error::{ErrorResponse, ServerMetadata, Status};
use crate::json::Marshaler;

/// Calls one unary RPC and returns the response to write, either way.
///
/// `codec` must be built from the request and response message
/// descriptors, in that order (`DynamicCodec::new(request_desc,
/// response_desc)`) — the same rule `RequestBinding::decode` already
/// follows for which descriptor decodes what.
pub async fn unary<S>(
    grpc: &mut tonic::client::Grpc<S>,
    rpc_path: &http::uri::PathAndQuery,
    codec: DynamicCodec,
    message: DynamicMessage,
    metadata: &Incoming,
    marshaler: &Marshaler,
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

    const CODE_UNKNOWN: i32 = 2;
    const CODE_INTERNAL: i32 = 13;

    if let Err(e) = grpc.ready().await {
        let status = Status::new(CODE_UNKNOWN, e.into().to_string());
        return Err(Box::new(ErrorResponse::from_status(
            &status,
            None,
            accepts_trailers,
        )));
    }

    match grpc.unary(request, rpc_path.clone(), codec).await {
        Ok(response) => {
            let server_metadata = incoming_response_metadata(response.metadata());
            let message = response.into_inner();
            response::Response::success(
                marshaler,
                &message,
                Some(&server_metadata),
                accepts_trailers,
            )
            .map_err(|e| {
                let status = Status::new(CODE_INTERNAL, format!("encode: {e}"));
                Box::new(ErrorResponse::from_status(
                    &status,
                    Some(&server_metadata),
                    accepts_trailers,
                ))
            })
        }
        Err(status) => {
            let server_metadata = incoming_response_metadata(status.metadata());
            let abada_status = Status::new(status.code() as i32, status.message().to_string());
            Err(Box::new(ErrorResponse::from_status(
                &abada_status,
                Some(&server_metadata),
                accepts_trailers,
            )))
        }
    }
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

/// A response's metadata as [`ServerMetadata`] holds it. There are no
/// trailers here: this reads the metadata attached to the `Response`/
/// `Status` tonic already gave back, and tonic exposes only headers this
/// way — see `docs/DESIGN.md`'s "Call" section.
fn incoming_response_metadata(md: &tonic::metadata::MetadataMap) -> ServerMetadata {
    let mut headers = Vec::new();
    for kv in md.iter() {
        match kv {
            KeyAndValueRef::Ascii(k, v) => headers.push((k.to_string(), v.as_bytes().to_vec())),
            KeyAndValueRef::Binary(k, v) => {
                if let Ok(bytes) = v.to_bytes() {
                    headers.push((k.to_string(), bytes.to_vec()));
                }
            }
        }
    }
    ServerMetadata {
        headers,
        trailers: Vec::new(),
    }
}
