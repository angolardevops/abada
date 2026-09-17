//! Code generation core: reads a `FileDescriptorSet`, extracts every `HttpRule`
//! and emits the Rust gateway. Shared by `protoc-gen-abada` and `abada-build`,
//! so the two entry points can never generate different code.
//!
//! Status: pre-release skeleton. The design lives in `docs/DESIGN.md`.
