//! Generates, from the committed descriptor set and without protoc, the prost
//! types a tonic user would have, plus pbjson serde impls for them. Twice: the
//! pbjson option that decides whether default values are written is chosen at
//! build time, and grpc-gateway's default and its common override differ on it.

use std::path::PathBuf;

use prost::Message;

fn main() {
    let binpb = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/contracts/delonix-node-v1.binpb");
    println!("cargo:rerun-if-changed={}", binpb.display());
    let raw = std::fs::read(&binpb).unwrap();
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap());

    for (dir, emit_fields) in [("emit", true), ("omit", false)] {
        let dir = out.join(dir);
        std::fs::create_dir_all(&dir).unwrap();
        let set = prost_types::FileDescriptorSet::decode(raw.as_slice()).unwrap();
        prost_build::Config::new()
            .out_dir(&dir)
            .compile_well_known_types()
            .extern_path(".google.protobuf", "::pbjson_types")
            .compile_fds(set)
            .unwrap();
        let mut b = pbjson_build::Builder::new();
        b.register_descriptors(&raw)
            .unwrap()
            .out_dir(&dir)
            .ignore_unknown_fields();
        if emit_fields {
            b.emit_fields();
        }
        b.build(&[".delonix", ".google.api"]).unwrap();
    }
}
