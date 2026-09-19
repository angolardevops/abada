//! Proves `service::Gateway` end to end as a `tower::Service`: a real HTTP
//! request goes in, routing and request population are the same
//! `PathService.Top` binding `tests/request.rs` already proves against
//! grpc-gateway (no `response_body` on this one, so the whole-message
//! response `service::call`/`service::response` write is the right
//! behaviour to expect), the call reaches a real, hand-written, codegen-free
//! tonic service (as `tests/call.rs`'s), and a real `http::Response` comes
//! back. Like `tests/call.rs`, this has no grpc-gateway equivalent to
//! compare against — it is abada's own assembly — so it is an integration
//! test, not a conformance suite.

use std::convert::Infallible;
use std::path::PathBuf;

use abada::json::{Marshaler, TypeRegistry};
use abada::path::{Pattern, Router, UnescapingMode};
use abada::request::{BindingOptions, RequestBinding};
use abada::service::{Gateway, Registration};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use prost_reflect::{DescriptorPool, DynamicMessage, ReflectMessage};
use tower::ServiceExt;

fn contract_pool() -> DescriptorPool {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/contracts/abada-conformance-request-v1.binpb");
    let raw = std::fs::read(path).expect("read the conformance descriptor set");
    DescriptorPool::decode(raw.as_slice()).expect("valid descriptor set")
}

async fn fake_rpc(
    request: tonic::Request<DynamicMessage>,
) -> Result<tonic::Response<DynamicMessage>, tonic::Status> {
    // Proves the request metadata `Gateway` built from the incoming HTTP
    // headers actually reached the server: echoed back, not just present.
    let seen_request_id = request
        .metadata()
        .get("x-request-id")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let pool = request.get_ref().descriptor().parent_pool().clone();
    let empty = pool
        .get_message_by_name("google.protobuf.Empty")
        .expect("google.protobuf.Empty is in this descriptor set");
    let mut response = tonic::Response::new(DynamicMessage::new(empty));
    response
        .metadata_mut()
        .insert("x-gateway-test", "seen".parse().unwrap());
    response
        .metadata_mut()
        .insert("x-echo-request-id", seen_request_id.parse().unwrap());
    Ok(response)
}

fn fake_server(
    request_desc: prost_reflect::MessageDescriptor,
    response_desc: prost_reflect::MessageDescriptor,
) -> impl tonic::client::GrpcService<
    tonic::body::Body,
    ResponseBody = tonic::body::Body,
    Error = Infallible,
    Future: Send,
> + Clone {
    tower::service_fn(move |req: http::Request<tonic::body::Body>| {
        // The server encodes the response and decodes the request — the
        // opposite order from the client's codec in `service::call::unary`.
        let codec =
            abada::service::codec::DynamicCodec::new(response_desc.clone(), request_desc.clone());
        let fut: std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<http::Response<tonic::body::Body>, Infallible>,
                    > + Send,
            >,
        > = Box::pin(async move {
            let mut server = tonic::server::Grpc::new(codec);
            let response = server.unary(tower::service_fn(fake_rpc), req).await;
            Ok::<_, Infallible>(response)
        });
        fut
    })
}

#[tokio::test]
async fn a_get_request_is_routed_called_and_answered() {
    let pool = contract_pool();
    let service = pool
        .get_service_by_name("abada.conformance.request.v1.PathService")
        .expect("PathService is in the contract");
    let method = service
        .methods()
        .find(|m| m.name() == "Top")
        .expect("Top is a method of PathService");
    let request_desc = method.input();
    let response_desc = method.output();

    let pattern = Pattern::new("/v1/top/int32/{f_int32}").expect("a valid template");
    let binding = RequestBinding::new(
        method.clone(),
        "GET",
        pattern.template(),
        "",
        "",
        &BindingOptions::default(),
    )
    .expect("a valid binding");
    let mut router = Router::new(UnescapingMode::Legacy);
    router.add("GET", pattern, Registration::new(binding));

    let marshaler = Marshaler::new(TypeRegistry::new(pool.clone()).expect("a valid registry"));
    let gateway = Gateway::new(router, fake_server(request_desc, response_desc), marshaler);

    let request = http::Request::builder()
        .method("GET")
        .uri("/v1/top/int32/42")
        .header("grpc-metadata-x-request-id", "gw-1")
        .body(Full::new(Bytes::new()))
        .expect("a valid request");

    let response = gateway
        .oneshot(request)
        .await
        .expect("never fails outright");

    assert_eq!(response.status(), http::StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get("grpc-metadata-x-gateway-test")
            .unwrap(),
        "seen"
    );
    assert_eq!(
        response
            .headers()
            .get("grpc-metadata-x-echo-request-id")
            .unwrap(),
        "gw-1",
        "the incoming Grpc-Metadata-X-Request-Id header must reach the RPC"
    );
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert_eq!(&*body, b"{}".as_slice());
}

#[tokio::test]
async fn an_unrouted_request_answers_grpc_gateways_own_404() {
    let pool = contract_pool();
    let method = pool
        .get_service_by_name("abada.conformance.request.v1.PathService")
        .unwrap()
        .methods()
        .find(|m| m.name() == "Top")
        .unwrap();
    let request_desc = method.input();
    let response_desc = method.output();
    let pattern = Pattern::new("/v1/top/int32/{f_int32}").unwrap();
    let binding = RequestBinding::new(
        method.clone(),
        "GET",
        pattern.template(),
        "",
        "",
        &BindingOptions::default(),
    )
    .unwrap();
    let mut router = Router::new(UnescapingMode::Legacy);
    router.add("GET", pattern, Registration::new(binding));
    let marshaler = Marshaler::new(TypeRegistry::new(pool).unwrap());
    let gateway = Gateway::new(router, fake_server(request_desc, response_desc), marshaler);

    let request = http::Request::builder()
        .method("GET")
        .uri("/nowhere")
        .body(Full::new(Bytes::new()))
        .unwrap();
    let response = gateway.oneshot(request).await.unwrap();
    assert_eq!(response.status(), http::StatusCode::NOT_FOUND);
}
