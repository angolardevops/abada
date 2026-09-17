//! Cost of each JSON transcoding candidate on the bench cases of
//! `conformance/cases/json-delonix-node-v1.json`.
//! `cargo run --release -p abada-json-bench --bin bench`
//!
//! Every operation of a case runs once per round, round-robin, so load that
//! drifts during the run lands on all of them alike. Reported: the median, min
//! and max over the rounds, in ns per operation, and heap allocations per
//! operation (counted, not sampled).

use std::alloc::{GlobalAlloc, Layout, System};
use std::hint::black_box;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use abada::json::Marshaler;
use abada_json_bench::{Mode, abada_codec, pbjson, reflect, with_type};
use prost::Message;
use prost_reflect::MessageDescriptor;
use serde::Serialize;
use serde::de::DeserializeOwned;

struct Counting;
static ALLOCS: AtomicU64 = AtomicU64::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        ALLOCS.fetch_add(1, Ordering::Relaxed);
        unsafe { System.realloc(ptr, layout, new_size) }
    }
}

#[global_allocator]
static GLOBAL: Counting = Counting;

const ROUNDS: usize = 15;
/// Target time of one operation's slice of a round.
const SLICE_NS: u128 = 20_000_000;

struct Op<'a> {
    name: &'static str,
    run: Box<dyn FnMut() + 'a>,
}

fn allocs_per_op(f: &mut dyn FnMut()) -> f64 {
    let n = 64;
    let before = ALLOCS.load(Ordering::Relaxed);
    for _ in 0..n {
        f();
    }
    (ALLOCS.load(Ordering::Relaxed) - before) as f64 / n as f64
}

fn measure(case: &str, bytes: usize, mut ops: Vec<Op<'_>>) {
    // Calibrate each op to about SLICE_NS per round.
    let mut iters = Vec::new();
    for op in ops.iter_mut() {
        for _ in 0..16 {
            (op.run)();
        }
        let start = Instant::now();
        let mut n = 0u64;
        while start.elapsed().as_nanos() < SLICE_NS / 4 {
            (op.run)();
            n += 1;
        }
        iters.push((n * 4).max(1));
    }
    let mut samples = vec![Vec::with_capacity(ROUNDS); ops.len()];
    for _ in 0..ROUNDS {
        for (i, op) in ops.iter_mut().enumerate() {
            let start = Instant::now();
            for _ in 0..iters[i] {
                (op.run)();
            }
            samples[i].push(start.elapsed().as_nanos() as f64 / iters[i] as f64);
        }
    }
    for (i, op) in ops.iter_mut().enumerate() {
        let s = &mut samples[i];
        s.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let allocs = allocs_per_op(&mut op.run);
        println!(
            "{case:<32} {bytes:>6} B  {:<34} median {:>10.0} ns  min {:>10.0}  max {:>10.0}  allocs {:>7.1}",
            op.name,
            s[ROUNDS / 2],
            s[0],
            s[ROUNDS - 1],
            allocs
        );
    }
}

fn ops_for<'a, M>(desc: &'a MessageDescriptor, body: &'a [u8], abada: &'a Marshaler) -> Vec<Op<'a>>
where
    M: Message + Default + Clone + Serialize + DeserializeOwned + 'a,
{
    let dynamic = reflect::decode(desc, body).expect("prost-reflect decodes the bench body");
    // The typed message comes from prost-reflect through the binary form, so a
    // body pbjson cannot read (Any) still gets a response-path figure.
    let typed = M::decode(dynamic.encode_to_vec().as_slice()).unwrap();
    let pbjson_decodes = pbjson::decode::<M>(body).is_ok();
    if !pbjson_decodes {
        println!("{:<43}  (b) decode JSON->M: rejected, not timed", "");
    }
    let typed2 = typed.clone();
    let typed3 = typed.clone();
    let dynamic2 = dynamic.clone();
    let dynamic3 = dynamic.clone();
    let mut ops = vec![
        Op {
            name: "(abada) decode JSON->Dynamic",
            run: Box::new(move || {
                black_box(abada_codec::decode(abada, desc, black_box(body)).unwrap());
            }),
        },
        Op {
            name: "(abada) encode Dynamic->JSON",
            run: Box::new(move || {
                black_box(abada_codec::encode(abada, black_box(&dynamic3)).unwrap());
            }),
        },
        Op {
            name: "(a) decode JSON->Dynamic",
            run: Box::new(move || {
                black_box(reflect::decode(desc, black_box(body)).unwrap());
            }),
        },
        Op {
            name: "(a) decode JSON->Dynamic->M",
            run: Box::new(move || {
                black_box(reflect::decode_typed::<M>(desc, black_box(body)).unwrap());
            }),
        },
        Op {
            name: "(b) decode JSON->M",
            run: Box::new(move || {
                black_box(pbjson::decode::<M>(black_box(body)).unwrap());
            }),
        },
        Op {
            name: "(a) encode Dynamic->JSON",
            run: Box::new(move || {
                black_box(reflect::encode(black_box(&dynamic2), Mode::Emit).unwrap());
            }),
        },
        Op {
            name: "(a) encode M->Dynamic->JSON",
            run: Box::new(move || {
                black_box(reflect::encode_typed(desc, black_box(&typed2), Mode::Emit).unwrap());
            }),
        },
        Op {
            name: "(b) encode M->JSON",
            run: Box::new(move || {
                black_box(pbjson::encode(black_box(&typed3)).unwrap());
            }),
        },
        Op {
            name: "(ref) prost encode M->binary",
            run: Box::new(move || {
                black_box(black_box(&typed).encode_to_vec());
            }),
        },
    ];
    if !pbjson_decodes {
        ops.retain(|o| o.name != "(b) decode JSON->M");
    }
    ops
}

fn one_time() {
    let pool = reflect::pool();
    let desc = pool
        .get_message_by_name("delonix.node.v1.CreateContainerRequest")
        .unwrap();
    let body = br#"{"name":"web"}"#;
    let ops = vec![
        Op {
            name: "(a) DescriptorPool::decode",
            run: Box::new(|| {
                black_box(reflect::pool());
            }),
        },
        Op {
            name: "(a) get_message_by_name",
            run: Box::new(|| {
                black_box(
                    pool.get_message_by_name(black_box("delonix.node.v1.CreateContainerRequest")),
                );
            }),
        },
        Op {
            name: "(a) decode, descriptor cached",
            run: Box::new(|| {
                black_box(reflect::decode(&desc, black_box(body)).unwrap());
            }),
        },
        Op {
            name: "(a) decode, lookup per request",
            run: Box::new(|| {
                let d = pool
                    .get_message_by_name("delonix.node.v1.CreateContainerRequest")
                    .unwrap();
                black_box(reflect::decode(&d, black_box(body)).unwrap());
            }),
        },
    ];
    measure("descriptor", abada_json_bench::DESCRIPTOR_SET.len(), ops);
}

fn main() {
    #[derive(serde::Deserialize)]
    struct Case {
        name: String,
        message: String,
        input: Box<serde_json::value::RawValue>,
        #[serde(default)]
        repeat: usize,
        #[serde(default)]
        bench: bool,
    }
    #[derive(serde::Deserialize)]
    struct File {
        cases: Vec<Case>,
    }
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conformance/cases/json-delonix-node-v1.json"
    );
    let file: File = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    let only = std::env::args().nth(1);

    let abada = abada_codec::marshaler(Mode::Emit);
    if only.as_deref().is_none_or(|o| o == "descriptor") {
        one_time();
    }
    for case in file.cases.iter().filter(|c| c.bench) {
        if only.as_deref().is_some_and(|o| o != case.name) {
            continue;
        }
        let body = abada_json_bench::expand(case.input.get(), case.repeat);
        // The descriptor comes from the marshaler's registry, as generated
        // code would hand it over (abada skips the required-field walk for a
        // pool without required fields it knows); (a) uses the same one.
        let desc = abada
            .registry()
            .pool()
            .get_message_by_name(&case.message)
            .unwrap();
        let ops = with_type!(emit, case.message.as_str(), ops_for(&desc, &body, &abada)).unwrap();
        measure(&case.name, body.len(), ops);
    }
}
