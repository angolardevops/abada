//! Replays grpc-gateway's own error responses (`conformance/vectors/errors.json`,
//! produced by `scripts/regen-vectors.sh`) against `abada::error`. Every
//! disagreement is listed, not just the first.

use abada::error::{
    self, Any, ErrorResponse, RoutingError, ServerMetadata, Status, http_status_from_code,
};
use abada::json::Marshaler;
use abada::path::{Pattern, RequestPath, Router, UnescapingMode};
use http::HeaderValue;
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    quote_not_printable_2byte: Vec<(u32, u32)>,
    codes: Vec<CodeVector>,
    statuses: Vec<StatusVector>,
    routes: Vec<RouteVector>,
}

#[derive(Deserialize)]
struct CodeVector {
    code: i32,
    status: u16,
}

#[derive(Deserialize, Default)]
struct AnyCase {
    #[serde(default)]
    type_url: String,
    #[serde(default)]
    value_hex: String,
}

#[derive(Deserialize)]
struct Wire {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
    #[serde(default)]
    trailers: Vec<(String, String)>,
}

#[derive(Deserialize)]
struct StatusVector {
    name: String,
    code: i32,
    message: String,
    #[serde(default)]
    details: Vec<AnyCase>,
    #[serde(default)]
    metadata: bool,
    #[serde(default)]
    header_md: Vec<(String, String)>,
    #[serde(default)]
    trailer_md: Vec<(String, String)>,
    #[serde(default)]
    te: String,
    #[serde(default)]
    registered: Vec<bool>,
    #[serde(flatten)]
    wire: Wire,
}

#[derive(Deserialize)]
struct Handler {
    method: String,
    template: String,
}

#[derive(Deserialize)]
struct RouteVector {
    name: String,
    mode: String,
    handlers: Vec<Handler>,
    method: String,
    target: String,
    #[serde(flatten)]
    wire: Wire,
}

/// Vectors where abada knowingly writes a different response: `net/http`
/// writes a header value with a control byte as it is over HTTP/1.1, and
/// `http::HeaderValue` cannot hold one, so abada drops that header.
const DROPPED_HEADER_VALUES: &[&str] = &["unauth-control", "md-header-control"];

/// Vectors with a detail grpc-gateway's registry resolves: abada's default
/// registry must resolve the same ones, and render them the same way.
const REGISTERED_DETAILS: &[&str] = &[
    "details-registered-duration",
    "details-registered-status",
    "details-registered-bad-value",
    "details-registered-other-host",
];

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/errors.json"
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

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

type Seen = (u16, Vec<(String, String)>, String, Vec<(String, String)>);

fn lines(map: &http::HeaderMap) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = map
        .iter()
        .map(|(k, v)| {
            (
                k.as_str().to_string(),
                String::from_utf8_lossy(v.as_bytes()).into_owned(),
            )
        })
        .collect();
    sort_like_oracle(&mut out);
    out
}

/// By name, keeping the order of values, except `trailer`, whose values the
/// oracle sorts because grpc-gateway announces them in map order.
fn sort_like_oracle(lines: &mut [(String, String)]) {
    lines.sort_by(|a, b| {
        a.0.cmp(&b.0).then_with(|| {
            if a.0 == "trailer" {
                a.1.cmp(&b.1)
            } else {
                std::cmp::Ordering::Equal
            }
        })
    });
}

fn seen(r: &ErrorResponse) -> Seen {
    (
        r.status.as_u16(),
        lines(&r.headers),
        String::from_utf8(r.body.clone()).expect("UTF-8 body"),
        lines(&r.trailers),
    )
}

/// The oracle's answer, less the header values `http` cannot represent.
/// Returns how many were removed.
fn expected(w: &Wire) -> (Seen, usize) {
    let valid = |(_, v): &(String, String)| HeaderValue::from_str(v).is_ok();
    let headers: Vec<_> = w.headers.iter().filter(|l| valid(l)).cloned().collect();
    let trailers: Vec<_> = w.trailers.iter().filter(|l| valid(l)).cloned().collect();
    let dropped = w.headers.len() - headers.len() + w.trailers.len() - trailers.len();
    ((w.status, headers, w.body.clone(), trailers), dropped)
}

#[test]
fn quote_table_is_strconv_is_print() {
    let v = vectors();
    assert_eq!(
        error::not_printable_2byte(),
        &v.quote_not_printable_2byte[..]
    );
}

#[test]
fn codes_map_to_http_status_like_grpc_gateway() {
    let v = vectors();
    let failures = v
        .codes
        .iter()
        .filter(|c| http_status_from_code(c.code).as_u16() != c.status)
        .map(|c| {
            format!(
                "code {}: {} want {}",
                c.code,
                http_status_from_code(c.code),
                c.status
            )
        })
        .collect();
    report("code", &v.generator, v.codes.len(), failures);
}

#[test]
fn statuses_are_written_like_grpc_gateway() {
    let v = vectors();
    let mut failures = Vec::new();
    let mut registered = Vec::new();
    let mut dropped_in = Vec::new();
    for s in &v.statuses {
        let status = Status {
            code: s.code,
            message: s.message.clone(),
            details: s
                .details
                .iter()
                .map(|d| Any {
                    type_url: d.type_url.clone(),
                    value: unhex(&d.value_hex),
                })
                .collect(),
        };
        let pairs = |p: &[(String, String)]| -> Vec<(String, Vec<u8>)> {
            p.iter()
                .map(|(k, v)| (k.clone(), v.as_bytes().to_vec()))
                .collect()
        };
        let md = ServerMetadata {
            headers: pairs(&s.header_md),
            trailers: pairs(&s.trailer_md),
        };
        let te = (!s.te.is_empty()).then(|| HeaderValue::from_str(&s.te).expect("TE"));
        let got = ErrorResponse::from_status(
            &status,
            s.metadata.then_some(&md),
            error::accepts_trailers(te.as_ref()),
        );

        if s.registered.iter().any(|&r| r) {
            registered.push(s.name.as_str());
        }
        // The oracle's registry is its binary's; abada's default registry
        // must agree on every type the vectors name.
        for (d, &go_resolves) in s.details.iter().zip(&s.registered) {
            let resolves = !d.type_url.is_empty()
                && Marshaler::default()
                    .registry()
                    .find_message_by_url(&d.type_url)
                    .is_some();
            if resolves != go_resolves {
                failures.push(format!(
                    "{}: {} resolves in grpc-gateway: {go_resolves}, in abada: {resolves}",
                    s.name, d.type_url
                ));
            }
        }

        let (want, dropped) = expected(&s.wire);
        if dropped > 0 {
            dropped_in.push(s.name.as_str());
        }
        let got = seen(&got);
        if got != want {
            failures.push(format!("{}: got {:?}\n    want {:?}", s.name, got, want));
        }
    }
    assert_eq!(
        registered, REGISTERED_DETAILS,
        "vectors with registered details"
    );
    assert_eq!(
        dropped_in, DROPPED_HEADER_VALUES,
        "vectors with dropped values"
    );
    report("status", &v.generator, v.statuses.len(), failures);
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
fn routing_errors_are_written_like_grpc_gateway() {
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
        let (want, dropped) = expected(&r.wire);
        assert_eq!(dropped, 0, "{}: routing errors carry no metadata", r.name);
        let got = match RequestPath::parse(&r.target) {
            Err(_) if !r.target.starts_with('/') => {
                Some(ErrorResponse::routing(RoutingError::BadRequest))
            }
            Err(e) => panic!("{}: {e}", r.name),
            Ok(req) => {
                let outcome = router.route(&r.method, &req);
                let mut response = ErrorResponse::for_route(&outcome);
                // What a matched handler writes follows the error bodies.
                if let (Some(resp), Some(i)) = (response.as_mut(), matched(&outcome)) {
                    resp.body.extend(format!("handler {i}").bytes());
                }
                response
            }
        };
        match got {
            None => failures.push(format!("{}: abada routed it cleanly", r.name)),
            Some(got) => {
                let got = seen(&got);
                if got != want {
                    failures.push(format!("{}: got {:?}\n    want {:?}", r.name, got, want));
                }
            }
        }
    }
    report("routing error", &v.generator, v.routes.len(), failures);
}

fn matched(outcome: &abada::path::RouteOutcome<'_, usize>) -> Option<usize> {
    use abada::path::RouteOutcome;
    match outcome {
        RouteOutcome::Matched { handler, .. } => Some(**handler),
        RouteOutcome::BadRequest { then, .. } => matched(then),
        _ => None,
    }
}
