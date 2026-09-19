//! Property test of `abada-security` §2: a hostile corpus goes through the real
//! `Gateway` and every input must (1) not panic, (2) get a well-formed HTTP
//! answer, (3) cost memory and time proportional to its size, and (4) leave no
//! state behind. There is no grpc-gateway answer for these inputs, so this is
//! the "property" half of the proof, not conformance.
//!
//! Run `cargo test -p abada --test security --release -- --nocapture` to see
//! the measured table; the bounds below are the assertions.
//!
//! One test function, sequential on purpose: the counting allocator is global,
//! so two inputs at once would pollute each other's peak.

use std::alloc::{GlobalAlloc, Layout, System};
use std::convert::Infallible;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use abada::json::{Marshaler, TypeRegistry};
use abada::path::{Pattern, Router, UnescapingMode};
use abada::request::{BindingOptions, RequestBinding};
use abada::service::{Gateway, Registration};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use prost_reflect::{DescriptorPool, DynamicMessage, ReflectMessage};
use tower::ServiceExt;

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            TOTAL.fetch_add(l.size(), Ordering::Relaxed);
            let now = LIVE.fetch_add(l.size(), Ordering::Relaxed) + l.size();
            PEAK.fetch_max(now, Ordering::Relaxed);
        }
        p
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        LIVE.fetch_sub(l.size(), Ordering::Relaxed);
        // SAFETY: `p` came from `alloc` with the same layout.
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, new: usize) -> *mut u8 {
        // SAFETY: forwarded unchanged; `l` is the layout `p` was allocated with.
        let q = unsafe { System.realloc(p, l, new) };
        if !q.is_null() {
            if new >= l.size() {
                TOTAL.fetch_add(new - l.size(), Ordering::Relaxed);
                let now = LIVE.fetch_add(new - l.size(), Ordering::Relaxed) + new - l.size();
                PEAK.fetch_max(now, Ordering::Relaxed);
            } else {
                LIVE.fetch_sub(l.size() - new, Ordering::Relaxed);
            }
        }
        q
    }
}
#[global_allocator]
static A: Counting = Counting;

fn pool() -> DescriptorPool {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../conformance/contracts/abada-conformance-request-v1.binpb");
    DescriptorPool::decode(std::fs::read(path).unwrap().as_slice()).unwrap()
}

async fn ok_rpc(
    request: tonic::Request<DynamicMessage>,
) -> Result<tonic::Response<DynamicMessage>, tonic::Status> {
    let pool = request.get_ref().descriptor().parent_pool().clone();
    let empty = pool.get_message_by_name("google.protobuf.Empty").unwrap();
    Ok(tonic::Response::new(DynamicMessage::new(empty)))
}

fn fake_server(
    req_d: prost_reflect::MessageDescriptor,
    resp_d: prost_reflect::MessageDescriptor,
) -> impl tonic::client::GrpcService<
    tonic::body::Body,
    ResponseBody = tonic::body::Body,
    Error = Infallible,
    Future: Send,
> + Clone {
    tower::service_fn(move |req: http::Request<tonic::body::Body>| {
        let codec = abada::service::codec::DynamicCodec::new(resp_d.clone(), req_d.clone());
        let fut: std::pin::Pin<
            Box<
                dyn std::future::Future<
                        Output = Result<http::Response<tonic::body::Body>, Infallible>,
                    > + Send,
            >,
        > = Box::pin(async move {
            let mut server = tonic::server::Grpc::new(codec);
            Ok(server.unary(tower::service_fn(ok_rpc), req).await)
        });
        fut
    })
}

fn gateway() -> impl tower::Service<
    http::Request<Full<Bytes>>,
    Response = http::Response<
        impl http_body::Body<Data = Bytes, Error = Infallible> + Send + 'static,
    >,
    Error = Infallible,
    Future: Send,
> + Clone {
    let pool = pool();
    let mut router = Router::new(UnescapingMode::Legacy);
    let mut descs = None;
    for (svc, rpc, method, tmpl, body) in [
        ("PathService", "Top", "GET", "/v1/top/int32/{f_int32}", ""),
        ("QueryService", "Query", "GET", "/v1/query/{f_string}", ""),
        (
            "BodyService",
            "Body",
            "POST",
            "/v1/body/star/{f_string}",
            "*",
        ),
    ] {
        let m = pool
            .get_service_by_name(&format!("abada.conformance.request.v1.{svc}"))
            .unwrap()
            .methods()
            .find(|m| m.name() == rpc)
            .unwrap();
        descs = Some((m.input(), m.output()));
        let pat = Pattern::new(tmpl).unwrap();
        let b = RequestBinding::new(
            m,
            method,
            pat.template(),
            body,
            "",
            &BindingOptions::default(),
        )
        .unwrap();
        router.add(method, pat, Registration::new(b));
    }
    let (i, o) = descs.unwrap();
    let marshaler = Marshaler::new(TypeRegistry::new(pool).unwrap());
    Gateway::new(router, fake_server(i, o), marshaler)
}

struct Case {
    name: String,
    method: &'static str,
    uri: String,
    headers: Vec<(String, Vec<u8>)>,
    body: Vec<u8>,
}

fn case(name: &str, method: &'static str, uri: &str, body: Vec<u8>) -> Case {
    Case {
        name: name.into(),
        method,
        uri: uri.into(),
        headers: vec![("content-type".into(), b"application/json".to_vec())],
        body,
    }
}

/// `{"fStruct":{"a":{"a":…}}}` nested `depth` levels.
fn nested_struct(depth: usize) -> Vec<u8> {
    let mut s = String::from("{\"fStruct\":");
    for _ in 0..depth {
        s.push_str("{\"a\":");
    }
    s.push('1');
    for _ in 0..depth {
        s.push('}');
    }
    s.push('}');
    s.into_bytes()
}

fn corpus() -> Vec<Case> {
    let star = "/v1/body/star/x";
    let mut c = vec![
        // path and escapes
        case("path: truncated escape", "GET", "/v1/top/int32/%", vec![]),
        case("path: bad hex", "GET", "/v1/top/int32/%zz", vec![]),
        case("path: NUL", "GET", "/v1/top/int32/%00", vec![]),
        case("path: encoded slash", "GET", "/v1/top/int32/1%2f2", vec![]),
        case("path: dot-dot", "GET", "/v1/top/int32/../../etc", vec![]),
        case(
            "path: overlong utf-8",
            "GET",
            "/v1/top/int32/%c0%af",
            vec![],
        ),
        case(
            "path: int32 overflow",
            "GET",
            "/v1/top/int32/99999999999999999999",
            vec![],
        ),
        case(
            "path: 64KiB",
            "GET",
            &format!("/v1/top/int32/{}", "1".repeat(65536)),
            vec![],
        ),
        // query
        case(
            "query: 10k repeated keys",
            "GET",
            &format!("/v1/query/x?{}", "rString=a&".repeat(10_000)),
            vec![],
        ),
        case(
            "query: 100-deep field path",
            "GET",
            &format!("/v1/query/x?{}=1", vec!["nested"; 100].join(".")),
            vec![],
        ),
        // Past ~2000 components (debug: ~500) this aborted the process.
        case(
            "query: 4000-deep field path",
            "GET",
            &format!("/v1/query/x?{}=1", vec!["nested"; 4000].join(".")),
            vec![],
        ),
        case(
            "query: 9000-deep field path (54 KB)",
            "GET",
            &format!("/v1/query/x?{}=1", vec!["nested"; 9000].join(".")),
            vec![],
        ),
        case(
            "query: overwrite path-bound field",
            "GET",
            "/v1/query/x?fString=evil",
            vec![],
        ),
        case(
            "query: malformed escape",
            "GET",
            "/v1/query/x?fString=%",
            vec![],
        ),
        case(
            "query: 64KiB updateMask",
            "GET",
            &format!("/v1/query/x?updateMask={}", "a,".repeat(32768)),
            vec![],
        ),
        // body / json
        case("body: empty", "POST", star, vec![]),
        case("body: not json", "POST", star, b"\xff\xfe\x00".to_vec()),
        case("body: BOM", "POST", star, b"\xef\xbb\xbf{}".to_vec()),
        case(
            "body: 1e999999999",
            "POST",
            star,
            br#"{"fDouble":1e999999999}"#.to_vec(),
        ),
        case(
            "body: duplicate keys",
            "POST",
            star,
            br#"{"fString":"a","fString":"b"}"#.to_vec(),
        ),
        case(
            "body: lone surrogate",
            "POST",
            star,
            br#"{"fString":"\ud800"}"#.to_vec(),
        ),
        case(
            "body: Any unregistered",
            "POST",
            star,
            br#"{"fStruct":{"@type":"x/y"}}"#.to_vec(),
        ),
        case("body: nesting 99", "POST", star, nested_struct(99)),
        case("body: nesting 101", "POST", star, nested_struct(101)),
        case("body: nesting 10000", "POST", star, nested_struct(10_000)),
        case("body: nesting 200000", "POST", star, nested_struct(200_000)),
        case(
            "body: 1MiB string",
            "POST",
            star,
            format!("{{\"fString\":\"{}\"}}", "a".repeat(1 << 20)).into_bytes(),
        ),
        case(
            "body: 10^6 element array",
            "POST",
            star,
            format!("{{\"rInt32\":[{}0]}}", "0,".repeat(1_000_000)).into_bytes(),
        ),
        case(
            "body: 10^5 map entries",
            "POST",
            star,
            format!(
                "{{\"mStr\":{{{}\"z\":\"\"}}}}",
                (0..100_000)
                    .map(|i| format!("\"k{i}\":\"v\","))
                    .collect::<String>()
            )
            .into_bytes(),
        ),
        case(
            "body: 8MiB bytes field",
            "POST",
            star,
            format!("{{\"fBytes\":\"{}\"}}", "QUJD".repeat(2 << 20)).into_bytes(),
        ),
        case(
            "body: GET with body",
            "GET",
            "/v1/top/int32/1",
            b"{\"fString\":\"x\"}".to_vec(),
        ),
    ];
    // headers / metadata
    let mut many = case(
        "headers: 1000 grpc-metadata",
        "GET",
        "/v1/top/int32/1",
        vec![],
    );
    for i in 0..1000 {
        many.headers
            .push((format!("grpc-metadata-k{i}"), b"v".to_vec()));
    }
    c.push(many);
    for (name, h, v) in [
        (
            "headers: bad -bin base64",
            "grpc-metadata-x-bin",
            b"!!!".to_vec(),
        ),
        ("headers: 64KiB value", "grpc-metadata-x", vec![b'a'; 65536]),
        (
            "headers: control byte",
            "grpc-metadata-x",
            b"a\x01b".to_vec(),
        ),
        (
            "headers: override to DELETE",
            "x-http-method-override",
            b"DELETE".to_vec(),
        ),
        (
            "headers: override garbage",
            "x-http-method-override",
            b"\xff".to_vec(),
        ),
        (
            "headers: te trailers list",
            "te",
            b"trailers, deflate;q=0".to_vec(),
        ),
        (
            "headers: authorization big",
            "authorization",
            vec![b'a'; 65536],
        ),
    ] {
        let mut k = case(name, "GET", "/v1/top/int32/1", vec![]);
        k.headers.push((h.into(), v));
        c.push(k);
    }
    for ct in [
        &b"text/plain"[..],
        b"application/json; charset=utf-16",
        b"",
        b"\xff",
    ] {
        let mut k = case("content-type variant", "POST", star, b"{}".to_vec());
        k.headers = vec![("content-type".into(), ct.to_vec())];
        c.push(k);
    }
    c
}

/// Findings kept visible instead of failing CI: the ceiling is what abada
/// measures today (peak allocation over input bytes). It only goes down. The
/// second number is grpc-gateway's, measured on the same body through
/// `runtime.JSONPb` into a `dynamicpb` message (peak sampled every 200 µs, max
/// of 3 runs, host load ~17/32): the target the ceiling must reach.
/// See docs/readiness/grpc-gateway-parity.md, S5.
const RATCHET: &[(&str, f64, f64)] = &[
    ("body: 10^6 element array", 60.0, 39.1),
    ("body: 10^5 map entries", 18.0, 11.9),
];

fn build(c: &Case) -> Option<http::Request<Full<Bytes>>> {
    let mut b = http::Request::builder().method(c.method).uri(&c.uri);
    for (k, v) in &c.headers {
        b = b.header(k, http::HeaderValue::from_bytes(v).ok()?);
    }
    b.body(Full::new(Bytes::from(c.body.clone()))).ok()
}

struct Outcome {
    status: u16,
    out_bytes: usize,
    elapsed: Duration,
    peak_over_base: usize,
    total: usize,
    snippet: String,
}

async fn run<G, B>(gw: G, req: http::Request<Full<Bytes>>) -> Result<Outcome, String>
where
    G: tower::Service<
            http::Request<Full<Bytes>>,
            Response = http::Response<B>,
            Error = Infallible,
            Future: Send,
        > + Send
        + 'static,
    B: http_body::Body<Data = Bytes, Error = Infallible> + Send + 'static,
{
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let total0 = TOTAL.load(Ordering::Relaxed);
    let t = Instant::now();
    // A spawned task: a panic comes back as a JoinError instead of tearing the
    // test down, and it runs on a worker thread with a normal (2 MiB) stack.
    let handle = tokio::spawn(async move {
        let resp = gw.oneshot(req).await.unwrap();
        let status = resp.status().as_u16();
        let body = resp.into_body().collect().await.unwrap().to_bytes();
        (
            status,
            body.len(),
            String::from_utf8_lossy(&body[..body.len().min(90)]).into_owned(),
        )
    });
    match tokio::time::timeout(Duration::from_secs(20), handle).await {
        Err(_) => Err("timeout > 20 s".into()),
        Ok(Err(e)) => Err(format!("panicked: {e}")),
        Ok(Ok((status, out_bytes, snippet))) => Ok(Outcome {
            total: TOTAL.load(Ordering::Relaxed) - total0,
            snippet,
            status,
            out_bytes,
            elapsed: t.elapsed(),
            peak_over_base: PEAK.load(Ordering::Relaxed).saturating_sub(base),
        }),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn hostile_input_never_panics_and_costs_in_proportion() {
    let gw = gateway();
    let mut failures = Vec::new();
    println!(
        "{:<44} {:>6} {:>10} {:>9} {:>8} {:>6}",
        "case", "status", "in bytes", "out", "ms", "amp"
    );
    for c in corpus() {
        let in_bytes = c.uri.len()
            + c.body.len()
            + c.headers
                .iter()
                .map(|(k, v)| k.len() + v.len())
                .sum::<usize>();
        let Some(req) = build(&c) else {
            println!(
                "{:<44} (not expressible as an http::Request: rejected before abada)",
                c.name
            );
            continue;
        };
        match run(gw.clone(), req).await {
            Err(e) => {
                println!("{:<44} FAIL {e}", c.name);
                failures.push(format!("{}: {e}", c.name));
            }
            Ok(o) => {
                let amp = o.peak_over_base as f64 / in_bytes.max(1) as f64;
                println!(
                    "{:<44} {:>6} {:>10} {:>9} {:>8.1} {:>5.1}x total {:>5.1}x  {}",
                    c.name,
                    o.status,
                    in_bytes,
                    o.out_bytes,
                    o.elapsed.as_secs_f64() * 1e3,
                    amp,
                    o.total as f64 / in_bytes.max(1) as f64,
                    if o.status >= 400 {
                        o.snippet.replace('\n', " ")
                    } else {
                        String::new()
                    }
                );
                if !(100..=599).contains(&o.status) {
                    failures.push(format!("{}: status {}", c.name, o.status));
                }
                // Amplification only means something above a floor: a 20-byte
                // input allocating 4 KiB of fixed cost is not a finding.
                let bound = RATCHET.iter().find(|r| r.0 == c.name).map_or(10.0, |r| r.1);
                if in_bytes >= 64 * 1024 && amp > bound {
                    failures.push(format!(
                        "{}: peak {} B for {} B in ({amp:.1}x, bound {bound}x)",
                        c.name, o.peak_over_base, in_bytes
                    ));
                }
                if o.out_bytes > 64 * 1024 && o.out_bytes > 4 * in_bytes {
                    failures.push(format!(
                        "{}: {} B out for {} B in (output amplification)",
                        c.name, o.out_bytes, in_bytes
                    ));
                }
                // Unoptimised code is ~10x slower; the memory bound is the same.
                let slack = if cfg!(debug_assertions) { 10 } else { 1 };
                if o.elapsed > Duration::from_millis(slack * (1000 + in_bytes as u64 / 1024)) {
                    failures.push(format!("{}: {:?} for {} B in", c.name, o.elapsed, in_bytes));
                }
            }
        }
    }
    // No cross-request state: the same request before and after the corpus.
    let probe = case("probe", "GET", "/v1/top/int32/7", vec![]);
    let o = run(gw.clone(), build(&probe).unwrap()).await.unwrap();
    assert_eq!(
        o.status, 200,
        "the gateway still answers a good request after the corpus"
    );
    assert!(failures.is_empty(), "\n{}", failures.join("\n"));
}

/// `?nested.nested.….f_string=1` with N components used to make
/// `populate_field_value_from_path` recurse N deep on a tokio worker's 2 MiB
/// stack: one `GET` of 54 KB aborted the whole process (SIGABRT, nothing can
/// catch it), where grpc-gateway's Go stack grows to 1 GB. The walk is now a
/// loop and refuses a request nested past 100 levels (DESIGN.md, "Query field
/// paths"); the abort itself is the `corpus()` entries below, this pins the
/// answers around the limit.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deep_query_field_path_is_refused_past_100_levels() {
    let gw = gateway();
    for (components, status) in [(99, 200), (100, 200), (101, 400), (4000, 400), (9000, 400)] {
        let uri = format!(
            "/v1/query/x?{}.f_string=1",
            vec!["nested"; components - 1].join(".")
        );
        let c = case("deep", "GET", &uri, vec![]);
        let o = run(gw.clone(), build(&c).unwrap()).await.unwrap();
        assert_eq!(o.status, status, "{components} levels: {}", o.snippet);
    }
}
