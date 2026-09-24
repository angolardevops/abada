//! The `tower::Service` ([`gateway::Gateway`]) and what it is built from:
//! metadata in and out, the call itself, and (later) NDJSON streaming. See
//! [ADR 0002](../../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md)
//! for how in-process and proxy calls share the one path `call::unary` uses.

pub mod call;
pub mod codec;
pub mod gateway;
pub mod metadata;
pub mod response;

pub use gateway::{Gateway, Registration};
