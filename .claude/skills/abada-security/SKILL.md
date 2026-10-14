---
name: abada-security
description: How abada proves it is at least as safe as grpc-gateway — a threat model for an HTTP/JSON front to gRPC, a hostile corpus per class (path, escapes, query, body, JSON, headers/metadata, method override, streaming), differential testing against grpc-gateway v2.27.3 where it has an answer and property tests (no panic, bounded memory and time) where it does not, and the "security parity gate" with its verdicts. Use when adding or changing anything that parses bytes from a request or a user descriptor, before a release, when a security-relevant finding is reported, or when someone asks "is abada as safe as grpc-gateway?". Not for correctness vectors (`abada-conformance`) nor for throughput and latency (`abada-performance`).
---

# Security: at least as safe as grpc-gateway, and measured

abada sits between the internet and a gRPC service. Everything it parses was
written by an attacker until proven otherwise. "As safe as grpc-gateway" is a
claim with two parts, and each has its own kind of proof:

1. **Where grpc-gateway has an answer, abada gives the same one.** A request
   Go rejects with 400 and abada accepts is a finding, even if abada's answer
   looks "more lenient and harmless": the parser difference is the bug class
   (smuggling, filter bypass, cache poisoning). Proof: a vector, as in
   [`abada-conformance`](../abada-conformance/SKILL.md).
2. **Where grpc-gateway has NO answer, abada must not be worse than Go's
   memory-safe floor and must be better where it costs nothing.** Go cannot
   read out of bounds and its panics are recovered per request by `net/http`;
   a Rust panic on a request path is at best a dropped connection and at worst
   a poisoned lock. Proof: a property test, not a vector.

## 0. Threat model (write it into the PR when it moves)

| Asset | Attacker | Goal |
|---|---|---|
| The gRPC service behind the gateway | anonymous HTTP client | reach an RPC or a field the annotation did not expose; smuggle a value the gateway's own filter should have stopped |
| Availability of the process | anonymous HTTP client, cheap requests | exhaust memory, CPU or stack with a small input (amplification) |
| Identity and metadata | any client | forge `Grpc-Metadata-*`, `X-Forwarded-For`, `Authorization`, or inject into response headers |
| The user's descriptor | whoever supplies `.proto`/`.binpb` | crash or hang `abada-codegen`/`protoc-gen-abada` at build time |

Out of scope, said once so nobody assumes it: TLS, authentication and rate
limiting are the mounter's job (abada is a `tower::Service`, property 4);
authorisation is the service's.

## 1. The hostile corpus — one class at a time

Cases live in `conformance/cases/security-<class>.json` (inputs only, like all
cases) and are answered by the oracle. Minimum content per class; add what the
change you are making could break:

| Class | Must include |
|---|---|
| **Path and escapes** | `%2f`, `%2F` in a `*` and in a `**`; `%00`; `%` truncated (`%`, `%4`, `%zz`); double-encoding (`%252f`); `..`, `.`, `//`, trailing `/`; `;` params; non-ASCII and overlong UTF-8 (`%c0%af`); all four unescaping modes; `:verb` with and without a matching binding; a 64 KiB path |
| **Query** | duplicate keys; 10 000 repeated values; `a[]=`, `a.b.c.d…` 1 000 levels deep; a field named like an internal one (`@type`, `__proto__`); `=` and `&` inside values; `%` malformed; huge `updateMask`; a key that sets a field the binding already took from the path or the body (**overwrite order is a security property**) |
| **Body and content type** | body with `GET`/`DELETE`; wrong or missing `Content-Type`; `charset` variants; `Content-Length` lying (needs a raw-socket test, see §3); chunked with a trailer; empty body where a message is required; a body larger than the limit the mounter configured |
| **JSON** | nesting 99/100/101 and 10 000; a 1 MiB string; a 10⁶-element array; `1e999999999`, `-0`, NaN/Infinity spelled every way; duplicate keys; unknown fields and enum names; BOM; lone surrogates; invalid UTF-8; `Any` with an unregistered `@type`; `Any` inside `Any` 100 deep; `Duration`/`Timestamp` out of range; `FieldMask` with 10⁵ paths; `Struct` with numeric-string keys; `bytes` with mixed base64 alphabets and padding |
| **Headers and metadata** | `Grpc-Metadata-` with control bytes, CR/LF, empty names, duplicates, binary `-bin` suffix with bad base64; 1 000 headers; a 64 KiB value; `X-HTTP-Method-Override` with a method the route does not have, lower-case, with a list; `Host` with userinfo; `TE`/`Connection` smuggling combinations |
| **Errors** | a gRPC status whose message contains `</script>`, NUL, invalid UTF-8 or 10 MiB; `details` of a type the registry lacks; a status code outside `0..=16` |
| **Streaming** (when it exists) | a client that stops reading; a server that never ends; an error after the first message; a frame larger than the limit |
| **Descriptors** (build time) | recursive messages, `google.api.http` with a malformed template, 10⁴ bindings, a `body` naming a field that does not exist, `additional_bindings` nested |

The **ugly** cases are the point. A corpus of well-formed requests is a
conformance suite, not a security one.

## 2. Two kinds of proof

**Differential** — the request is in the corpus, the oracle answers it through
grpc-gateway's real code, the Rust test replays and compares every recorded
field. Same procedure and same rules as `abada-conformance` (never edit a
vector; break the code on purpose and watch the test fail). A difference is
either fixed or written in `docs/DESIGN.md` under a named deviation **that says
which side is safer and why** — "we reject what Go accepts" is a legitimate
deviation for a security reason; "we accept what Go rejects" almost never is.

**Property** — for input Go has no answer for, or an input too big to keep as a
vector. Run the SAME corpus through the real `Gateway` with the cheapest fake
service and assert, for every input:

1. **No panic.** The test runs each input in `catch_unwind` (or a spawned task
   whose `JoinHandle` is checked) and fails naming the input.
2. **A well-formed HTTP answer.** Status in 100..=599, headers that
   `http::HeaderMap` accepted, body valid UTF-8 JSON when the content type says
   JSON.
3. **Bounded resources.** Peak allocation and wall time stay under a bound
   *proportional to the input size*, and the bound is written in the test. An
   amplification factor above 10× (output or peak memory over input bytes) is a
   finding. Measure with the counting allocator in the test file, not by eye.
4. **No cross-request state.** The same request twice, with a hostile one
   between, gives the same answer.

Randomised inputs use a **fixed seed committed in the test** (as `path.json`
does); a failure prints the seed and the input, shrunk. `cargo fuzz` targets
(`fuzz/`, not part of the workspace, `publish = false`) are welcome for
`abada::path`, `abada::json::decode` and `abada::request` — run them for at
least 10 minutes per target before a release and record the number of
executions, not "ran the fuzzer".

## 3. What the in-process test cannot see

`tower::Service` tests never touch a socket. Listing what they miss is part of
the report:

- HTTP/1 request smuggling and header-size limits belong to hyper; abada is only
  responsible for the bytes it receives afterwards — but `Content-Length` vs
  body length disagreement, slowloris and connection limits need a **raw-socket
  test** against `abada` mounted in hyper, differential against a `net/http`
  server running the oracle's mux. Until that exists, say "not validated".
- Body size: `Gateway` collects the whole body before decoding. grpc-gateway
  does too and Go has no default limit either, so this is **parity, not safety**:
  the report states the limit the mounter must add (`http_body_util::Limited`)
  and the docs and generated `README` example must show it.

## 4. The security parity gate

Six rows. `docs/readiness/grpc-gateway-parity.md` holds the current verdict with
its evidence; a row moves only with its proof filled in.

| # | Row | PASS means |
|---|---|---|
| S1 | Path and routing | every `security-path` case: same outcome as grpc-gateway in the four unescaping modes |
| S2 | Request population | every `security-query`/`-body` case: same message or same error; overwrite order identical |
| S3 | JSON | corpus answered like protojson **or** rejected with a written safer deviation; no panic or stack use scaling with input on the 10 000-deep and 10⁶-element cases |
| S4 | Headers and metadata | no header injection either direction; every dropped or rejected header is a named deviation |
| S5 | Resource bounds | property test §2.3 green with the bounds written down; body limit documented |
| S6 | Supply chain | `cargo deny check` (advisories, licences, bans, sources) and `cargo audit` clean or every exception justified in `deny.toml`; `unsafe` count in `abada*` is zero or each block has a `// SAFETY:` and a test; `Cargo.lock` committed for binaries |

Verdicts, per row and overall — never "almost":

- **PASS** — the proof exists and ran on the current commit.
- **PASS WITH DEVIATION** — deviations named, each on the safer side.
- **FAIL** — a finding is open. Say which input and what it does.
- **NOT VALIDATED** — nothing ran. This is not a soft PASS; the overall verdict
  cannot be PASS while any row is NOT VALIDATED.

## 5. Reporting a finding

Each finding: the **exact input** (bytes, not a description), what abada did,
what grpc-gateway did (vector or "no answer"), the impact in one sentence
("a client can allocate 4 GiB with a 12-byte body"), and the fix or the
deviation. Severity follows impact on the assets in §0, not how clever the
input is. A confirmed remotely triggerable crash or unbounded amplification is
fixed before the PR merges; it is not a follow-up.

Do not publish exploit detail for a finding in a released version before the
fix ships; follow `SECURITY.md` if the repo has one, otherwise ask the
maintainer how to disclose.
