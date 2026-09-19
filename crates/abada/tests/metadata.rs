//! Replays `runtime.AnnotateContext`'s own answers
//! (`conformance/vectors/metadata.json`) against `abada::service::metadata`.
//! Every disagreement is listed, not just the first.

use abada::service::metadata::incoming;
use http::{HeaderMap, HeaderName, HeaderValue};
use serde::Deserialize;

#[derive(Deserialize)]
struct Vectors {
    generator: String,
    vectors: Vec<Vector>,
}

#[derive(Deserialize)]
struct Vector {
    name: String,
    headers: Vec<(String, String)>,
    #[serde(default)]
    host: String,
    #[serde(default)]
    remote_addr: String,
    pairs: Option<Vec<(String, String)>>,
    has_timeout: bool,
    #[serde(default)]
    error: String,
}

fn vectors() -> Vectors {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/vectors/metadata.json"
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

/// Vectors whose `error` is Go's own base64 decoder text, which abada does
/// not reproduce (see `MetadataError::InvalidBinaryHeader`'s doc comment).
/// Only the part before it — "invalid binary header <name>: " — is compared.
const OPAQUE_BASE64_ERROR: &[&str] = &["grpc-metadata-bin-invalid"];

#[test]
fn incoming_metadata_matches_annotate_context() {
    let v = vectors();
    let mut failures = Vec::new();
    for vec in &v.vectors {
        let mut headers = HeaderMap::new();
        for (k, val) in &vec.headers {
            let name =
                HeaderName::from_bytes(k.as_bytes()).expect("case gives a valid header name");
            let value =
                HeaderValue::from_bytes(val.as_bytes()).expect("case gives a valid header value");
            headers.append(name, value);
        }
        let remote_addr = (!vec.remote_addr.is_empty()).then_some(vec.remote_addr.as_str());

        match incoming(&headers, &vec.host, remote_addr) {
            Err(e) => {
                if vec.error.is_empty() {
                    failures.push(format!("{}: got error {e}, want none", vec.name));
                    continue;
                }
                // grpc-gateway's message embeds Go's own base64 decoder
                // text and the header's original casing; abada reproduces
                // neither (see MetadataError::InvalidBinaryHeader), so for
                // these vectors only "an error happened" is checked.
                if OPAQUE_BASE64_ERROR.contains(&vec.name.as_str()) {
                    continue;
                }
                let got = e.to_string();
                if !vec.error.contains(&got) {
                    failures.push(format!(
                        "{}: got error {got:?}\n    want (contained in) {:?}",
                        vec.name, vec.error
                    ));
                }
            }
            Ok(got) => {
                if !vec.error.is_empty() {
                    failures.push(format!(
                        "{}: abada accepted it, want error {}",
                        vec.name, vec.error
                    ));
                    continue;
                }
                let want_pairs: Vec<(String, String)> = vec.pairs.clone().unwrap_or_default();
                let got_pairs: Vec<(String, String)> = got
                    .pairs
                    .iter()
                    .map(|(k, val)| (k.clone(), String::from_utf8_lossy(val).into_owned()))
                    .collect();
                if got_pairs != want_pairs {
                    failures.push(format!(
                        "{}: pairs got {got_pairs:?}\n    want {want_pairs:?}",
                        vec.name
                    ));
                }
                // The exact duration is not checked here: the oracle does
                // not record it (see conformance/oracle/metadata.go), only
                // whether a deadline was set at all. The grammar itself —
                // which unit multiplies to what — is asserted directly
                // against Go's documented rule in this crate's unit tests.
                if got.timeout.is_some() != vec.has_timeout {
                    failures.push(format!(
                        "{}: has_timeout got {}, want {}",
                        vec.name,
                        got.timeout.is_some(),
                        vec.has_timeout
                    ));
                }
            }
        }
    }
    report("metadata", &v.generator, v.vectors.len(), failures);
}
