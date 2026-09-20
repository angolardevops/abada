//! The abada side of the performance gate (`.claude/skills/abada-performance`).
//!
//! `abada-parity-gateway --listen 127.0.0.1:8081 --backend 127.0.0.1:9000 \
//!     --set conformance/contracts/abada-conformance-request-v1.binpb`
//!
//! Mounts `abada::service::Gateway` in hyper (HTTP/1.1, keep-alive, default
//! options) and calls the backend through one `tonic::transport::Channel` —
//! proxy mode, the same shape as grpc-gateway's `grpc.ClientConn`. Routes are
//! every non-streaming binding of the contract, registered in declaration
//! order, like the generated Go code.

use std::net::SocketAddr;

use abada::json::{Marshaler, TypeRegistry};
use abada::path::{Pattern, Router, UnescapingMode};
use abada::request::{BindingOptions, RequestBinding};
use abada::service::{Gateway, Registration};
use hyper::server::conn::http1;
use hyper_util::rt::TokioIo;
use hyper_util::service::TowerToHyperService;
use prost_reflect::DescriptorPool;

fn arg(name: &str) -> String {
    let mut it = std::env::args();
    while let Some(a) = it.next() {
        if a == name {
            return it
                .next()
                .unwrap_or_else(|| die(&format!("{name} needs a value")));
        }
    }
    die(&format!("missing {name}"))
}

fn die(msg: &str) -> ! {
    eprintln!("abada-parity-gateway: {msg}");
    std::process::exit(2)
}

#[tokio::main]
async fn main() {
    let listen: SocketAddr = arg("--listen")
        .parse()
        .unwrap_or_else(|e| die(&format!("{e}")));
    let backend = arg("--backend");
    let raw = std::fs::read(arg("--set")).unwrap_or_else(|e| die(&format!("{e}")));

    let pool = DescriptorPool::decode(raw.as_slice()).unwrap_or_else(|e| die(&format!("{e}")));
    let mut router = Router::new(UnescapingMode::Legacy);
    for b in abada_codegen::bindings(&raw).unwrap_or_else(|e| die(&format!("{e}"))) {
        if b.client_streaming || b.server_streaming {
            continue;
        }
        let method = pool
            .get_service_by_name(&b.service)
            .and_then(|s| s.methods().find(|m| m.name() == b.method))
            .unwrap_or_else(|| die(&format!("no method {}.{}", b.service, b.method)));
        let pattern = Pattern::new(&b.template).unwrap_or_else(|e| die(&format!("{e:?}")));
        let binding = RequestBinding::new(
            method,
            &b.http_method,
            pattern.template(),
            &b.body,
            &b.response_body,
            &BindingOptions::default(),
        )
        .unwrap_or_else(|e| die(&format!("{e:?}")));
        router.add(&b.http_method, pattern, Registration::new(binding));
    }

    let channel = tonic::transport::Endpoint::from_shared(format!("http://{backend}"))
        .unwrap_or_else(|e| die(&format!("{e}")))
        .connect_lazy();
    let marshaler =
        Marshaler::new(TypeRegistry::new(pool).unwrap_or_else(|e| die(&format!("{e}"))));
    let gateway = Gateway::new(router, channel, marshaler);

    let listener = tokio::net::TcpListener::bind(listen)
        .await
        .unwrap_or_else(|e| die(&format!("{e}")));
    eprintln!("abada listening on {listen}");
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            continue;
        };
        let service = TowerToHyperService::new(gateway.clone());
        tokio::spawn(async move {
            let _ = http1::Builder::new()
                .serve_connection(TokioIo::new(stream), service)
                .await;
        });
    }
}
