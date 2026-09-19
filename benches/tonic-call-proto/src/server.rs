//! A fixed, hand-written server implementation: not what abada's future
//! `service` module would generate, just enough of a real tonic service to
//! call through both paths and check the response and its metadata.

use tonic::{Request, Response, Status};

use crate::pb::echo_server::Echo;
use crate::pb::{SayRequest, SayResponse};

pub struct EchoImpl;

#[tonic::async_trait]
impl Echo for EchoImpl {
    async fn say(&self, request: Request<SayRequest>) -> Result<Response<SayResponse>, Status> {
        let echoed_header = request
            .metadata()
            .get("x-prototype-request")
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string();
        let req = request.into_inner();

        let mut response = Response::new(SayResponse {
            echoed: format!("{}:{}", req.id, echoed_header),
            count: req.count,
        });
        response
            .metadata_mut()
            .insert("x-prototype-response", "seen".parse().unwrap());
        Ok(response)
    }
}
