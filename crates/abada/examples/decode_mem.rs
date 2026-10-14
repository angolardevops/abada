//! Where the memory of a JSON body goes (`abada-security` S5).
//!
//! The parity report measures peak allocation over input bytes through the
//! whole `Gateway`; that number says a repeated field costs 1.4–1.9× what
//! grpc-gateway costs, but not *which* allocation. This splits the same bodies
//! into three numbers that do say it:
//!
//! - **resident**: what the decoded `DynamicMessage` costs once decoding has
//!   finished and every temporary is dropped. This is the floor — it is
//!   `prost_reflect::Value`'s representation, not a buffer abada chose.
//! - **peak**: the high-water mark during `decode`. `peak − resident` is what
//!   decoding holds *on top of* the result: the temporaries.
//! - **total** and **allocations**: every byte and every call handed to the
//!   allocator, which is what a growth-doubled `Vec` shows up in.
//!
//! ```text
//! cargo run -p abada --release --example decode_mem
//! ```
//!
//! Not a benchmark: one run, no statistics. It locates an allocation; a claim
//! about a ratio against grpc-gateway comes from `tests/security.rs` and the
//! report.

use std::alloc::{GlobalAlloc, Layout, System};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Instant;

use abada::json::{Marshaler, TypeRegistry};
use prost_reflect::{DescriptorPool, DynamicMessage};

struct Counting;
static LIVE: AtomicUsize = AtomicUsize::new(0);
static PEAK: AtomicUsize = AtomicUsize::new(0);
static TOTAL: AtomicUsize = AtomicUsize::new(0);
static CALLS: AtomicUsize = AtomicUsize::new(0);

unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        // SAFETY: forwarded unchanged to the system allocator.
        let p = unsafe { System.alloc(l) };
        if !p.is_null() {
            CALLS.fetch_add(1, Ordering::Relaxed);
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
            CALLS.fetch_add(1, Ordering::Relaxed);
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

struct Measure {
    peak: usize,
    resident: usize,
    total: usize,
    calls: usize,
    millis: f64,
}

/// Decodes `body` and reports what it cost. The decoded message is kept alive
/// across the "resident" read and dropped after, so `resident` is the message
/// and `peak - resident` is what decoding held on top of it.
fn measure(marshaler: &Marshaler, desc: &prost_reflect::MessageDescriptor, body: &[u8]) -> Measure {
    let base = LIVE.load(Ordering::Relaxed);
    PEAK.store(base, Ordering::Relaxed);
    let total0 = TOTAL.load(Ordering::Relaxed);
    let calls0 = CALLS.load(Ordering::Relaxed);
    let t = Instant::now();
    let mut msg = DynamicMessage::new(desc.clone());
    let decoded = marshaler.decode_into(&mut msg, body);
    let millis = t.elapsed().as_secs_f64() * 1e3;
    let peak = PEAK.load(Ordering::Relaxed).saturating_sub(base);
    let resident = LIVE.load(Ordering::Relaxed).saturating_sub(base);
    let m = Measure {
        peak,
        resident,
        total: TOTAL.load(Ordering::Relaxed) - total0,
        calls: CALLS.load(Ordering::Relaxed) - calls0,
        millis,
    };
    if let Err(e) = decoded {
        eprintln!("  (decode failed: {e})");
    }
    drop(msg);
    m
}

fn bodies() -> Vec<(String, Vec<u8>)> {
    vec![
        (
            "rInt32, 10^6 elements".into(),
            format!("{{\"rInt32\":[{}0]}}", "0,".repeat(1_000_000)).into_bytes(),
        ),
        (
            "rInt32, 10^5 elements".into(),
            format!("{{\"rInt32\":[{}0]}}", "0,".repeat(100_000)).into_bytes(),
        ),
        (
            "rString, 10^5 elements".into(),
            format!("{{\"rString\":[{}\"z\"]}}", "\"abcdefgh\",".repeat(100_000)).into_bytes(),
        ),
        (
            "mStr, 10^5 entries".into(),
            format!(
                "{{\"mStr\":{{{}\"z\":\"\"}}}}",
                (0..100_000)
                    .map(|i| format!("\"k{i}\":\"v\","))
                    .collect::<String>()
            )
            .into_bytes(),
        ),
        (
            "items, 10^4 messages".into(),
            format!("{{\"items\":[{}{{}}]}}", "{\"fInt32\":1},".repeat(10_000)).into_bytes(),
        ),
        (
            "fString, 1 MiB".into(),
            format!("{{\"fString\":\"{}\"}}", "a".repeat(1 << 20)).into_bytes(),
        ),
    ]
}

fn main() {
    let pool = pool();
    let desc = pool
        .get_message_by_name("abada.conformance.request.v1.Everything")
        .unwrap();
    let marshaler = Marshaler::new(TypeRegistry::new(pool).unwrap());

    println!(
        "prost_reflect::Value = {} B, MapKey = {} B, DynamicMessage = {} B",
        size_of::<prost_reflect::Value>(),
        size_of::<prost_reflect::MapKey>(),
        size_of::<DynamicMessage>(),
    );
    println!(
        "\n{:<24} {:>10} {:>12} {:>12} {:>12} {:>10} {:>9}",
        "body", "in", "peak", "resident", "temporaries", "total", "allocs"
    );
    for (name, body) in bodies() {
        // Warm: the first decode of a shape also builds descriptor caches.
        let _ = measure(&marshaler, &desc, &body);
        let m = measure(&marshaler, &desc, &body);
        println!(
            "{:<24} {:>10} {:>9} {:>4.1}x {:>9} {:>4.1}x {:>12} {:>10} {:>9} {:>7.1} ms",
            name,
            body.len(),
            m.peak,
            m.peak as f64 / body.len() as f64,
            m.resident,
            m.resident as f64 / body.len() as f64,
            m.peak.saturating_sub(m.resident),
            m.total,
            m.calls,
            m.millis,
        );
    }
}
