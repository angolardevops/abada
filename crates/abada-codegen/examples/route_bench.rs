//! Routing cost over a real contract: every canonical request of
//! `conformance/vectors/contract-<name>.json`, all bindings registered.
//! `cargo run --release -p abada-codegen --example route_bench [name]`

use std::hint::black_box;
use std::time::Instant;

use abada::path::{Pattern, RequestPath, RouteOutcome, Router, UnescapingMode};

fn main() {
    let name = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "delonix-node-v1".into());
    let root = concat!(env!("CARGO_MANIFEST_DIR"), "/../../conformance");
    let raw = std::fs::read(format!("{root}/contracts/{name}.binpb")).unwrap();
    let vectors: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(format!("{root}/vectors/contract-{name}.json")).unwrap(),
    )
    .unwrap();

    let bindings = abada_codegen::bindings(&raw).unwrap();
    let mut router = Router::new(UnescapingMode::Legacy);
    for (i, b) in bindings.iter().enumerate() {
        router.add(&b.http_method, Pattern::new(&b.template).unwrap(), i);
    }
    let requests: Vec<(String, String)> = vectors["routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["canonical"].as_bool() == Some(true))
        .map(|r| {
            (
                r["method"].as_str().unwrap().to_string(),
                r["target"].as_str().unwrap().to_string(),
            )
        })
        .collect();

    // Parsing the target is part of what a request costs, so it is inside.
    let rounds = 20_000;
    let mut samples = Vec::new();
    for _ in 0..7 {
        let start = Instant::now();
        let mut matched = 0usize;
        for _ in 0..rounds {
            for (method, target) in &requests {
                let req = RequestPath::parse(target).unwrap();
                if let RouteOutcome::Matched { .. } = router.route(method, black_box(&req)) {
                    matched += 1;
                }
            }
        }
        let elapsed = start.elapsed();
        assert_eq!(matched, rounds * requests.len());
        samples.push(elapsed.as_nanos() as f64 / (rounds * requests.len()) as f64);
    }
    samples.sort_by(f64::total_cmp);
    println!(
        "abada route: {} bindings, {} requests, median {:.0} ns/request (min {:.0}, max {:.0})",
        bindings.len(),
        requests.len(),
        samples[3],
        samples[0],
        samples[6]
    );
}
