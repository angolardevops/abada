//! Does the same call code (`tonic::client::Grpc<S>` + a descriptor-driven
//! `DynamicCodec`) work whether `S` is a generated `<Service>Server<T>` used
//! directly, in-process, or a real `tonic::transport::Channel` over loopback?
//!
//! This is the evidence a future ADR on abada's `service` module needs
//! before choosing this design over one where in-process and proxy are two
//! separate code paths.

use std::collections::HashMap;

use prost_reflect::{DescriptorPool, DynamicMessage, MapKey, MessageDescriptor, Value};
use tonic::transport::{Channel, Server};

use tonic_call_proto::pb::DESCRIPTOR_BYTES;
use tonic_call_proto::pb::echo_server::EchoServer;
use tonic_call_proto::{DynamicCodec, EchoImpl};

fn descriptors() -> (MessageDescriptor, MessageDescriptor) {
    let pool = DescriptorPool::decode(DESCRIPTOR_BYTES).expect("valid descriptor set");
    let req = pool
        .get_message_by_name("abada.protocall.v1.SayRequest")
        .expect("SayRequest is in the descriptor set");
    let resp = pool
        .get_message_by_name("abada.protocall.v1.SayResponse")
        .expect("SayResponse is in the descriptor set");
    (req, resp)
}

fn say_request(desc: &MessageDescriptor, id: &str, count: i32) -> DynamicMessage {
    let mut msg = DynamicMessage::new(desc.clone());
    msg.set_field_by_name("id", Value::String(id.to_string()));
    msg.set_field_by_name("count", Value::I32(count));
    let mut tags = HashMap::new();
    tags.insert(
        MapKey::String("origin".to_string()),
        Value::String("prototype".to_string()),
    );
    msg.set_field_by_name("tags", Value::Map(tags));
    msg
}

fn path() -> http::uri::PathAndQuery {
    "/abada.protocall.v1.Echo/Say".parse().unwrap()
}

async fn call<S>(
    mut grpc: tonic::client::Grpc<S>,
    req: DynamicMessage,
    codec: DynamicCodec,
) -> tonic::Response<DynamicMessage>
where
    S: tonic::client::GrpcService<tonic::body::Body>,
    S::Error: std::fmt::Debug,
    S::ResponseBody: http_body::Body<Data = bytes::Bytes> + Send + 'static,
    <S::ResponseBody as http_body::Body>::Error:
        Into<Box<dyn std::error::Error + Send + Sync>> + Send,
{
    grpc.ready().await.expect("service is ready");
    let mut request = tonic::Request::new(req);
    request
        .metadata_mut()
        .insert("x-prototype-request", "hi".parse().unwrap());
    grpc.unary(request, path(), codec)
        .await
        .expect("unary call succeeds")
}

#[tokio::test]
async fn in_process_call_reaches_the_server_directly() {
    let (req_desc, resp_desc) = descriptors();
    let req = say_request(&req_desc, "in-process", 3);
    let codec = DynamicCodec::new(req_desc, resp_desc);

    let svc = EchoServer::new(EchoImpl);
    let grpc = tonic::client::Grpc::new(svc);
    let response = call(grpc, req, codec).await;

    assert_eq!(
        response.metadata().get("x-prototype-response").unwrap(),
        "seen"
    );
    let body = response.into_inner();
    assert_eq!(
        body.get_field_by_name("echoed").unwrap().as_str().unwrap(),
        "in-process:hi"
    );
    assert_eq!(
        body.get_field_by_name("count").unwrap().as_i32().unwrap(),
        3
    );
}

#[tokio::test]
async fn proxy_call_over_loopback_reaches_the_same_server() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind a loopback port");
    let addr = listener.local_addr().unwrap();
    let incoming = tokio_stream::wrappers::TcpListenerStream::new(listener);

    tokio::spawn(async move {
        Server::builder()
            .add_service(EchoServer::new(EchoImpl))
            .serve_with_incoming(incoming)
            .await
            .expect("server runs until the test drops it");
    });

    let channel = Channel::from_shared(format!("http://{addr}"))
        .unwrap()
        .connect()
        .await
        .expect("connect over loopback");

    let (req_desc, resp_desc) = descriptors();
    let req = say_request(&req_desc, "proxy", 7);
    let codec = DynamicCodec::new(req_desc, resp_desc);

    let grpc = tonic::client::Grpc::new(channel);
    let response = call(grpc, req, codec).await;

    assert_eq!(
        response.metadata().get("x-prototype-response").unwrap(),
        "seen"
    );
    let body = response.into_inner();
    assert_eq!(
        body.get_field_by_name("echoed").unwrap().as_str().unwrap(),
        "proxy:hi"
    );
    assert_eq!(
        body.get_field_by_name("count").unwrap().as_i32().unwrap(),
        7
    );
}
