//! Replays `conformance/vectors/request-*.json` — what the code
//! protoc-gen-grpc-gateway v2.27.3 generates put in the gRPC request for each
//! HTTP request, or answered instead — through abada's router, `ServeMux`
//! pre-handler step and request population.
//!
//! Messages are compared as values (a canonical rendering of the decoded
//! message), not as bytes: map order in the encoding is not a behaviour.
//! Errors are compared as the HTTP status and the list of `google.rpc.Status`
//! bodies written. The text of an error whose origin is the JSON decoder is
//! not compared, only its code: that text belongs to the codec (see
//! `abada::request::ErrorOrigin`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use abada::error::ErrorResponse;
use abada::json::{Marshaler, TypeRegistry};
use abada::path::{Pattern, RequestPath, RouteOutcome, Router, UnescapingMode};
use abada::request::{
    BindingOptions, DispatchOutcome, ErrorOrigin, HttpRequest, Incoming, MuxOptions,
    RequestBinding, dispatch, raw_query,
};
use base64::Engine;
use prost_reflect::{DescriptorPool, DynamicMessage, MapKey, ReflectMessage, Value};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    strconv_not_printable: Vec<(u32, u32)>,
    vectors: Vec<Vector>,
    #[serde(default)]
    dropped_nondeterministic: Vec<String>,
}

#[derive(Deserialize)]
struct Vector {
    name: String,
    #[serde(default)]
    mux: String,
    method: String,
    target: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    body: String,
    outcome: String,
    #[serde(default)]
    rpc: String,
    #[serde(default)]
    message: String,
    #[serde(default)]
    status: u16,
    #[serde(default)]
    errors: Vec<StatusBody>,
    #[serde(default)]
    raw_body: String,
}

#[derive(Deserialize, Debug, Clone, PartialEq)]
struct StatusBody {
    code: i32,
    message: String,
}

#[derive(Deserialize)]
struct ContractVectors {
    bindings: Vec<GoBinding>,
}

#[derive(Deserialize)]
struct GoBinding {
    service: String,
    method: String,
    http_method: String,
    template: String,
    #[serde(default)]
    body: String,
    #[serde(default)]
    response_body: String,
}

/// Cases where abada knowingly answers differently: grpc-gateway builds a
/// request nested as deep as the query names (its walk is a loop and Go's stack
/// grows), abada refuses one nested more than 100 levels, the root included —
/// see `abada::request::fields::MAX_MESSAGE_DEPTH` and DESIGN.md, "Query field
/// paths". The vector says `request`; abada must say 400, code 3. Those
/// vectors are not decoded: prost stops at 101 levels, and a vector's 5 000
/// levels cannot be.
const DEEPER_THAN_LIMIT: &[&str] = &[
    "query-depth-101",
    "query-depth-102",
    "query-depth-100-timestamp-leaf",
    "query-depth-1000",
    "query-depth-5000",
];

fn root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../conformance")
}

fn contracts() -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(root().join("cases"))
        .expect("conformance/cases")
        .filter_map(|e| {
            let name = e.ok()?.file_name().to_string_lossy().into_owned();
            Some(
                name.strip_prefix("request-")?
                    .strip_suffix(".json")?
                    .to_string(),
            )
        })
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no request cases");
    names
}

/// A value rendering in which equal messages are equal strings: fields in
/// number order, only those present, maps sorted, floats by bits.
fn canonical(msg: &DynamicMessage) -> String {
    fn value(v: &Value, out: &mut String) {
        match v {
            Value::Bool(b) => out.push_str(&b.to_string()),
            Value::I32(i) => out.push_str(&i.to_string()),
            Value::I64(i) => out.push_str(&i.to_string()),
            Value::U32(i) => out.push_str(&i.to_string()),
            Value::U64(i) => out.push_str(&i.to_string()),
            Value::F32(f) => out.push_str(&format!("f32:{:#x}", f.to_bits())),
            Value::F64(f) => out.push_str(&format!("f64:{:#x}", f.to_bits())),
            Value::String(s) => out.push_str(&format!("{s:?}")),
            Value::Bytes(b) => out.push_str(&format!("{:?}", b.as_ref())),
            Value::EnumNumber(n) => out.push_str(&format!("enum:{n}")),
            Value::Message(m) => out.push_str(&canonical(m)),
            Value::List(items) => {
                out.push('[');
                for item in items {
                    value(item, out);
                    out.push(',');
                }
                out.push(']');
            }
            Value::Map(map) => {
                let sorted: BTreeMap<&MapKey, &Value> = map.iter().collect();
                out.push('{');
                for (k, v) in sorted {
                    out.push_str(&format!("{k:?}="));
                    value(v, out);
                    out.push(',');
                }
                out.push('}');
            }
        }
    }
    let mut out = String::from("{");
    let desc = msg.descriptor();
    let mut fields: Vec<_> = desc.fields().collect();
    fields.sort_by_key(|f| f.number());
    for f in fields {
        if !msg.has_field(&f) {
            continue;
        }
        let v = msg.get_field(&f);
        if matches!(v.as_ref(), Value::List(l) if l.is_empty())
            || matches!(v.as_ref(), Value::Map(m) if m.is_empty())
        {
            continue;
        }
        out.push_str(f.name());
        out.push(':');
        value(&v, &mut out);
        out.push(';');
    }
    out.push('}');
    out
}

fn mode(mux: &str) -> (UnescapingMode, MuxOptions) {
    match mux {
        "" => (UnescapingMode::Legacy, MuxOptions::default()),
        "no_fallback" => (
            UnescapingMode::Legacy,
            MuxOptions {
                disable_path_length_fallback: true,
            },
        ),
        "all_except_reserved" => (UnescapingMode::AllExceptReserved, MuxOptions::default()),
        "all_characters" => (UnescapingMode::AllCharacters, MuxOptions::default()),
        other => panic!("unknown mux {other}"),
    }
}

/// Splits a response body into the `google.rpc.Status` objects written.
fn statuses(body: &[u8]) -> Vec<StatusBody> {
    let mut out = Vec::new();
    let mut de = serde_json::Deserializer::from_slice(body).into_iter::<StatusBody>();
    for s in de.by_ref() {
        out.push(s.expect("a Status body"));
    }
    out
}

#[derive(Debug, PartialEq)]
enum Got {
    Request {
        rpc: String,
        message: String,
    },
    Error {
        status: u16,
        errors: Vec<(StatusBody, ErrorOrigin)>,
    },
}

struct Contract {
    /// The codec over the contract's types, as the Go gateway binary links
    /// them (`Any` in a body resolves through it).
    marshaler: Marshaler,
    bindings: Vec<(String, RequestBinding)>,
    routers: BTreeMap<String, Router<usize>>,
}

fn load(name: &str) -> Contract {
    let raw = std::fs::read(root().join(format!("contracts/{name}.binpb"))).unwrap();
    let pool = DescriptorPool::decode(raw.as_slice()).expect("descriptor set");
    let cv: ContractVectors = serde_json::from_str(
        &std::fs::read_to_string(root().join(format!("vectors/contract-{name}.json"))).unwrap(),
    )
    .unwrap();
    let mut bindings = Vec::new();
    for b in &cv.bindings {
        let method = pool
            .get_service_by_name(&b.service)
            .and_then(|s| s.methods().find(|m| m.name() == b.method))
            .unwrap_or_else(|| panic!("{}.{} in the pool", b.service, b.method));
        let pattern = Pattern::new(&b.template).unwrap();
        let binding = RequestBinding::new(
            method,
            &b.http_method,
            pattern.template(),
            &b.body,
            &b.response_body,
            &BindingOptions::default(),
        )
        .unwrap_or_else(|e| panic!("{} {}: {e}", b.http_method, b.template));
        bindings.push((format!("/{}/{}", b.service, b.method), binding));
    }
    let mut routers = BTreeMap::new();
    for mux in ["", "no_fallback", "all_except_reserved", "all_characters"] {
        let mut router = Router::new(mode(mux).0);
        for (i, b) in cv.bindings.iter().enumerate() {
            router.add(&b.http_method, Pattern::new(&b.template).unwrap(), i);
        }
        routers.insert(mux.to_string(), router);
    }
    Contract {
        marshaler: Marshaler::new(TypeRegistry::new(pool).expect("registry")),
        bindings,
        routers,
    }
}

fn routing_error(outcome: &RouteOutcome<'_, usize>) -> Got {
    let response = ErrorResponse::for_route(outcome).expect("not a match");
    Got::Error {
        status: response.status.as_u16(),
        errors: statuses(&response.body)
            .into_iter()
            .map(|s| (s, ErrorOrigin::Gateway))
            .collect(),
    }
}

fn run(c: &Contract, v: &Vector) -> Got {
    let router = &c.routers[&v.mux];
    let (_, options) = mode(&v.mux);
    let first = |name: &str| {
        v.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_bytes())
    };
    let path = RequestPath::parse(&v.target).expect("net/http accepted the target");
    let incoming = Incoming {
        method: &v.method,
        target: &v.target,
        content_type: first("Content-Type"),
        method_override: first("X-HTTP-Method-Override"),
        body: v.body.as_bytes(),
    };
    let d = dispatch(router, &path, &incoming, &options);
    let outcome = match d.outcome {
        DispatchOutcome::FormError { escapes, error } => {
            let mut errors: Vec<(StatusBody, ErrorOrigin)> = escapes
                .iter()
                .flat_map(|e| statuses(&ErrorResponse::malformed_escape(e).body))
                .map(|s| (s, ErrorOrigin::Gateway))
                .collect();
            let response = ErrorResponse::from_status(&error.status(), None, false);
            errors.extend(
                statuses(&response.body)
                    .into_iter()
                    .map(|s| (s, error.origin())),
            );
            return Got::Error {
                status: 400,
                errors,
            };
        }
        DispatchOutcome::Route(outcome) => outcome,
    };
    let RouteOutcome::Matched { handler, params } = &outcome else {
        return routing_error(&outcome);
    };
    let (rpc, binding) = &c.bindings[**handler];
    let request = HttpRequest {
        method: &d.method,
        raw_query: raw_query(&v.target),
        content_type: first("Content-Type"),
        body: v.body.as_bytes(),
        form: d.form.as_ref(),
    };
    match binding.decode(&request, params, &c.marshaler) {
        Ok(msg) => Got::Request {
            rpc: rpc.clone(),
            message: canonical(&msg),
        },
        Err(e) => {
            let response = ErrorResponse::from_status(&e.status(), None, false);
            Got::Error {
                status: response.status.as_u16(),
                errors: statuses(&response.body)
                    .into_iter()
                    .map(|s| (s, e.origin()))
                    .collect(),
            }
        }
    }
}

fn expected(c: &Contract, v: &Vector) -> Got {
    match v.outcome.as_str() {
        "request" => {
            let (_, binding) = c
                .bindings
                .iter()
                .find(|(rpc, _)| *rpc == v.rpc)
                .unwrap_or_else(|| panic!("{}: rpc {} not bound", v.name, v.rpc));
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(&v.message)
                .unwrap();
            let msg = DynamicMessage::decode(binding.method().input(), bytes.as_slice())
                .unwrap_or_else(|e| panic!("{}: {e}", v.name));
            Got::Request {
                rpc: v.rpc.clone(),
                message: canonical(&msg),
            }
        }
        "error" => {
            assert!(
                v.raw_body.is_empty(),
                "{}: body is not Status objects: {}",
                v.name,
                v.raw_body
            );
            Got::Error {
                status: v.status,
                errors: v
                    .errors
                    .iter()
                    .map(|s| (s.clone(), ErrorOrigin::Gateway))
                    .collect(),
            }
        }
        other => panic!("{}: outcome {other}", v.name),
    }
}

/// Equal, except that the text of a decoder's error is not compared.
fn agrees(got: &Got, want: &Got) -> bool {
    match (got, want) {
        (
            Got::Error {
                status: gs,
                errors: ge,
            },
            Got::Error {
                status: ws,
                errors: we,
            },
        ) => {
            gs == ws
                && ge.len() == we.len()
                && ge.iter().zip(we).all(|((g, origin), (w, _))| {
                    g.code == w.code && (*origin == ErrorOrigin::Decoder || g.message == w.message)
                })
        }
        _ => got == want,
    }
}

#[test]
fn requests_become_the_messages_grpc_gateway_sends() {
    let mut failures = Vec::new();
    let mut deviating = 0;
    for name in contracts() {
        let v: Vectors = serde_json::from_str(
            &std::fs::read_to_string(root().join(format!("vectors/request-{name}.json")))
                .expect("vectors: run scripts/regen-vectors.sh"),
        )
        .unwrap();
        let c = load(&name);
        let mut decoder_text = 0;
        let mut requests = 0;
        for vector in &v.vectors {
            let got = run(&c, vector);
            if DEEPER_THAN_LIMIT.contains(&vector.name.as_str()) {
                deviating += 1;
                let refused = matches!(&got, Got::Error { status: 400, errors }
                    if errors.len() == 1
                        && errors[0].0.code == 3
                        && errors[0].0.message == "exceeded max recursion depth");
                if vector.outcome != "request" || !refused {
                    failures.push(format!(
                        "{name}/{} should still deviate (grpc-gateway answers `request`, abada 400/3):\n  abada: {got:?}\n  vector: {}",
                        vector.name, vector.outcome
                    ));
                }
                continue;
            }
            let want = expected(&c, vector);
            if !agrees(&got, &want) {
                let target: String = vector.target.chars().take(120).collect();
                failures.push(format!(
                    "{name}/{} [{} {target}]:\n  abada:        {got:?}\n  grpc-gateway: {want:?}",
                    vector.name, vector.method
                ));
            }
            match &got {
                Got::Request { .. } => requests += 1,
                Got::Error { errors, .. } => {
                    decoder_text += errors
                        .iter()
                        .filter(|(_, o)| *o == ErrorOrigin::Decoder)
                        .count()
                }
            }
        }
        eprintln!(
            "{name}: {} vectors ({requests} requests), {} dropped as nondeterministic, {decoder_text} decoder error texts not compared, against {}",
            v.vectors.len(),
            v.dropped_nondeterministic.len(),
            v.generator
        );
    }
    assert_eq!(
        deviating,
        DEEPER_THAN_LIMIT.len(),
        "every deviation is listed and found in the vectors"
    );
    assert!(
        failures.is_empty(),
        "{} disagreements:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn every_binding_of_the_contract_is_exercised() {
    let v: Vectors = serde_json::from_str(
        &std::fs::read_to_string(root().join("vectors/request-delonix-node-v1.json")).unwrap(),
    )
    .unwrap();
    let c = load("delonix-node-v1");
    let reached: std::collections::BTreeSet<&str> = v
        .vectors
        .iter()
        .filter(|x| x.outcome == "request")
        .map(|x| x.rpc.as_str())
        .collect();
    let missing: Vec<&str> = c
        .bindings
        .iter()
        .map(|(rpc, _)| rpc.as_str())
        .filter(|rpc| !reached.contains(rpc))
        .collect();
    assert!(
        missing.is_empty(),
        "bindings without a request: {missing:?}"
    );
}

#[test]
fn strconv_is_print_table_is_gos() {
    let v: Vectors = serde_json::from_str(
        &std::fs::read_to_string(root().join("vectors/request-delonix-node-v1.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(
        abada::request::strconv_not_printable(),
        v.strconv_not_printable.as_slice()
    );
}
