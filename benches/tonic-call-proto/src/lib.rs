//! Evidence crate, not published: does calling a unary RPC through
//! `tonic::client::Grpc<S>` work identically whether `S` is a generated
//! `<Service>Server<T>` used directly, in-process, with no socket — or a real
//! `tonic::transport::Channel` over loopback? And can the request/response
//! messages be `prost_reflect::DynamicMessage`s, driven only by the
//! descriptor, instead of the generated prost types — the same shape
//! `abada::request::RequestBinding::decode` already produces?
//!
//! Nothing here is `abada` API; this crate only measures a design candidate
//! for a future ADR on abada's `service` module.

pub mod pb {
    tonic::include_proto!("abada.protocall.v1");

    pub const DESCRIPTOR_BYTES: &[u8] =
        include_bytes!(concat!(env!("OUT_DIR"), "/service_descriptor.bin"));
}

pub mod codec;
pub mod server;

pub use codec::DynamicCodec;
pub use server::EchoImpl;
