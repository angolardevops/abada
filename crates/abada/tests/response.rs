//! Replays grpc-gateway's own `ForwardResponseMessage` answers
//! (`conformance/vectors/response.json`) against
//! `abada::service::response::Response`. Every disagreement is listed, not
//! just the first.

use abada::error::{self, ServerMetadata};
use abada::json::Marshaler;
use abada::service::response::Response;
use http::HeaderValue;
use prost_reflect::{DynamicMessage, Value};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    vectors: Vec<Vector>,
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
struct Vector {
    name: String,
    code: i32,
    message: String,
    #[serde(default)]
    metadata: bool,
    #[serde(default)]
    header_md: Vec<(String, String)>,
    #[serde(default)]
    trailer_md: Vec<(String, String)>,
    #[serde(default)]
    te: String,
    #[serde(flatten)]
    wire: Wire,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/response.json"
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
    out.sort_by(|a, b| a.0.cmp(&b.0));
    out
}

fn seen(r: &Response) -> Seen {
    (
        r.status.as_u16(),
        lines(&r.headers),
        String::from_utf8(r.body.clone()).expect("UTF-8 body"),
        lines(&r.trailers),
    )
}

fn expected(w: &Wire) -> Seen {
    let mut headers = w.headers.clone();
    let mut trailers = w.trailers.clone();
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    trailers.sort_by(|a, b| a.0.cmp(&b.0));
    (w.status, headers, w.body.clone(), trailers)
}

fn status_message(code: i32, message: &str) -> DynamicMessage {
    let pool = Marshaler::default().registry().pool().clone();
    let desc = pool
        .get_message_by_name("google.rpc.Status")
        .expect("google.rpc.Status is always in the registry");
    let mut msg = DynamicMessage::new(desc);
    msg.set_field_by_number(1, Value::I32(code));
    msg.set_field_by_number(2, Value::String(message.to_string()));
    msg
}

#[test]
fn successful_responses_match_forward_response_message() {
    let v = vectors();
    let mut failures = Vec::new();
    for vec in &v.vectors {
        let message = status_message(vec.code, &vec.message);
        let metadata = vec.metadata.then(|| {
            let pairs = |p: &[(String, String)]| -> Vec<(String, Vec<u8>)> {
                p.iter()
                    .map(|(k, val)| (k.clone(), val.as_bytes().to_vec()))
                    .collect()
            };
            ServerMetadata {
                headers: pairs(&vec.header_md),
                trailers: pairs(&vec.trailer_md),
            }
        });
        let te = (!vec.te.is_empty()).then(|| HeaderValue::from_str(&vec.te).expect("TE"));
        let got = Response::success(
            &Marshaler::default(),
            &message,
            metadata.as_ref(),
            error::accepts_trailers(te.as_ref()),
        )
        .expect("google.rpc.Status always encodes");

        let got = seen(&got);
        let want = expected(&vec.wire);
        if got != want {
            failures.push(format!("{}: got {:?}\n    want {:?}", vec.name, got, want));
        }
    }
    report("response", &v.generator, v.vectors.len(), failures);
}
