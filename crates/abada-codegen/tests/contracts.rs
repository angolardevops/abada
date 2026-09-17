//! Real contracts in `conformance/contracts/`: abada must read the same
//! bindings grpc-gateway reads, and route every request the way its ServeMux
//! does with all of them registered at once.

use std::collections::BTreeMap;
use std::path::PathBuf;

use abada::path::{Pattern, RequestPath, RouteOutcome, Router, UnescapingMode};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    source: String,
    bindings: Vec<GoBinding>,
    routes: Vec<Route>,
}

#[derive(Deserialize, Debug, PartialEq)]
struct GoBinding {
    service: String,
    method: String,
    http_method: String,
    template: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    response_body: String,
    #[serde(default)]
    client_streaming: bool,
    #[serde(default)]
    server_streaming: bool,
    index: usize,
}

#[derive(Deserialize)]
struct Route {
    from: usize,
    #[serde(default)]
    canonical: bool,
    mode: String,
    method: String,
    target: String,
    outcome: String,
    #[serde(default)]
    handler: usize,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

fn contracts() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root().join("contracts"))
        .expect("conformance/contracts")
        .filter_map(|e| {
            let p = e.ok()?.path();
            if p.extension()? != "binpb" {
                return None;
            }
            Some(p.file_stem()?.to_string_lossy().into_owned())
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no contract in conformance/contracts");
    names
}

fn mode(name: &str) -> UnescapingMode {
    match name {
        "legacy" => UnescapingMode::Legacy,
        "all_except_reserved" => UnescapingMode::AllExceptReserved,
        other => panic!("mode {other} not generated for contracts"),
    }
}

#[test]
fn contracts_bind_and_route_like_grpc_gateway() {
    let mut failures = Vec::new();
    for name in contracts() {
        let raw = std::fs::read(root().join(format!("contracts/{name}.binpb"))).unwrap();
        let v: Vectors = serde_json::from_str(
            &std::fs::read_to_string(root().join(format!("vectors/contract-{name}.json")))
                .expect("vectors: run scripts/regen-vectors.sh"),
        )
        .unwrap();

        let ours = abada_codegen::bindings(&raw).expect("abada reads the contract");
        let ours: Vec<GoBinding> = ours
            .into_iter()
            .map(|b| GoBinding {
                service: b.service,
                method: b.method,
                http_method: b.http_method,
                template: b.template,
                body: b.body,
                response_body: b.response_body,
                client_streaming: b.client_streaming,
                server_streaming: b.server_streaming,
                index: b.index,
            })
            .collect();
        if ours != v.bindings {
            failures.push(format!(
                "{name}: bindings differ\n  abada: {ours:#?}\n  grpc-gateway: {:#?}",
                v.bindings
            ));
            continue;
        }

        let mut routers: BTreeMap<String, Router<usize>> = BTreeMap::new();
        let mut canonical = 0;
        for r in &v.routes {
            let router = routers.entry(r.mode.clone()).or_insert_with(|| {
                let mut router = Router::new(mode(&r.mode));
                for (i, b) in v.bindings.iter().enumerate() {
                    router.add(&b.http_method, Pattern::new(&b.template).unwrap(), i);
                }
                router
            });
            let got = match RequestPath::parse(&r.target) {
                Err(_) => ("invalid_target".to_string(), 0, BTreeMap::new()),
                Ok(req) => match router.route(&r.method, &req) {
                    RouteOutcome::Matched { handler, params } => (
                        "matched".into(),
                        *handler,
                        params
                            .iter()
                            .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v).into_owned()))
                            .collect(),
                    ),
                    RouteOutcome::NotFound => ("not_found".into(), 0, BTreeMap::new()),
                    RouteOutcome::MethodNotAllowed => {
                        ("method_not_allowed".into(), 0, BTreeMap::new())
                    }
                    RouteOutcome::BadRequest { .. } => ("bad_request".into(), 0, BTreeMap::new()),
                },
            };
            if r.canonical {
                canonical += 1;
                if got.0 != "matched" || got.1 != r.from {
                    let b = &v.bindings[r.from];
                    failures.push(format!(
                        "{name}: {}.{} is unreachable: {} {} gave {:?}",
                        b.service, b.method, r.method, r.target, got
                    ));
                }
            }
            let want = (r.outcome.clone(), r.handler, r.params.clone());
            if got != want {
                failures.push(format!(
                    "{name} [{} {} mode={} from={}]: got {got:?} want {want:?}",
                    r.method, r.target, r.mode, r.from
                ));
            }
        }
        assert_eq!(
            canonical,
            v.bindings.len(),
            "{name}: one canonical route per binding"
        );
        eprintln!(
            "{name} ({}): {} bindings, {} routes agree with {}",
            v.source,
            v.bindings.len(),
            v.routes.len(),
            v.generator
        );
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
