//! Replays grpc-gateway's JSON answers (`conformance/vectors/json-*.json`,
//! produced by `scripts/regen-vectors.sh`) against `abada::json`. Every
//! disagreement is listed, not just the first.
//!
//! - Decoding: accepted or rejected like grpc-gateway, and the decoded
//!   message is Go's — byte for byte in deterministic binary for the
//!   conformance protos (generated Go types), as messages for the contract
//!   (dynamicpb orders its bytes differently).
//! - Encoding starts from **Go's** decoded message, so a decoding bug cannot
//!   hide an encoding one, and compares the bytes: key order, number
//!   spelling and escapes included. Failing to encode must match too.

use abada::json::{Marshaler, TypeRegistry, testing};
use prost_reflect::{DynamicMessage, Kind, MessageDescriptor, ReflectMessage, Value};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    name: String,
    message: String,
    // `null` is an input too, which `Option` would turn into `None`.
    #[serde(default, deserialize_with = "raw")]
    input: Option<Box<serde_json::value::RawValue>>,
    #[serde(default)]
    body: Option<String>,
    #[serde(default)]
    body_hex: String,
    #[serde(default)]
    text: Option<String>,
    #[serde(default)]
    proto_hex: String,
    #[serde(default)]
    field: String,
    #[serde(default)]
    response_field: String,
    accepted: bool,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    output_error: Option<String>,
    #[serde(default)]
    output_omit_unpopulated: Option<String>,
    #[serde(default)]
    output_omit_error: Option<String>,
    #[serde(default)]
    proto: String,
    #[serde(default)]
    prefill_proto: String,
}

/// Cases where abada knowingly answers differently: grpc-gateway accepts
/// 10 000 nested messages, abada 100 (see `abada::json`).
const DEEPER_THAN_LIMIT: &[&str] = &["depth_101"];

fn raw<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<Box<serde_json::value::RawValue>>, D::Error> {
    Box::<serde_json::value::RawValue>::deserialize(d).map(Some)
}

fn load(file: &str) -> Vectors {
    let path = format!(
        "{}/../../conformance/vectors/{file}",
        env!("CARGO_MANIFEST_DIR")
    );
    serde_json::from_str(&std::fs::read_to_string(&path).expect("read vectors"))
        .expect("parse vectors")
}

fn registry(binpb: &str) -> TypeRegistry {
    let path = format!("{}/../../conformance/{binpb}", env!("CARGO_MANIFEST_DIR"));
    TypeRegistry::decode(&std::fs::read(path).expect("descriptor set")).expect("pool")
}

fn unbase64(s: &str) -> Vec<u8> {
    const A: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::new();
    let mut acc = 0u32;
    let mut bits = 0;
    for c in s.bytes().filter(|&c| c != b'=') {
        acc = acc << 6 | A.iter().position(|&a| a == c).expect("base64") as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
        }
    }
    out
}

fn unhex(s: &str) -> Vec<u8> {
    (0..s.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&s[i..i + 2], 16).expect("hex"))
        .collect()
}

/// Re-encodes every resolvable `Any` payload, so that two messages whose
/// payloads differ only in field order compare equal.
fn normalize(msg: &mut DynamicMessage, reg: &TypeRegistry) {
    let desc = msg.descriptor();
    if desc.full_name() == "google.protobuf.Any" {
        let url = msg
            .get_field_by_number(1)
            .unwrap()
            .as_str()
            .unwrap()
            .to_owned();
        if let Some(d) = reg.find_message_by_url(&url) {
            let bytes = msg
                .get_field_by_number(2)
                .unwrap()
                .as_bytes()
                .unwrap()
                .clone();
            if let Ok(mut inner) = testing::decode_binary(&d, &bytes) {
                normalize(&mut inner, reg);
                msg.set_field_by_number(
                    2,
                    Value::Bytes(testing::deterministic_binary(&inner).into()),
                );
            }
        }
        return;
    }
    for fd in desc.fields() {
        if !matches!(fd.kind(), Kind::Message(_)) || !msg.has_field(&fd) {
            continue;
        }
        match msg.get_field_mut(&fd) {
            Value::Message(m) => normalize(m, reg),
            Value::List(items) => items.iter_mut().for_each(|v| {
                if let Value::Message(m) = v {
                    normalize(m, reg)
                }
            }),
            Value::Map(map) => map.values_mut().for_each(|v| {
                if let Value::Message(m) = v {
                    normalize(m, reg)
                }
            }),
            _ => {}
        }
    }
}

fn body(c: &Case) -> Vec<u8> {
    if !c.body_hex.is_empty() {
        unhex(&c.body_hex)
    } else if let Some(b) = &c.body {
        b.as_bytes().to_vec()
    } else {
        c.input
            .as_ref()
            .map(|r| r.get().as_bytes().to_vec())
            .unwrap_or_default()
    }
}

fn show(r: &Result<Vec<u8>, abada::json::JsonError>) -> String {
    match r {
        Ok(b) => String::from_utf8_lossy(b).into_owned(),
        Err(e) => format!("error: {e}"),
    }
}

fn check_output(
    name: &str,
    what: &str,
    got: Result<Vec<u8>, abada::json::JsonError>,
    want: &Option<String>,
    want_error: &Option<String>,
    failures: &mut Vec<String>,
) {
    let ok = match (&got, want, want_error) {
        (Ok(b), Some(w), None) => b == w.as_bytes(),
        (Err(_), None, Some(_)) => true,
        _ => false,
    };
    if !ok {
        failures.push(format!(
            "{name} {what}: got {}\n    want {}",
            show(&got),
            want.clone()
                .unwrap_or_else(|| format!("error: {}", want_error.clone().unwrap_or_default()))
        ));
    }
}

/// Runs one vector file; `exact_binary` compares decoded messages byte for
/// byte (generated Go types) instead of as messages (dynamicpb).
fn replay(file: &str, binpb: &str, exact_binary: bool) {
    let v = load(file);
    let reg = registry(binpb);
    let emit = Marshaler::new(reg.clone());
    let mut omit = Marshaler::new(reg.clone());
    omit.marshal.emit_unpopulated = false;
    let mut failures = Vec::new();
    let mut deviations = Vec::new();
    for c in &v.cases {
        let desc: MessageDescriptor = reg
            .pool()
            .get_message_by_name(&c.message)
            .unwrap_or_else(|| panic!("{}: no message {}", c.name, c.message));
        let go_msg = || testing::decode_binary(&desc, &unbase64(&c.proto)).expect("Go's binary");

        let encode_only = c.text.is_some() || !c.proto_hex.is_empty();
        if !encode_only {
            let mut msg = if c.prefill_proto.is_empty() {
                DynamicMessage::new(desc.clone())
            } else {
                testing::decode_binary(&desc, &unbase64(&c.prefill_proto)).expect("prefill")
            };
            let result = if c.field.is_empty() {
                emit.decode_into(&mut msg, &body(c))
            } else {
                let fd = desc.get_field_by_name(&c.field).expect("field");
                emit.decode_field(&mut msg, &fd, &body(c))
            };
            if DEEPER_THAN_LIMIT.contains(&c.name.as_str()) {
                if c.accepted && result.is_err() {
                    deviations.push(c.name.as_str());
                } else {
                    failures.push(format!("{}: expected the depth deviation", c.name));
                }
                continue;
            }
            match (c.accepted, &result) {
                (true, Err(e)) => {
                    failures.push(format!(
                        "{}: rejected what grpc-gateway accepts: {e}",
                        c.name
                    ));
                    continue;
                }
                (false, Ok(())) => {
                    failures.push(format!(
                        "{}: accepted what grpc-gateway rejects ({})",
                        c.name,
                        show(&emit.encode(&msg))
                    ));
                    continue;
                }
                (false, Err(_)) => continue,
                (true, Ok(())) => {}
            }
            let want = unbase64(&c.proto);
            let got = testing::deterministic_binary(&msg);
            let same = if exact_binary {
                got == want
            } else {
                let (mut a, mut b) = (msg.clone(), go_msg());
                normalize(&mut a, &reg);
                normalize(&mut b, &reg);
                testing::deterministic_binary(&a) == testing::deterministic_binary(&b)
            };
            if !same {
                failures.push(format!(
                    "{}: decoded message differs\n    got  {}\n    want {}",
                    c.name,
                    show(&emit.encode(&msg)),
                    show(&emit.encode(&go_msg()))
                ));
            }
        }
        if !c.accepted {
            continue;
        }
        let msg = go_msg();
        if c.response_field.is_empty() {
            check_output(
                &c.name,
                "emit",
                emit.encode(&msg),
                &c.output,
                &c.output_error,
                &mut failures,
            );
            check_output(
                &c.name,
                "omit",
                omit.encode(&msg),
                &c.output_omit_unpopulated,
                &c.output_omit_error,
                &mut failures,
            );
        } else {
            let fd = desc.get_field_by_name(&c.response_field).expect("field");
            check_output(
                &c.name,
                "emit",
                emit.encode_field(&msg, &fd),
                &c.output,
                &c.output_error,
                &mut failures,
            );
            check_output(
                &c.name,
                "omit",
                omit.encode_field(&msg, &fd),
                &c.output_omit_unpopulated,
                &c.output_omit_error,
                &mut failures,
            );
        }
    }
    assert!(!v.cases.is_empty(), "{file}: no cases");
    assert!(
        failures.is_empty(),
        "{} disagreements with {} in {file} ({} cases):\n{}",
        failures.len(),
        v.generator,
        v.cases.len(),
        failures.join("\n")
    );
    if file.contains("conformance") {
        assert_eq!(deviations, DEEPER_THAN_LIMIT, "written deviations");
    }
}

#[test]
fn conformance_protos_like_grpc_gateway() {
    replay(
        "json-abada-conformance-v1.json",
        "protos/abada-conformance-v1.binpb",
        true,
    );
}

#[test]
fn delonix_contract_like_grpc_gateway() {
    replay(
        "json-delonix-node-v1.json",
        "contracts/delonix-node-v1.binpb",
        false,
    );
}
