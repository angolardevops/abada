# abada

REST/JSON gateways for [tonic](https://github.com/hyperium/tonic) gRPC services,
generated from `google.api.http` annotations — the Rust counterpart of Go's
[grpc-gateway](https://github.com/grpc-ecosystem/grpc-gateway).

**Status: pre-release.** Nothing is published yet; see [docs/DESIGN.md](docs/DESIGN.md).

## Planned usage

As a `protoc`/`buf` plugin:

```bash
cargo install protoc-gen-abada
```

```yaml
# buf.gen.yaml
version: v2
plugins:
  - local: protoc-gen-abada
    out: src/gen
```

Or from `build.rs`:

```rust
abada_build::configure().compile_protos(&["proto/api.proto"], &["proto"])?;
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Contributors and AI agents follow
[AGENTS.md](AGENTS.md).

## License

Apache-2.0.
