//! The `tower::Service` itself: [`Gateway`] holds a router of
//! [`Registration`]s and a call target, and turns an incoming HTTP request
//! into routing (`path::Router`, `request::dispatch`), request population
//! (`RequestBinding::decode`), the call ([`call::unary`]), and the HTTP
//! response — the same order `runtime.ServeMux.ServeHTTP` and its generated
//! handler follow, read for `request`/`response`/`metadata` already.
//!
//! One RPC still means one [`Registration`], built by hand: there is no
//! code generation yet (`protoc-gen-abada`/`abada-build`), so nothing
//! builds a `Router<Registration>` for a whole service automatically.
//! `response_body` and streaming RPCs are not handled — see
//! `docs/DESIGN.md`'s "Gateway" section for the rest of what is not.

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};

use bytes::Bytes;
use http_body_util::BodyExt;
use prost_reflect::DynamicMessage;

use super::call;
use super::codec::DynamicCodec;
use super::metadata;
use crate::error::{ErrorResponse, RoutingError, Status, accepts_trailers};
use crate::json::Marshaler;
use crate::path::{self, RequestPath, RouteOutcome};
use crate::request::{self, DispatchOutcome, HttpRequest, Incoming, MuxOptions, RequestBinding};

/// gRPC `INVALID_ARGUMENT`: what a request abada itself refuses (a body
/// `tower`'s incoming `Body` fails to read) becomes.
const CODE_INVALID_ARGUMENT: i32 = 3;

/// One RPC a [`Gateway`] can route to and call: the request binding
/// (`RequestBinding`, itself built from a `MethodDescriptor` and its
/// `google.api.http` rule) plus the full gRPC method path
/// (`/<package>.<Service>/<Method>`) `call::unary` sends the request to.
pub struct Registration {
    binding: RequestBinding,
    rpc_path: http::uri::PathAndQuery,
}

impl Registration {
    pub fn new(binding: RequestBinding) -> Self {
        let method = binding.method();
        let rpc_path = format!("/{}/{}", method.parent_service().full_name(), method.name())
            .parse()
            .expect("a proto service and method name make a valid path");
        Self { binding, rpc_path }
    }
}

/// A `tower::Service<http::Request<ReqBody>>` that routes to one or more
/// [`Registration`]s and calls every one of them through the same `S`
/// ([ADR 0002](../../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md):
/// in-process and proxy are the same call, this is the type that makes
/// that concrete).
pub struct Gateway<S> {
    router: Arc<path::Router<Registration>>,
    inner: S,
    marshaler: Arc<Marshaler>,
    options: MuxOptions,
}

impl<S> Gateway<S> {
    pub fn new(router: path::Router<Registration>, inner: S, marshaler: Marshaler) -> Self {
        Self {
            router: Arc::new(router),
            inner,
            marshaler: Arc::new(marshaler),
            options: MuxOptions::default(),
        }
    }
}

impl<S: Clone> Clone for Gateway<S> {
    fn clone(&self) -> Self {
        Self {
            router: Arc::clone(&self.router),
            inner: self.inner.clone(),
            marshaler: Arc::clone(&self.marshaler),
            options: self.options,
        }
    }
}

/// The body [`Gateway`] answers with: at most one data frame (already
/// encoded — `abada::json` already ran), then, only when the response
/// carries them, one trailers frame. Not a general-purpose body type;
/// `abada` writes it, nothing else needs to.
pub struct ResponseBody {
    data: Option<Bytes>,
    trailers: Option<http::HeaderMap>,
}

impl ResponseBody {
    fn new(data: Vec<u8>, trailers: http::HeaderMap) -> Self {
        Self {
            data: Some(Bytes::from(data)),
            trailers: (!trailers.is_empty()).then_some(trailers),
        }
    }
}

impl http_body::Body for ResponseBody {
    type Data = Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        mut self: Pin<&mut Self>,
        _cx: &mut Context<'_>,
    ) -> Poll<Option<Result<http_body::Frame<Self::Data>, Self::Error>>> {
        if let Some(data) = self.data.take() {
            if !data.is_empty() {
                return Poll::Ready(Some(Ok(http_body::Frame::data(data))));
            }
        }
        if let Some(trailers) = self.trailers.take() {
            return Poll::Ready(Some(Ok(http_body::Frame::trailers(trailers))));
        }
        Poll::Ready(None)
    }
}

impl<S, ReqBody> tower::Service<http::Request<ReqBody>> for Gateway<S>
where
    S: tonic::client::GrpcService<tonic::body::Body> + Clone + Send + 'static,
    S::Future: Send,
    S::ResponseBody: tonic::codegen::Body<Data = Bytes> + Send + 'static,
    <S::ResponseBody as tonic::codegen::Body>::Error: Into<tonic::codegen::StdError> + Send,
    ReqBody: http_body::Body + Send + 'static,
    ReqBody::Data: Send,
    ReqBody::Error: std::fmt::Display,
{
    type Response = http::Response<ResponseBody>;
    type Error = std::convert::Infallible;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, req: http::Request<ReqBody>) -> Self::Future {
        let router = Arc::clone(&self.router);
        let inner = self.inner.clone();
        let marshaler = Arc::clone(&self.marshaler);
        let options = self.options;
        Box::pin(async move { Ok(handle(&router, inner, &marshaler, options, req).await) })
    }
}

async fn handle<S, ReqBody>(
    router: &path::Router<Registration>,
    inner: S,
    marshaler: &Marshaler,
    options: MuxOptions,
    req: http::Request<ReqBody>,
) -> http::Response<ResponseBody>
where
    S: tonic::client::GrpcService<tonic::body::Body>,
    S::ResponseBody: tonic::codegen::Body<Data = Bytes> + Send + 'static,
    <S::ResponseBody as tonic::codegen::Body>::Error: Into<tonic::codegen::StdError> + Send,
    ReqBody: http_body::Body,
    ReqBody::Error: std::fmt::Display,
{
    let (parts, body) = req.into_parts();

    let target = parts
        .uri
        .path_and_query()
        .map(|pq| pq.as_str())
        .unwrap_or("/");
    let request_path = match RequestPath::parse(target) {
        Ok(p) => p,
        Err(_) => return write(ErrorResponse::routing(RoutingError::BadRequest)),
    };

    let body_bytes = match body.collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(e) => {
            let status = Status::new(CODE_INVALID_ARGUMENT, format!("reading the body: {e}"));
            return write(ErrorResponse::from_status(&status, None, false));
        }
    };

    let content_type = parts
        .headers
        .get(http::header::CONTENT_TYPE)
        .map(http::HeaderValue::as_bytes);
    let method_override = parts
        .headers
        .get("x-http-method-override")
        .map(http::HeaderValue::as_bytes);
    let incoming = Incoming {
        method: parts.method.as_str(),
        target,
        content_type,
        method_override,
        body: &body_bytes,
    };

    let dispatch = request::dispatch(router, &request_path, &incoming, &options);
    let route_outcome = match dispatch.outcome {
        DispatchOutcome::FormError { escapes, error } => {
            let mut body = Vec::new();
            for escape in &escapes {
                body.extend(ErrorResponse::malformed_escape(escape).body);
            }
            let mut response = ErrorResponse::from_status(&error.status(), None, false);
            response.body = {
                body.extend(response.body);
                body
            };
            return write(response);
        }
        DispatchOutcome::Route(outcome) => outcome,
    };
    let RouteOutcome::Matched {
        handler: registration,
        params,
    } = &route_outcome
    else {
        return write(ErrorResponse::for_route(&route_outcome).expect("not a match"));
    };

    let http_request = HttpRequest {
        method: &dispatch.method,
        raw_query: request::raw_query(target),
        content_type,
        body: &body_bytes,
        form: dispatch.form.as_ref(),
    };
    let message: DynamicMessage =
        match registration
            .binding
            .decode(&http_request, params, marshaler)
        {
            Ok(message) => message,
            Err(e) => return write(ErrorResponse::from_status(&e.status(), None, false)),
        };

    let host = parts
        .headers
        .get(http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    // No peer address: a generic `http::Request<ReqBody>` carries none —
    // `X-Forwarded-For` from the immediate peer is not set here. See
    // docs/DESIGN.md's "Gateway" section.
    let incoming_metadata = match metadata::incoming(&parts.headers, host, None) {
        Ok(m) => m,
        Err(e) => {
            let status = Status::new(CODE_INVALID_ARGUMENT, e.to_string());
            return write(ErrorResponse::from_status(&status, None, false));
        }
    };
    let wants_trailers = accepts_trailers(parts.headers.get(http::header::TE));

    let response_desc = registration.binding.method().output();
    let codec = DynamicCodec::new(registration.binding.method().input(), response_desc);
    let mut grpc = tonic::client::Grpc::new(inner);
    match call::unary(
        &mut grpc,
        &registration.rpc_path,
        codec,
        message,
        &incoming_metadata,
        marshaler,
        wants_trailers,
    )
    .await
    {
        Ok(response) => to_http_response(
            response.status,
            response.headers,
            response.body,
            response.trailers,
        ),
        Err(err) => write(*err),
    }
}

fn write(response: ErrorResponse) -> http::Response<ResponseBody> {
    to_http_response(
        response.status,
        response.headers,
        response.body,
        response.trailers,
    )
}

fn to_http_response(
    status: http::StatusCode,
    headers: http::HeaderMap,
    body: Vec<u8>,
    trailers: http::HeaderMap,
) -> http::Response<ResponseBody> {
    http::Response::builder()
        .status(status)
        .body(ResponseBody::new(body, trailers))
        .map(|mut r| {
            *r.headers_mut() = headers;
            r
        })
        .expect("a status code abada built is valid")
}
