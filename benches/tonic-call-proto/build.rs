//! Compiles `proto/service.proto` twice: once as ordinary tonic/prost server
//! code (so a real `<Service>Server<T>` exists to plug into
//! `tonic::client::Grpc<S>`), and emits the `FileDescriptorSet` bytes so the
//! prototype can build a `prost_reflect::DynamicMessage` for the request and
//! response the same way `abada::request::RequestBinding::decode` already
//! does, without depending on the generated prost types for the message
//! shape itself.

use std::path::PathBuf;

fn main() {
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let descriptor_path = out.join("service_descriptor.bin");
    tonic_prost_build::configure()
        .build_client(false)
        .file_descriptor_set_path(&descriptor_path)
        .compile_protos(&["proto/service.proto"], &["proto"])
        .unwrap();
}
