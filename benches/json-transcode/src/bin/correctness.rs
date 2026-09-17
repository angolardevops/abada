//! Prints how each candidate compares with grpc-gateway on every JSON vector.
//! `cargo run -p abada-json-bench --bin correctness [--bless]`; `--bless`
//! rewrites `expected.txt`, which the test compares against.

fn main() {
    let findings = abada_json_bench::compare();
    let summary = abada_json_bench::summary(&findings);
    if std::env::args().any(|a| a == "--bless") {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/expected.txt");
        std::fs::write(path, &summary).unwrap();
        eprintln!("wrote {path}");
    }
    for f in &findings {
        println!(
            "{:<48} {:<13} decode={:?}\n{:<62} emit={:?}\n{:<62} omit={:?}",
            f.case, f.candidate, f.decode, "", f.encode_emit, "", f.encode_omit
        );
    }
}
