//! Code generation core: reads a `FileDescriptorSet`, extracts every `HttpRule`
//! and (later) emits the Rust gateway. Shared by `protoc-gen-abada` and
//! `abada-build`, so the two entry points can never generate different code.

mod descriptor;
mod rules;

pub use rules::{Binding, ExtractError, bindings};
