//! Replays grpc-gateway's own answers (`conformance/vectors/path.json`, produced
//! by `scripts/regen-vectors.sh`) against abada. Every disagreement is listed,
//! not just the first, so a regression shows its whole shape.

use std::collections::BTreeMap;

use abada::path::{
    PathTemplate, Pattern, RequestPath, RouteOutcome, Router, UnescapingMode, path_kept_bytes,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    path_unescaped_bytes: String,
    templates: Vec<TemplateVector>,
    routes: Vec<RouteVector>,
}

#[derive(Deserialize)]
struct TemplateVector {
    template: String,
    parses: bool,
    #[serde(default)]
    display: String,
    #[serde(default)]
    op_codes: Vec<i64>,
    #[serde(default)]
    pool: Vec<String>,
    #[serde(default)]
    verb: String,
    #[serde(default)]
    fields: Vec<String>,
    valid: bool,
}

#[derive(Deserialize)]
struct Handler {
    method: String,
    template: String,
}

#[derive(Deserialize)]
struct RouteVector {
    name: String,
    #[serde(default)]
    mode: String,
    handlers: Vec<Handler>,
    method: String,
    target: String,
    outcome: String,
    #[serde(default)]
    handler: usize,
    #[serde(default)]
    params: BTreeMap<String, String>,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/path.json"
    );
    serde_json::from_str(&std::fs::read_to_string(path).expect("read vectors"))
        .expect("parse vectors")
}

fn report(what: &str, generator: &str, total: usize, failures: Vec<String>) {
    assert!(total > 0, "no {what} vectors: the file is empty");
    assert!(
        failures.is_empty(),
        "{} of {total} {what} disagree with {generator}:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn request_paths_are_escaped_like_net_url() {
    let v = vectors();
    let hex: String = path_kept_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(
        hex, v.path_unescaped_bytes,
        "bytes kept unescaped in a path"
    );
}

#[test]
fn templates_parse_and_compile_like_grpc_gateway() {
    let v = vectors();
    let mut failures = Vec::new();
    for t in &v.templates {
        let parsed = PathTemplate::parse(&t.template);
        if parsed.is_ok() != t.parses {
            failures.push(format!(
                "{:?}: parses={} want {}",
                t.template,
                parsed.is_ok(),
                t.parses
            ));
            continue;
        }
        let Ok(parsed) = parsed else { continue };
        // grpc-gateway prints the root's EOF sentinel; abada prints "/".
        let display = t.display.replace('\0', "");
        if parsed.to_string() != display {
            failures.push(format!(
                "{:?}: display {:?} want {:?}",
                t.template,
                parsed.to_string(),
                display
            ));
        }
        let c = parsed.compile();
        if (&c.op_codes, &c.pool, &c.verb, &c.fields) != (&t.op_codes, &t.pool, &t.verb, &t.fields)
        {
            failures.push(format!(
                "{:?}: compiled {:?} {:?} {:?} {:?} want {:?} {:?} {:?} {:?}",
                t.template,
                c.op_codes,
                c.pool,
                c.verb,
                c.fields,
                t.op_codes,
                t.pool,
                t.verb,
                t.fields
            ));
        }
        let valid = Pattern::from_template(parsed).is_ok();
        if valid != t.valid {
            failures.push(format!("{:?}: valid={valid} want {}", t.template, t.valid));
        }
    }
    report("template", &v.generator, v.templates.len(), failures);
}

fn mode(name: &str) -> UnescapingMode {
    match name {
        "" | "legacy" => UnescapingMode::Legacy,
        "all_except_reserved" => UnescapingMode::AllExceptReserved,
        "all_except_slash" => UnescapingMode::AllExceptSlash,
        "all_characters" => UnescapingMode::AllCharacters,
        other => panic!("unknown mode {other}"),
    }
}

#[test]
fn requests_route_like_grpc_gateway() {
    let v = vectors();
    let mut failures = Vec::new();
    for r in &v.routes {
        let mut router = Router::new(mode(&r.mode));
        for (i, h) in r.handlers.iter().enumerate() {
            router.add(
                &h.method,
                Pattern::new(&h.template).expect("oracle accepted it"),
                i,
            );
        }
        let got = match RequestPath::parse(&r.target) {
            Err(_) => ("invalid_target".to_string(), 0, BTreeMap::new()),
            Ok(req) => match router.route(&r.method, &req) {
                RouteOutcome::Matched { handler, params } => (
                    "matched".to_string(),
                    *handler,
                    params
                        .iter()
                        .map(|(k, v)| (k.to_string(), String::from_utf8_lossy(v).into_owned()))
                        .collect(),
                ),
                RouteOutcome::NotFound => ("not_found".into(), 0, BTreeMap::new()),
                RouteOutcome::MethodNotAllowed => ("method_not_allowed".into(), 0, BTreeMap::new()),
                RouteOutcome::BadRequest { .. } => ("bad_request".into(), 0, BTreeMap::new()),
            },
        };
        let want = (r.outcome.clone(), r.handler, r.params.clone());
        if got != want {
            failures.push(format!(
                "{} [{} {} mode={}]: got {:?} want {:?}",
                r.name, r.method, r.target, r.mode, got, want
            ));
        }
    }
    report("route", &v.generator, v.routes.len(), failures);
}
