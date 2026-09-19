//! Incoming HTTP headers to outgoing gRPC metadata, as `runtime.AnnotateContext`
//! builds it (`runtime/context.go`), using the default incoming header
//! matcher (`runtime/mux.go`'s `DefaultHeaderMatcher`). A custom header
//! matcher or metadata annotator (`ServeMuxOption`s in Go) is not in v0.1
//! scope.
//!
//! grpc-gateway checks the `Grpc-Timeout` header before anything else, and a
//! malformed one aborts the whole call; only after that does it walk the
//! other headers, where a malformed `-bin` value also aborts, but an invalid
//! metadata key or a non-ASCII text value is silently dropped. That order is
//! observable when more than one thing is wrong at once, so it is kept here.
//!
//! Proven against the real function by `tests/metadata.rs` over
//! `conformance/vectors/metadata.json`.

use std::time::Duration;

use http::HeaderMap;

/// A gRPC metadata key/value pair; the value is already decoded for a
/// `-bin` key.
pub type Pair = (String, Vec<u8>);

/// Why `AnnotateContext` refuses the request outright, instead of silently
/// dropping the one header responsible.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MetadataError {
    /// A `<name>-bin` header whose value is not valid base64. The detail
    /// after the header name is not grpc-gateway's own text: that comes
    /// from Go's `encoding/base64` decoder, which abada does not
    /// reimplement error message for error message — see `docs/DESIGN.md`.
    InvalidBinaryHeader { header: String },
    /// A `Grpc-Timeout` header that does not parse: shorter than two bytes,
    /// a non-decimal count, or a unit other than `H`, `M`, `S`, `m`, `u`,
    /// `n`.
    InvalidTimeout { value: String },
}

impl std::fmt::Display for MetadataError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidBinaryHeader { header } => write!(f, "invalid binary header {header}"),
            Self::InvalidTimeout { value } => write!(f, "invalid grpc-timeout: {value}"),
        }
    }
}

impl std::error::Error for MetadataError {}

/// What reaches the RPC: the metadata pairs, and the deadline a
/// `Grpc-Timeout` header asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Incoming {
    pub pairs: Vec<Pair>,
    pub timeout: Option<Duration>,
}

const METADATA_HEADER_PREFIX: &str = "grpc-metadata-";
const METADATA_PREFIX: &str = "grpcgateway-";
const METADATA_BINARY_SUFFIX: &str = "-bin";

/// `isPermanentHTTPHeader`, lower-cased: `http::HeaderMap` gives header
/// names already folded to lowercase, so there is no mixed-case
/// intermediate to preserve — nothing downstream reads one; the final gRPC
/// metadata key is lowercase either way.
const PERMANENT_HEADERS: &[&str] = &[
    "accept",
    "accept-charset",
    "accept-language",
    "accept-ranges",
    "authorization",
    "cache-control",
    "content-type",
    "cookie",
    "date",
    "expect",
    "from",
    "host",
    "if-match",
    "if-modified-since",
    "if-none-match",
    "if-schedule-tag-match",
    "if-unmodified-since",
    "max-forwards",
    "origin",
    "pragma",
    "referer",
    "user-agent",
    "via",
    "warning",
];

/// `DefaultHeaderMatcher`: the metadata key a header name becomes, or
/// `None` when it forwards to nothing.
fn matched_key(name: &str) -> Option<String> {
    if PERMANENT_HEADERS.contains(&name) {
        return Some(format!("{METADATA_PREFIX}{name}"));
    }
    name.strip_prefix(METADATA_HEADER_PREFIX)
        .map(str::to_string)
}

/// `isValidGRPCMetadataTextValue`: printable ASCII, byte for byte.
fn is_valid_text_value(v: &[u8]) -> bool {
    v.iter().all(|&b| (0x20..=0x7E).contains(&b))
}

/// `decodeBinHeader`: standard base64, padded when the string's length is a
/// multiple of 4, unpadded otherwise. Only whether decoding succeeds is
/// compared to Go, never the error text (see [`MetadataError::InvalidBinaryHeader`]).
fn decode_base64_std(s: &str) -> Result<Vec<u8>, ()> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a') as u32 + 26),
            b'0'..=b'9' => Some((c - b'0') as u32 + 52),
            b'+' => Some(62),
            b'/' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let padded = bytes.len() % 4 == 0;
    let data = if padded {
        let mut pad = 0usize;
        while pad < 2 && bytes[..bytes.len() - pad].ends_with(b"=") {
            pad += 1;
        }
        &bytes[..bytes.len() - pad]
    } else {
        if bytes.contains(&b'=') {
            return Err(());
        }
        bytes
    };
    if data.contains(&b'=') || data.len() % 4 == 1 {
        return Err(());
    }
    let mut out = Vec::with_capacity(data.len() / 4 * 3 + 2);
    let mut chunks = data.chunks(4).peekable();
    while let Some(chunk) = chunks.next() {
        if chunk.len() != 4 && chunks.peek().is_some() {
            return Err(()); // only the last group may be short
        }
        let mut v = [0u32; 4];
        for (i, &c) in chunk.iter().enumerate() {
            v[i] = value(c).ok_or(())?;
        }
        let packed = (v[0] << 18) | (v[1] << 12) | (v[2] << 6) | v[3];
        out.push((packed >> 16) as u8);
        if chunk.len() >= 3 {
            out.push((packed >> 8) as u8);
        }
        if chunk.len() == 4 {
            out.push(packed as u8);
        }
    }
    Ok(out)
}

/// A rough `net.SplitHostPort`: the host before the last `:`, minus IPv6
/// brackets. `RemoteAddr` is always a real socket address, never a bare
/// hostname, so this does not need Go's full grammar — see "Not validated"
/// in `docs/DESIGN.md`.
fn split_host_port(addr: &str) -> Option<&str> {
    if let Some(rest) = addr.strip_prefix('[') {
        let (host, tail) = rest.split_once(']')?;
        tail.starts_with(':').then_some(host)
    } else {
        let (host, port) = addr.rsplit_once(':')?;
        (!host.contains(':') && !port.is_empty()).then_some(host)
    }
}

/// `timeoutDecode`/`timeoutUnitToDuration`: the last byte is the unit
/// (`H M S m u n`, case-sensitive), the rest a base-10 `i64` (Go's
/// `strconv.ParseInt` accepts a leading `+`, which Rust's `i64::from_str`
/// does not, so it is stripped first).
///
/// A negative count is not rejected by Go, which hands
/// `context.WithTimeout` an already-past deadline; `Duration` cannot
/// represent that, so abada treats it as zero (immediate timeout) instead
/// — a written deviation nothing in the v0.1 scope exercises.
fn parse_grpc_timeout(s: &str) -> Result<Duration, ()> {
    let bytes = s.as_bytes();
    if bytes.len() < 2 {
        return Err(());
    }
    let unit = bytes[bytes.len() - 1];
    let nanos_per_unit: i64 = match unit {
        b'H' => 3_600_000_000_000,
        b'M' => 60_000_000_000,
        b'S' => 1_000_000_000,
        b'm' => 1_000_000,
        b'u' => 1_000,
        b'n' => 1,
        _ => return Err(()),
    };
    let digits = std::str::from_utf8(&bytes[..bytes.len() - 1]).map_err(|_| ())?;
    let count: i64 = digits
        .strip_prefix('+')
        .unwrap_or(digits)
        .parse()
        .map_err(|_| ())?;
    let nanos = count.checked_mul(nanos_per_unit).ok_or(())?;
    Ok(Duration::from_nanos(nanos.max(0) as u64))
}

/// `AnnotateContext`, less the context plumbing: incoming HTTP headers
/// (already lower-cased and multi-value, as [`http::HeaderMap`] holds
/// them), the request's `Host`, and its peer address (`ip:port`, when
/// known) become the metadata pairs and deadline a call carries.
///
/// Custom header matchers and metadata annotators are not in v0.1 scope, so
/// this always behaves as `runtime.NewServeMux()` with no options would.
pub fn incoming(
    headers: &HeaderMap,
    host: &str,
    remote_addr: Option<&str>,
) -> Result<Incoming, MetadataError> {
    let timeout = match headers
        .get("grpc-timeout")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
    {
        Some(v) => Some(
            parse_grpc_timeout(v).map_err(|_| MetadataError::InvalidTimeout { value: v.into() })?,
        ),
        None => None,
    };

    let mut pairs: Vec<Pair> = Vec::new();
    for (name, value) in headers.iter() {
        let name = name.as_str();
        if name == "x-forwarded-for" || name == "x-forwarded-host" {
            continue;
        }
        if name == "authorization" {
            pairs.push(("authorization".into(), value.as_bytes().to_vec()));
        }
        let Some(key) = matched_key(name) else {
            continue;
        };
        if name.ends_with(METADATA_BINARY_SUFFIX) {
            let text = value
                .to_str()
                .map_err(|_| MetadataError::InvalidBinaryHeader {
                    header: name.into(),
                })?;
            let decoded =
                decode_base64_std(text).map_err(|()| MetadataError::InvalidBinaryHeader {
                    header: name.into(),
                })?;
            pairs.push((key, decoded));
        } else if is_valid_text_value(value.as_bytes()) {
            pairs.push((key, value.as_bytes().to_vec()));
        }
        // else: not valid gRPC metadata text, silently dropped, as
        // grpc-gateway logs and continues rather than failing the request.
    }

    let forwarded_host = headers
        .get("x-forwarded-host")
        .and_then(|v| v.to_str().ok())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| (!host.is_empty()).then(|| host.to_string()));
    if let Some(h) = forwarded_host {
        pairs.push(("x-forwarded-host".into(), h.into_bytes()));
    }

    let mut xff: Vec<String> = headers
        .get_all("x-forwarded-for")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .map(str::to_string)
        .collect();
    if let Some(ip) = remote_addr.and_then(split_host_port) {
        xff.push(ip.to_string());
    }
    if !xff.is_empty() {
        pairs.push(("x-forwarded-for".into(), xff.join(", ").into_bytes()));
    }

    Ok(Incoming { pairs, timeout })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The exact multiplier `timeoutUnitToDuration` (`runtime/mux.go`) uses
    /// for each unit; the conformance oracle only records whether a
    /// deadline was set at all (a duration read back from `ctx.Deadline()`
    /// is wall-clock jitter, not something to commit), so the grammar
    /// itself is checked here against the numbers straight from Go's
    /// switch statement.
    #[test]
    fn timeout_units_match_go() {
        assert_eq!(
            parse_grpc_timeout("17H"),
            Ok(Duration::from_secs(17 * 3600))
        );
        assert_eq!(parse_grpc_timeout("19M"), Ok(Duration::from_secs(19 * 60)));
        assert_eq!(parse_grpc_timeout("23S"), Ok(Duration::from_secs(23)));
        assert_eq!(parse_grpc_timeout("1009m"), Ok(Duration::from_millis(1009)));
        assert_eq!(
            parse_grpc_timeout("1000003u"),
            Ok(Duration::from_micros(1_000_003))
        );
        assert_eq!(
            parse_grpc_timeout("100000007n"),
            Ok(Duration::from_nanos(100_000_007))
        );
    }

    #[test]
    fn timeout_rejects_what_go_rejects() {
        assert_eq!(parse_grpc_timeout("H"), Err(())); // too short
        assert_eq!(parse_grpc_timeout("5X"), Err(())); // unknown unit
        assert_eq!(parse_grpc_timeout("5s"), Err(())); // wrong case
        assert_eq!(parse_grpc_timeout(""), Err(()));
    }

    #[test]
    fn timeout_accepts_a_leading_plus_like_strconv_parseint() {
        assert_eq!(parse_grpc_timeout("+5S"), Ok(Duration::from_secs(5)));
    }

    #[test]
    fn base64_padded_and_unpadded_agree_with_go() {
        assert_eq!(decode_base64_std("AGhlbGxv"), Ok(b"\x00hello".to_vec()));
        assert_eq!(decode_base64_std("Alo"), Ok(vec![0x02, b'Z']));
        assert_eq!(decode_base64_std("AAE="), Ok(vec![0x00, 0x01]));
        assert_eq!(decode_base64_std("AQ=="), Ok(vec![0x01]));
        assert_eq!(decode_base64_std("not valid base64!!"), Err(()));
    }

    #[test]
    fn split_host_port_handles_the_shapes_remote_addr_takes() {
        assert_eq!(split_host_port("192.0.2.100:12345"), Some("192.0.2.100"));
        assert_eq!(split_host_port("[::1]:12345"), Some("::1"));
        assert_eq!(split_host_port("bad-addr-no-port"), None);
    }
}
