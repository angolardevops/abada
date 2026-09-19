//! Proves `service::call::unary`'s own wiring — not grpc-gateway's behaviour,
//! which has no equivalent to compare against here, since this is abada's
//! own assembly of pieces each already proven on their own: `metadata::
//! incoming` (`tests/metadata.rs`), `response::Response::success`
//! (`tests/response.rs`), and the in-process call path
//! ([ADR 0002](../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md)'s
//! prototype). What has no prior proof is whether `call::unary` threads
//! metadata into a real `tonic::Request`, reads a real `tonic::Response`'s
//! metadata back out, and turns a real `tonic::Status` into an
//! `ErrorResponse` — so this is an integration test against a real,
//! hand-written tonic service (no `tonic-build`: that needs Rust 1.88,
//! above abada's own 1.85, so nothing in this crate's own build or tests
//! may depend on it either — see ADR 0002's "Consequences").

use std::convert::Infallible;

use abada::json::Marshaler;
use abada::service::call;
use abada::service::codec::DynamicCodec;
use abada::service::metadata::Incoming;
use prost_reflect::{DynamicMessage, MessageDescriptor, ReflectMessage, Value};
use tower::service_fn;

fn descriptors() -> (MessageDescriptor, MessageDescriptor) {
    let pool = Marshaler::default().registry().pool().clone();
    let status = pool
        .get_message_by_name("google.rpc.Status")
        .expect("google.rpc.Status is always in the registry");
    (status.clone(), status)
}

/// The fake backend: echoes the request message's `message` field back with
/// a prefix, copies one request metadata value into the response, and
/// fails with `NOT_FOUND` when the request code asks for it — enough to
/// exercise both the success and the error arm of `call::unary`.
const TRIGGER_NOT_FOUND: i32 = 5;

async fn fake_rpc(
    request: tonic::Request<DynamicMessage>,
) -> Result<tonic::Response<DynamicMessage>, tonic::Status> {
    let seen = request
        .metadata()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let msg = request.into_inner();
    let code = msg
        .get_field_by_number(1)
        .and_then(|v| v.as_i32())
        .unwrap_or(0);
    if code == TRIGGER_NOT_FOUND {
        return Err(tonic::Status::not_found("no such thing"));
    }
    let text = msg
        .get_field_by_number(2)
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default();

    let mut response = DynamicMessage::new(msg.descriptor());
    response.set_field_by_number(1, Value::I32(0));
    response.set_field_by_number(2, Value::String(format!("echo:{text}:{seen}")));
    let mut response = tonic::Response::new(response);
    response
        .metadata_mut()
        .insert("x-response-id", "seen".parse().unwrap());
    Ok(response)
}

/// A hand-written, codegen-free `GrpcService`: the HTTP/2-shaped outer
/// service `tonic::client::Grpc<S>` calls, built from `tonic::server::Grpc`
/// (the server-side counterpart of the client wrapper ADR 0002 already
/// uses) driven by the same [`DynamicCodec`] the client encodes/decodes
/// with — the two ends of one call, both descriptor-driven, no generated
/// Rust type for either.
fn fake_server(
    request_desc: MessageDescriptor,
    response_desc: MessageDescriptor,
) -> impl tonic::client::GrpcService<
    tonic::body::Body,
    ResponseBody = tonic::body::Body,
    Error = Infallible,
> + Clone
+ use<> {
    service_fn(move |req: http::Request<tonic::body::Body>| {
        let codec = DynamicCodec::new(request_desc.clone(), response_desc.clone());
        async move {
            let mut server = tonic::server::Grpc::new(codec);
            let response = server.unary(service_fn(fake_rpc), req).await;
            Ok::<_, Infallible>(response)
        }
    })
}

fn status_message(code: i32, message: &str) -> DynamicMessage {
    let (request_desc, _) = descriptors();
    let mut msg = DynamicMessage::new(request_desc);
    msg.set_field_by_number(1, Value::I32(code));
    msg.set_field_by_number(2, Value::String(message.to_string()));
    msg
}

fn rpc_path() -> http::uri::PathAndQuery {
    "/abada.service.call.v1.Probe/Call".parse().unwrap()
}

#[tokio::test]
async fn a_successful_call_carries_metadata_both_ways() {
    let (request_desc, response_desc) = descriptors();
    let codec = DynamicCodec::new(request_desc.clone(), response_desc.clone());
    let mut grpc = tonic::client::Grpc::new(fake_server(request_desc, response_desc));
    grpc.ready().await.expect("fake server is always ready");

    let mut metadata = Incoming::default();
    metadata
        .pairs
        .push(("x-request-id".to_string(), b"req-42".to_vec()));

    let got = call::unary(
        &mut grpc,
        &rpc_path(),
        codec,
        status_message(0, "hello"),
        &metadata,
        &Marshaler::default(),
        false,
    )
    .await
    .expect("the fake server succeeds for code 0");

    assert_eq!(got.status, http::StatusCode::OK);
    let body: serde_json::Value = serde_json::from_slice(&got.body).expect("valid JSON");
    assert_eq!(body["message"], "echo:hello:req-42");
    assert_eq!(
        got.headers.get("grpc-metadata-x-response-id").unwrap(),
        "seen"
    );
}

#[tokio::test]
async fn a_failing_call_becomes_an_error_response() {
    let (request_desc, response_desc) = descriptors();
    let codec = DynamicCodec::new(request_desc.clone(), response_desc.clone());
    let mut grpc = tonic::client::Grpc::new(fake_server(request_desc, response_desc));
    grpc.ready().await.expect("fake server is always ready");

    let err = call::unary(
        &mut grpc,
        &rpc_path(),
        codec,
        status_message(TRIGGER_NOT_FOUND, "irrelevant"),
        &Incoming::default(),
        &Marshaler::default(),
        false,
    )
    .await
    .expect_err("the fake server refuses code 5");

    // code 5 (NOT_FOUND) maps to HTTP 404 — abada::error::http_status_from_code,
    // proven against grpc-gateway on its own in tests/errors.rs.
    assert_eq!(err.status, http::StatusCode::NOT_FOUND);
    let body: serde_json::Value = serde_json::from_slice(&err.body).expect("valid JSON");
    assert_eq!(body["message"], "no such thing");
}
