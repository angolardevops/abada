//! The pieces a `tower::Service` needs to actually call an RPC: metadata in
//! and out, and (later) the call itself and NDJSON streaming. See
//! [ADR 0002](../../../../docs/adr/0002-chamada-do-rpc-in-process-e-proxy.md)
//! for how in-process and proxy calls share one path once this module calls
//! anything; nothing here does yet.

pub mod metadata;
