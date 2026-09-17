//! Error responses, as grpc-gateway v2.27.3 writes them: `HTTPStatusFromCode`,
//! `DefaultHTTPErrorHandler` with the default marshaler, and
//! `DefaultRoutingErrorHandler`.
//!
//! What is produced is what reaches an HTTP/1.1 client from a Go `net/http`
//! server: header values are already sanitised the way `net/http` writes
//! them. Framing headers (`content-length`, `transfer-encoding`, `date`) are
//! the HTTP server's business and are not set here.
//!
//! Details are the one part that needs a type registry: protojson renders an
//! `Any` by resolving its type, and when it cannot, grpc-gateway answers `500`
//! with a fixed body. abada has no registry yet (the JSON transcoding decision
//! is open), so every detail except an empty `Any` takes that path — see
//! `docs/DESIGN.md`.

use http::{HeaderMap, HeaderName, HeaderValue, StatusCode, header};

use crate::path::RouteOutcome;

/// `google.protobuf.Any`, still encoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Any {
    pub type_url: String,
    pub value: Vec<u8>,
}

/// `google.rpc.Status`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Status {
    pub code: i32,
    pub message: String,
    pub details: Vec<Any>,
}

impl Status {
    pub fn new(code: i32, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: Vec::new(),
        }
    }
}

/// The metadata a gRPC call returned, as `runtime.ServerMetadata` holds it:
/// keys as gRPC gives them, values in order, binary values already decoded.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ServerMetadata {
    pub headers: Vec<(String, Vec<u8>)>,
    pub trailers: Vec<(String, Vec<u8>)>,
}

/// The response to write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorResponse {
    pub status: StatusCode,
    pub headers: HeaderMap,
    pub body: Vec<u8>,
    /// Sent after the body; non-empty only when the request accepts trailers.
    pub trailers: HeaderMap,
}

/// A routing failure, before any RPC is called.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutingError {
    NotFound,
    MethodNotAllowed,
    /// The request path does not start with `/`.
    BadRequest,
}

const CODE_OK: i32 = 0;
const CODE_UNKNOWN: i32 = 2;
const CODE_INVALID_ARGUMENT: i32 = 3;
const CODE_NOT_FOUND: i32 = 5;
const CODE_UNIMPLEMENTED: i32 = 12;
const CODE_UNAUTHENTICATED: i32 = 16;

const MARSHAL_FALLBACK: &[u8] = br#"{"code": 13, "message": "failed to marshal error message"}"#;

/// `runtime.HTTPStatusFromCode`. Codes outside `0..=16` are `500`.
pub fn http_status_from_code(code: i32) -> StatusCode {
    let status = match code {
        0 => 200,
        1 => 499,
        2 => 500,
        3 => 400,
        4 => 504,
        5 => 404,
        6 => 409,
        7 => 403,
        8 => 429,
        9 => 400,
        10 => 409,
        11 => 400,
        12 => 501,
        13 => 500,
        14 => 503,
        15 => 500,
        16 => 401,
        _ => 500,
    };
    StatusCode::from_u16(status).expect("valid status code")
}

/// Whether grpc-gateway forwards trailers for a request with this `TE`
/// header (its first value): the lower-cased value contains `trailers`.
pub fn accepts_trailers(te: Option<&HeaderValue>) -> bool {
    let Some(te) = te else { return false };
    go_to_lower(te.as_bytes())
        .windows(b"trailers".len())
        .any(|w| w == b"trailers")
}

/// `strings.ToLower`, as far as it can produce ASCII letters: besides ASCII,
/// only U+0130 and U+212A lower-case to one. Other bytes are kept.
fn go_to_lower(s: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i..].starts_with("\u{130}".as_bytes()) {
            out.push(b'i');
            i += 2;
        } else if s[i..].starts_with("\u{212a}".as_bytes()) {
            out.push(b'k');
            i += 3;
        } else {
            out.push(s[i].to_ascii_lowercase());
            i += 1;
        }
    }
    out
}

impl ErrorResponse {
    /// `DefaultHTTPErrorHandler` for a status returned by an RPC. `metadata`
    /// is `None` when the call produced none.
    pub fn from_status(
        status: &Status,
        metadata: Option<&ServerMetadata>,
        accepts_trailers: bool,
    ) -> Self {
        // A gRPC client turns an OK status into a nil error, and
        // `status.Convert(nil)` has no message and no details.
        let ok;
        let status = if status.code == CODE_OK {
            ok = Status::default();
            &ok
        } else {
            status
        };

        let mut headers = HeaderMap::new();
        headers.insert(
            header::CONTENT_TYPE,
            HeaderValue::from_static("application/json"),
        );
        if status.code == CODE_UNAUTHENTICATED {
            if let Some(v) = wire_value(status.message.as_bytes()) {
                headers.insert(header::WWW_AUTHENTICATE, v);
            }
        }

        let Some(body) = marshal_status(status) else {
            return Self {
                status: StatusCode::INTERNAL_SERVER_ERROR,
                headers,
                body: MARSHAL_FALLBACK.to_vec(),
                trailers: HeaderMap::new(),
            };
        };

        let mut trailers = HeaderMap::new();
        if let Some(md) = metadata {
            for (key, value) in &md.headers {
                append(&mut headers, "grpc-metadata-", key, value);
            }
            if accepts_trailers {
                let mut announced: Vec<String> = Vec::new();
                for (key, value) in &md.trailers {
                    let name = canonical_mime_header_key(&format!("Grpc-Trailer-{key}"));
                    if !announced.contains(&name) {
                        if let Ok(v) = HeaderValue::from_str(&name) {
                            headers.append(header::TRAILER, v);
                        }
                        announced.push(name);
                    }
                    append(&mut trailers, "grpc-trailer-", key, value);
                }
            }
        }

        Self {
            status: http_status_from_code(status.code),
            headers,
            body,
            trailers,
        }
    }

    /// `DefaultRoutingErrorHandler`.
    pub fn routing(error: RoutingError) -> Self {
        let status = match error {
            RoutingError::NotFound => Status::new(CODE_NOT_FOUND, "Not Found"),
            RoutingError::MethodNotAllowed => Status::new(CODE_UNIMPLEMENTED, "Method Not Allowed"),
            RoutingError::BadRequest => Status::new(CODE_INVALID_ARGUMENT, "Bad Request"),
        };
        Self::from_status(&status, None, false)
    }

    /// The `400` grpc-gateway writes for one malformed escape: an
    /// `HTTPStatusError` around a plain error, so the code is `Unknown`.
    pub fn malformed_escape(sequence: &[u8]) -> Self {
        let mut message = String::from("malformed path escape ");
        go_quote(sequence, &mut message);
        let mut response = Self::from_status(&Status::new(CODE_UNKNOWN, message), None, false);
        response.status = StatusCode::BAD_REQUEST;
        response
    }

    /// The response for a routing outcome, or `None` when a handler matched
    /// cleanly.
    ///
    /// After malformed escapes grpc-gateway keeps routing and appends what it
    /// finds to the `400` it already started: one error body per escape, then
    /// the `404`/`501` body. When a handler matched after all, its output
    /// follows the returned body in the same `400` response.
    pub fn for_route<T>(outcome: &RouteOutcome<'_, T>) -> Option<Self> {
        match outcome {
            RouteOutcome::Matched { .. } => None,
            RouteOutcome::NotFound => Some(Self::routing(RoutingError::NotFound)),
            RouteOutcome::MethodNotAllowed => Some(Self::routing(RoutingError::MethodNotAllowed)),
            RouteOutcome::BadRequest { escapes, then } => {
                let mut escapes = escapes.iter();
                let mut response = Self::malformed_escape(escapes.next()?);
                for seq in escapes {
                    response.body.extend(Self::malformed_escape(seq).body);
                }
                if let Some(tail) = Self::for_route(then) {
                    response.body.extend(tail.body);
                }
                Some(response)
            }
        }
    }
}

fn append(map: &mut HeaderMap, prefix: &str, key: &str, value: &[u8]) {
    let Ok(name) = HeaderName::from_bytes(format!("{prefix}{}", key.to_lowercase()).as_bytes())
    else {
        return;
    };
    if let Some(v) = wire_value(value) {
        map.append(name, v);
    }
}

/// A header value as `net/http` writes it: CR and LF become spaces, then
/// surrounding spaces and tabs go. `net/http` writes any other control byte
/// as it is; `http` cannot hold one, so that value is dropped — what Go's
/// HTTP/2 server does.
fn wire_value(value: &[u8]) -> Option<HeaderValue> {
    let replaced: Vec<u8> = value
        .iter()
        .map(|&b| if b == b'\r' || b == b'\n' { b' ' } else { b })
        .collect();
    let is_space = |b: &u8| matches!(b, b' ' | b'\t' | b'\r' | b'\n');
    let start = replaced
        .iter()
        .position(|b| !is_space(b))
        .unwrap_or(replaced.len());
    let end = replaced
        .iter()
        .rposition(|b| !is_space(b))
        .map_or(start, |i| i + 1);
    HeaderValue::from_bytes(&replaced[start..end]).ok()
}

/// `textproto.CanonicalMIMEHeaderKey`: a key with a byte that is not a token
/// character is returned as it is.
fn canonical_mime_header_key(key: &str) -> String {
    let token = |b: u8| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b);
    if !key.bytes().all(token) {
        return key.to_string();
    }
    let mut upper = true;
    key.bytes()
        .map(|b| {
            let c = if upper {
                b.to_ascii_uppercase()
            } else {
                b.to_ascii_lowercase()
            } as char;
            upper = b == b'-';
            c
        })
        .collect()
}

/// `google.rpc.Status` through protojson with `EmitUnpopulated`, without the
/// random spaces. `None` where protojson fails: any detail that needs a type
/// to be resolved.
fn marshal_status(status: &Status) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(48 + status.message.len());
    out.extend_from_slice(b"{\"code\":");
    out.extend_from_slice(status.code.to_string().as_bytes());
    out.extend_from_slice(b",\"message\":");
    json_string(&status.message, &mut out);
    out.extend_from_slice(b",\"details\":[");
    for (i, any) in status.details.iter().enumerate() {
        if i > 0 {
            out.push(b',');
        }
        if !any.type_url.is_empty() || !any.value.is_empty() {
            return None;
        }
        out.extend_from_slice(b"{}");
    }
    out.extend_from_slice(b"]}");
    Some(out)
}

/// protojson's string encoding: only `"`, `\` and control characters are
/// escaped; `<`, `>`, `&`, DEL and U+2028 are written as they are.
fn json_string(s: &str, out: &mut Vec<u8>) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    out.push(b'"');
    for &b in s.as_bytes() {
        match b {
            b'"' => out.extend_from_slice(b"\\\""),
            b'\\' => out.extend_from_slice(b"\\\\"),
            0x08 => out.extend_from_slice(b"\\b"),
            0x0c => out.extend_from_slice(b"\\f"),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            b if b < 0x20 => out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 15) as usize],
            ]),
            b => out.push(b),
        }
    }
    out.push(b'"');
}

/// Runes in U+0080..U+07FF that `strconv.IsPrint` rejects, as inclusive
/// ranges. Measured from Go by the conformance oracle.
const NOT_PRINTABLE_2BYTE: &[(u32, u32)] = &[
    (0x80, 0xa0),
    (0xad, 0xad),
    (0x378, 0x379),
    (0x380, 0x383),
    (0x38b, 0x38b),
    (0x38d, 0x38d),
    (0x3a2, 0x3a2),
    (0x530, 0x530),
    (0x557, 0x558),
    (0x58b, 0x58c),
    (0x590, 0x590),
    (0x5c8, 0x5cf),
    (0x5eb, 0x5ee),
    (0x5f5, 0x605),
    (0x61c, 0x61c),
    (0x6dd, 0x6dd),
    (0x70e, 0x70f),
    (0x74b, 0x74c),
    (0x7b2, 0x7bf),
    (0x7fb, 0x7fc),
];

#[doc(hidden)]
pub fn not_printable_2byte() -> &'static [(u32, u32)] {
    NOT_PRINTABLE_2BYTE
}

/// `strconv.Quote` for a malformed escape sequence: `%` and at most two more
/// bytes, so the only multi-byte runes it can meet are two bytes long. Longer
/// runes are written as they are.
fn go_quote(s: &[u8], out: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let hex_byte = |b: u8, out: &mut String| {
        out.push_str("\\x");
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 15) as usize] as char);
    };
    out.push('"');
    let mut rest = s;
    while let Some(&b) = rest.first() {
        if b < 0x80 {
            match b {
                b'"' => out.push_str("\\\""),
                b'\\' => out.push_str("\\\\"),
                0x07 => out.push_str("\\a"),
                0x08 => out.push_str("\\b"),
                0x0c => out.push_str("\\f"),
                b'\n' => out.push_str("\\n"),
                b'\r' => out.push_str("\\r"),
                b'\t' => out.push_str("\\t"),
                0x0b => out.push_str("\\v"),
                0x20..=0x7e => out.push(b as char),
                _ => hex_byte(b, out),
            }
            rest = &rest[1..];
            continue;
        }
        let len = match b {
            0xc0..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf7 => 4,
            _ => 1,
        };
        match rest
            .get(..len)
            .and_then(|r| std::str::from_utf8(r).ok())
            .and_then(|r| r.chars().next())
        {
            Some(c) => {
                let r = c as u32;
                if NOT_PRINTABLE_2BYTE
                    .iter()
                    .any(|&(lo, hi)| (lo..=hi).contains(&r))
                {
                    out.push_str(&format!("\\u{r:04x}"));
                } else {
                    out.push(c);
                }
                rest = &rest[len..];
            }
            None => {
                hex_byte(b, out);
                rest = &rest[1..];
            }
        }
    }
    out.push('"');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quote(s: &[u8]) -> String {
        let mut out = String::new();
        go_quote(s, &mut out);
        out
    }

    #[test]
    fn quote_escapes_like_strconv() {
        assert_eq!(quote(b"%zz"), r#""%zz""#);
        assert_eq!(quote(b"%\x01\x7f"), r#""%\x01\x7f""#);
        assert_eq!(quote("%\u{85}".as_bytes()), r#""%\u0085""#);
        assert_eq!(quote(b"%a\xc3"), r#""%a\xc3""#);
        assert_eq!(quote(b"%\xc0\x80"), r#""%\xc0\x80""#);
    }

    #[test]
    fn te_is_lowered_like_go() {
        let te = |s: &str| accepts_trailers(Some(&HeaderValue::from_str(s).unwrap()));
        assert!(te("deflate, TRAILERS"));
        assert!(!te("gzip"));
        assert!(!accepts_trailers(None));
    }
}
