//! The two JSON transcoding candidates of ADR 0001, and the codec abada
//! built on the chosen one, side by side over the `delonix.node.v1`
//! contract, compared with grpc-gateway's own answers. See
//! `docs/adr/0001-transcodificacao-json.md`.
//!
//! - (a) `prost-reflect`: the body becomes a `DynamicMessage` through
//!   prost-reflect's serde support, and reaches the user's prost type through
//!   the binary encoding.
//! - (b) `pbjson`: serde impls generated for the user's prost types.
//! - `abada`: `abada::json::Marshaler`, a `DynamicMessage` too, without serde.

use prost::Message;
use prost_reflect::{
    DescriptorPool, DeserializeOptions, DynamicMessage, MessageDescriptor, SerializeOptions,
};
use serde::Serialize;
use serde::de::DeserializeOwned;

macro_rules! generated {
    ($dir:literal) => {
        #[allow(clippy::all, missing_docs)]
        pub mod google {
            pub mod api {
                include!(concat!(env!("OUT_DIR"), "/", $dir, "/google.api.rs"));
                include!(concat!(env!("OUT_DIR"), "/", $dir, "/google.api.serde.rs"));
            }
        }
        #[allow(clippy::all, missing_docs)]
        pub mod delonix {
            pub mod node {
                pub mod v1 {
                    include!(concat!(env!("OUT_DIR"), "/", $dir, "/delonix.node.v1.rs"));
                    include!(concat!(
                        env!("OUT_DIR"),
                        "/",
                        $dir,
                        "/delonix.node.v1.serde.rs"
                    ));
                }
            }
        }
    };
}

/// prost types with pbjson impls that write default values
/// (grpc-gateway's default, `EmitUnpopulated: true`).
pub mod emit {
    generated!("emit");
}

/// The same prost types with pbjson impls that skip default values
/// (`EmitUnpopulated: false`).
pub mod omit {
    generated!("omit");
}

/// The committed descriptor set of the contract.
pub const DESCRIPTOR_SET: &[u8] =
    include_bytes!("../../../conformance/contracts/delonix-node-v1.binpb");

/// Whether default values are written, as `protojson.MarshalOptions.EmitUnpopulated`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Mode {
    /// grpc-gateway's `runtime.ServeMux` default.
    Emit,
    /// `EmitUnpopulated: false`.
    Omit,
}

/// Candidate (a): `prost-reflect`.
pub mod reflect {
    use super::*;

    /// Builds the descriptor pool: the one-time cost of this candidate.
    pub fn pool() -> DescriptorPool {
        DescriptorPool::decode(DESCRIPTOR_SET).expect("descriptor set")
    }

    /// The inbound options of grpc-gateway's default marshaler: unknown fields
    /// are discarded, not rejected.
    pub const DECODE: DeserializeOptions = DeserializeOptions::new().deny_unknown_fields(false);

    /// The outbound options for a mode: lowerCamelCase names, enums by name,
    /// 64-bit integers as strings.
    pub const fn encode_options(mode: Mode) -> SerializeOptions {
        SerializeOptions::new().skip_default_fields(matches!(mode, Mode::Omit))
    }

    /// Request path: JSON body into a dynamic message.
    pub fn decode(desc: &MessageDescriptor, body: &[u8]) -> Result<DynamicMessage, String> {
        let mut de = serde_json::Deserializer::from_slice(body);
        let msg = DynamicMessage::deserialize_with_options(desc.clone(), &mut de, &DECODE)
            .map_err(|e| e.to_string())?;
        de.end().map_err(|e| e.to_string())?;
        Ok(msg)
    }

    /// Response path: dynamic message into a JSON body.
    pub fn encode(msg: &DynamicMessage, mode: Mode) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(256);
        let mut ser = serde_json::Serializer::new(&mut out);
        msg.serialize_with_options(&mut ser, &encode_options(mode))
            .map_err(|e| e.to_string())?;
        Ok(out)
    }

    /// Request path, all the way to the user's type: what an in-process call
    /// to a tonic service costs with this candidate.
    pub fn decode_typed<M: Message + Default>(
        desc: &MessageDescriptor,
        body: &[u8],
    ) -> Result<M, String> {
        let dynamic = decode(desc, body)?;
        M::decode(dynamic.encode_to_vec().as_slice()).map_err(|e| e.to_string())
    }

    /// Response path from the user's type.
    pub fn encode_typed<M: Message>(
        desc: &MessageDescriptor,
        msg: &M,
        mode: Mode,
    ) -> Result<Vec<u8>, String> {
        let dynamic = DynamicMessage::decode(desc.clone(), msg.encode_to_vec().as_slice())
            .map_err(|e| e.to_string())?;
        encode(&dynamic, mode)
    }
}

/// `abada::json`, the codec built on (a).
pub mod abada_codec {
    use super::*;
    use abada::json::{Marshaler, TypeRegistry};

    /// The default marshaler for a mode, over the contract.
    pub fn marshaler(mode: Mode) -> Marshaler {
        let mut m = Marshaler::new(TypeRegistry::decode(DESCRIPTOR_SET).expect("descriptor set"));
        m.marshal.emit_unpopulated = matches!(mode, Mode::Emit);
        m
    }

    /// Request path: JSON body into a dynamic message, as for `body: "*"`.
    pub fn decode(
        m: &Marshaler,
        desc: &MessageDescriptor,
        body: &[u8],
    ) -> Result<DynamicMessage, String> {
        m.decode(desc, body).map_err(|e| e.to_string())
    }

    /// Response path: dynamic message into a JSON body.
    pub fn encode(m: &Marshaler, msg: &DynamicMessage) -> Result<Vec<u8>, String> {
        m.encode(msg).map_err(|e| e.to_string())
    }
}

/// Candidate (b): `pbjson`.
pub mod pbjson {
    use super::*;

    /// Request path: JSON body into the user's type.
    pub fn decode<M: DeserializeOwned>(body: &[u8]) -> Result<M, String> {
        serde_json::from_slice(body).map_err(|e| e.to_string())
    }

    /// Response path: the user's type into a JSON body.
    pub fn encode<M: Serialize>(msg: &M) -> Result<Vec<u8>, String> {
        let mut out = Vec::with_capacity(256);
        serde_json::to_writer(&mut out, msg).map_err(|e| e.to_string())?;
        Ok(out)
    }
}

/// Evaluates `$f::<Type>($args)` with the generated type (from `emit` or
/// `omit`) named by a full message name, for the messages the cases use.
#[macro_export]
macro_rules! with_type {
    ($module:ident, $name:expr, $f:ident ( $($arg:expr),* )) => {{
        use $crate::$module::delonix::node::v1 as v1;
        match $name {
            "delonix.node.v1.CreateContainerRequest" => Some($f::<v1::CreateContainerRequest>($($arg),*)),
            "delonix.node.v1.CreateVirtualMachineRequest" => Some($f::<v1::CreateVirtualMachineRequest>($($arg),*)),
            "delonix.node.v1.Container" => Some($f::<v1::Container>($($arg),*)),
            "delonix.node.v1.ListContainersResponse" => Some($f::<v1::ListContainersResponse>($($arg),*)),
            "delonix.node.v1.Operation" => Some($f::<v1::Operation>($($arg),*)),
            "delonix.node.v1.LogChunk" => Some($f::<v1::LogChunk>($($arg),*)),
            "delonix.node.v1.UpdateContainerRequest" => Some($f::<v1::UpdateContainerRequest>($($arg),*)),
            "delonix.node.v1.GetContainerRequest" => Some($f::<v1::GetContainerRequest>($($arg),*)),
            "delonix.node.v1.Mount" => Some($f::<v1::Mount>($($arg),*)),
            "delonix.node.v1.Resources" => Some($f::<v1::Resources>($($arg),*)),
            "delonix.node.v1.PortMapping" => Some($f::<v1::PortMapping>($($arg),*)),
            "delonix.node.v1.Condition" => Some($f::<v1::Condition>($($arg),*)),
            "delonix.node.v1.ErrorDetail" => Some($f::<v1::ErrorDetail>($($arg),*)),
            _ => None,
        }
    }};
}

/// One case of `conformance/vectors/json-<contract>.json`.
#[derive(serde::Deserialize, Debug)]
pub struct Vector {
    /// Case name.
    pub name: String,
    /// Full message name.
    pub message: String,
    /// The body, byte for byte as written in the case file.
    pub input: Box<serde_json::value::RawValue>,
    /// Bench-only list size; see the oracle.
    #[serde(default)]
    pub repeat: usize,
    /// Whether the case is benchmarked.
    #[serde(default)]
    pub bench: bool,
    /// Whether grpc-gateway's default marshaler decodes the body.
    pub accepted: bool,
    /// grpc-gateway's error, for reading only.
    #[serde(default)]
    pub error: String,
    /// grpc-gateway's answer with `EmitUnpopulated: true`, compacted.
    #[serde(default)]
    pub output: String,
    /// grpc-gateway's answer with `EmitUnpopulated: false`, compacted.
    #[serde(default)]
    pub output_omit_unpopulated: String,
    /// The decoded message, deterministic binary, base64; absent when empty.
    #[serde(default)]
    pub proto: String,
}

/// Reads the vectors grpc-gateway wrote for the contract.
pub fn vectors() -> Vec<Vector> {
    #[derive(serde::Deserialize)]
    struct File {
        cases: Vec<Vector>,
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/json-delonix-node-v1.json"
    );
    let f: File = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    f.cases
}

/// Cycles the elements of the body's array fields up to `repeat`, as the
/// oracle's `json-bench` does.
pub fn expand(input: &str, repeat: usize) -> Vec<u8> {
    if repeat == 0 {
        return input.as_bytes().to_vec();
    }
    let mut v: serde_json::Value = serde_json::from_str(input).unwrap();
    for field in v.as_object_mut().unwrap().values_mut() {
        if let Some(items) = field.as_array_mut() {
            let src = items.clone();
            *items = (0..repeat).map(|i| src[i % src.len()].clone()).collect();
        }
    }
    serde_json::to_vec(&v).unwrap()
}

/// How a candidate's answer compares with grpc-gateway's.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    /// Encode: the same JSON text once whitespace and object key order are
    /// set aside. Key order is not compared: prost's maps are `HashMap`s, so
    /// map keys come out in a different order on every run, where Go sorts
    /// them. Decode: the same message.
    Identical,
    /// Encode, for abada only: the same bytes as grpc-gateway's compacted
    /// output, key order and number spelling included.
    SameBytes,
    /// Encode, for abada only: the same JSON value as `Identical` means it,
    /// but not the same bytes.
    OtherBytes,
    /// The same JSON value, but a number is spelled differently (`0` vs `0.0`).
    SameValue,
    /// Accepted by both, different answer; the first differing path, with
    /// grpc-gateway's value first.
    Differs(String),
    /// grpc-gateway accepts, the candidate rejects; the candidate's error.
    Rejects(String),
    /// grpc-gateway rejects, the candidate accepts.
    Accepts,
    /// Both reject (decode), or nothing to compare (encode).
    BothReject,
    /// The candidate fails to write a message grpc-gateway writes.
    EncodeFails(String),
}

/// The comparison of one candidate on one case.
#[derive(Debug)]
pub struct Finding {
    /// Case name.
    pub case: String,
    /// `prost-reflect`, `pbjson` or `abada`.
    pub candidate: &'static str,
    /// Decoding: accepted/rejected like grpc-gateway, and the same message.
    pub decode: Verdict,
    /// Encoding of grpc-gateway's decoded message, `EmitUnpopulated: true`.
    pub encode_emit: Verdict,
    /// Same, `EmitUnpopulated: false`.
    pub encode_omit: Verdict,
}

/// Every place where two JSON values differ, with array indices folded into
/// `[*]` so that one cause repeated over a list reads once. Numbers compare by
/// value: `0` and `0.0` are the same number (the byte comparison still sees them).
fn differences(path: &str, a: &serde_json::Value, b: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    match (a, b) {
        (Value::Object(x), Value::Object(y)) => {
            let mut keys: Vec<&String> = x.keys().chain(y.keys()).collect();
            keys.sort();
            keys.dedup();
            for k in keys {
                match (x.get(k), y.get(k)) {
                    (Some(p), Some(q)) => differences(&format!("{path}.{k}"), p, q, out),
                    (p, q) => out.push(format!("{path}.{k}: {} vs {}", show(p), show(q))),
                }
            }
        }
        (Value::Array(x), Value::Array(y)) if x.len() == y.len() => {
            for (p, q) in x.iter().zip(y) {
                differences(&format!("{path}[*]"), p, q, out);
            }
        }
        (Value::Number(x), Value::Number(y)) if x.as_f64() == y.as_f64() => {}
        _ if a == b => {}
        _ => out.push(format!("{path}: {} vs {}", show(Some(a)), show(Some(b)))),
    }
}

fn first_difference(a: &serde_json::Value, b: &serde_json::Value) -> Option<String> {
    let mut out = Vec::new();
    differences("$", a, b, &mut out);
    // Folded paths repeat; keep the first occurrence of each.
    let mut seen = std::collections::HashSet::new();
    out.retain(|d| seen.insert(d.split(':').next().unwrap_or(d).to_string()));
    if out.is_empty() {
        None
    } else {
        Some(out.join("; "))
    }
}

fn show(v: Option<&serde_json::Value>) -> String {
    match v {
        None => "absent".into(),
        Some(v) => {
            let s = v.to_string();
            match s.char_indices().nth(60) {
                Some((i, _)) => format!("{}…", &s[..i]),
                None => s,
            }
        }
    }
}

fn compare_output(expected: &str, got: Result<Vec<u8>, String>) -> Verdict {
    let got = match got {
        Ok(g) => g,
        Err(e) => return Verdict::EncodeFails(e),
    };
    let want: serde_json::Value = serde_json::from_str(expected).unwrap();
    let have: serde_json::Value = serde_json::from_slice(&got).unwrap();
    // Value equality ignores object key order and tells an integer from a
    // float (`0` is not `0.0`); everything else must match.
    if want == have {
        return Verdict::Identical;
    }
    match first_difference(&want, &have) {
        None => Verdict::SameValue,
        Some(d) => Verdict::Differs(d),
    }
}

/// Like `compare_output`, then byte for byte.
fn compare_output_exact(expected: &str, got: Result<Vec<u8>, String>) -> Verdict {
    match got {
        Ok(bytes) if bytes == expected.as_bytes() => Verdict::SameBytes,
        Ok(bytes) => match compare_output(expected, Ok(bytes)) {
            Verdict::Identical => Verdict::OtherBytes,
            other => other,
        },
        Err(e) => Verdict::EncodeFails(e),
    }
}

fn compare_decode(
    desc: &MessageDescriptor,
    accepted: bool,
    got: Result<Vec<u8>, String>,
    go_proto: &[u8],
) -> Verdict {
    match (accepted, got) {
        (false, Err(_)) => Verdict::BothReject,
        (false, Ok(_)) => Verdict::Accepts,
        (true, Err(e)) => Verdict::Rejects(e),
        (true, Ok(bytes)) => {
            // Both sides go through the binary form into a DynamicMessage, so
            // map order and encoder choices do not count as differences.
            let want = DynamicMessage::decode(desc.clone(), go_proto).unwrap();
            let have = DynamicMessage::decode(desc.clone(), bytes.as_slice()).unwrap();
            if want == have {
                return Verdict::Identical;
            }
            let as_value = |m: &DynamicMessage| {
                m.serialize_with_options(
                    serde_json::value::Serializer,
                    &reflect::encode_options(Mode::Omit),
                )
                .unwrap()
            };
            Verdict::Differs(
                first_difference(&as_value(&want), &as_value(&have))
                    .unwrap_or_else(|| "binary only".into()),
            )
        }
    }
}

fn pbjson_decode_to_proto<M: Message + DeserializeOwned>(body: &[u8]) -> Result<Vec<u8>, String> {
    pbjson::decode::<M>(body).map(|m| m.encode_to_vec())
}

fn pbjson_encode_from_proto<M: Message + Default + Serialize>(
    proto: &[u8],
) -> Result<Vec<u8>, String> {
    pbjson::encode(&M::decode(proto).map_err(|e| e.to_string())?)
}

/// Compares both candidates with grpc-gateway on every vector. Decoding is
/// checked against grpc-gateway's decoded message; encoding starts from
/// grpc-gateway's decoded message, so a decoding bug cannot hide an encoding one.
pub fn compare() -> Vec<Finding> {
    use base64::Engine;
    let pool = reflect::pool();
    let abada_emit = abada_codec::marshaler(Mode::Emit);
    let abada_omit = abada_codec::marshaler(Mode::Omit);
    let mut out = Vec::new();
    for v in vectors() {
        let desc = pool.get_message_by_name(&v.message).expect("message");
        let body = v.input.get().as_bytes();
        let go_proto = base64::engine::general_purpose::STANDARD
            .decode(&v.proto)
            .unwrap();

        let decoded = reflect::decode(&desc, body).map(|m| m.encode_to_vec());
        let (emit, omit) = if v.accepted {
            let m = DynamicMessage::decode(desc.clone(), go_proto.as_slice()).unwrap();
            (
                compare_output(&v.output, reflect::encode(&m, Mode::Emit)),
                compare_output(&v.output_omit_unpopulated, reflect::encode(&m, Mode::Omit)),
            )
        } else {
            (Verdict::BothReject, Verdict::BothReject)
        };
        out.push(Finding {
            case: v.name.clone(),
            candidate: "prost-reflect",
            decode: compare_decode(&desc, v.accepted, decoded, &go_proto),
            encode_emit: emit,
            encode_omit: omit,
        });

        let decoded = abada_codec::decode(&abada_emit, &desc, body).map(|m| m.encode_to_vec());
        let (emit, omit) = if v.accepted {
            let m = DynamicMessage::decode(desc.clone(), go_proto.as_slice()).unwrap();
            (
                compare_output_exact(&v.output, abada_codec::encode(&abada_emit, &m)),
                compare_output_exact(
                    &v.output_omit_unpopulated,
                    abada_codec::encode(&abada_omit, &m),
                ),
            )
        } else {
            (Verdict::BothReject, Verdict::BothReject)
        };
        out.push(Finding {
            case: v.name.clone(),
            candidate: "abada",
            decode: compare_decode(&desc, v.accepted, decoded, &go_proto),
            encode_emit: emit,
            encode_omit: omit,
        });

        let message = v.message.as_str();
        let decoded = with_type!(emit, message, pbjson_decode_to_proto(body)).expect("type");
        let (emit, omit) = if v.accepted {
            (
                compare_output(
                    &v.output,
                    with_type!(emit, message, pbjson_encode_from_proto(&go_proto)).unwrap(),
                ),
                compare_output(
                    &v.output_omit_unpopulated,
                    with_type!(omit, message, pbjson_encode_from_proto(&go_proto)).unwrap(),
                ),
            )
        } else {
            (Verdict::BothReject, Verdict::BothReject)
        };
        out.push(Finding {
            case: v.name,
            candidate: "pbjson",
            decode: compare_decode(&desc, v.accepted, decoded, &go_proto),
            encode_emit: emit,
            encode_omit: omit,
        });
    }
    out
}

impl Verdict {
    fn render(&self) -> String {
        match self {
            Verdict::Identical => "identical".into(),
            Verdict::SameBytes => "same-bytes".into(),
            Verdict::OtherBytes => "other-bytes".into(),
            Verdict::SameValue => "number-spelling".into(),
            Verdict::Differs(d) => format!("differs[{d}]"),
            // The candidate's error text belongs to the library, not to the finding.
            Verdict::Rejects(_) => "rejects".into(),
            Verdict::Accepts => "accepts".into(),
            Verdict::BothReject => "-".into(),
            Verdict::EncodeFails(_) => "encode-fails".into(),
        }
    }
}

/// One line per case and candidate: `case candidate decode emit omit`. The
/// committed `expected.txt` is this text; a change in either library, or in
/// grpc-gateway's vectors, shows up as a diff of it.
pub fn summary(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        out.push_str(&format!(
            "{} {} decode={} emit={} omit={}\n",
            f.case,
            f.candidate,
            f.decode.render(),
            f.encode_emit.render(),
            f.encode_omit.render()
        ));
    }
    out
}
