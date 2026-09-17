//! Picking the handler for a request — grpc-gateway's `ServeMux.ServeHTTP`
//! routing, without the HTTP.

use std::fmt;

use super::pattern::{MatchError, PathParams, Pattern, UnescapingMode};

/// The path of an origin-form request target (`/a/b?q`), split the way Go's
/// `net/url` splits it: the decoded `path`, and the `raw_path` only when the
/// target was not the default encoding of that path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestPath {
    path: Vec<u8>,
    raw_path: Option<Vec<u8>>,
}

/// A request target grpc-gateway never gets to route.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidTarget(String);

impl fmt::Display for InvalidTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid request target: {}", self.0)
    }
}

impl std::error::Error for InvalidTarget {}

/// Bytes `net/url` leaves as they are in a path. Measured from Go by the
/// conformance oracle, not transcribed from the RFC.
fn path_keeps(b: u8) -> bool {
    b.is_ascii_alphanumeric()
        || matches!(
            b,
            b'$' | b'&'
                | b'+'
                | b','
                | b'-'
                | b'.'
                | b'/'
                | b':'
                | b';'
                | b'='
                | b'@'
                | b'_'
                | b'~'
        )
}

#[doc(hidden)]
pub fn path_kept_bytes() -> Vec<u8> {
    (0..=255).filter(|&b| path_keeps(b)).collect()
}

impl RequestPath {
    /// Parses an origin-form target. The query, if any, is ignored.
    pub fn parse(target: &str) -> Result<Self, InvalidTarget> {
        if target.bytes().any(|b| b < b' ' || b == 0x7f) {
            return Err(InvalidTarget("control character".into()));
        }
        if !target.starts_with('/') {
            return Err(InvalidTarget("not an origin-form path".into()));
        }
        let rest = if target.ends_with('?') && target.matches('?').count() == 1 {
            &target[..target.len() - 1]
        } else {
            target.split_once('?').map_or(target, |(p, _)| p)
        };
        let raw = rest.as_bytes();
        let path = percent_decode(raw)?;
        let default_encoding = percent_encode(&path);
        Ok(Self {
            raw_path: (default_encoding != raw).then(|| raw.to_vec()),
            path,
        })
    }

    pub fn path(&self) -> &[u8] {
        &self.path
    }

    pub fn raw_path(&self) -> Option<&[u8]> {
        self.raw_path.as_deref()
    }
}

fn percent_decode(s: &[u8]) -> Result<Vec<u8>, InvalidTarget> {
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let Some(pair) = s
                .get(i + 1..i + 3)
                .filter(|p| p.iter().all(u8::is_ascii_hexdigit))
            else {
                return Err(InvalidTarget("malformed percent-encoding".into()));
            };
            out.push(u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap());
            i += 3;
        } else {
            out.push(s[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn percent_encode(s: &[u8]) -> Vec<u8> {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = Vec::with_capacity(s.len());
    for &b in s {
        if path_keeps(b) {
            out.push(b);
        } else {
            out.extend_from_slice(&[b'%', HEX[(b >> 4) as usize], HEX[(b & 15) as usize]]);
        }
    }
    out
}

/// What routing decided.
#[derive(Debug, PartialEq, Eq)]
pub enum RouteOutcome<'a, T> {
    Matched {
        handler: &'a T,
        params: PathParams,
    },
    NotFound,
    /// The path matches under another method. grpc-gateway's default error
    /// handler answers this with `501 Unimplemented`.
    MethodNotAllowed,
    /// A candidate held a malformed escape.
    BadRequest(String),
}

/// Handlers by method, tried most-recently-registered first.
#[derive(Debug, Clone)]
pub struct Router<T> {
    mode: UnescapingMode,
    methods: Vec<(String, Vec<(Pattern, T)>)>,
}

impl<T> Default for Router<T> {
    fn default() -> Self {
        Self::new(UnescapingMode::default())
    }
}

impl<T> Router<T> {
    pub fn new(mode: UnescapingMode) -> Self {
        Self {
            mode,
            methods: Vec::new(),
        }
    }

    /// Registers a handler. It takes precedence over every earlier one for the
    /// same method, as in grpc-gateway.
    pub fn add(&mut self, method: &str, pattern: Pattern, handler: T) {
        match self.methods.iter_mut().find(|(m, _)| m == method) {
            Some((_, handlers)) => handlers.push((pattern, handler)),
            None => self
                .methods
                .push((method.to_string(), vec![(pattern, handler)])),
        }
    }

    pub fn route(&self, method: &str, request: &RequestPath) -> RouteOutcome<'_, T> {
        let path = match (self.mode, request.raw_path()) {
            (UnescapingMode::Legacy, _) | (_, None) => request.path(),
            (_, Some(raw)) => raw,
        };
        let components = split(&path[1..], self.mode == UnescapingMode::AllCharacters);
        let last = components
            .last()
            .expect("split yields at least one component");

        // grpc-gateway writes a 400 for a malformed escape and keeps looking;
        // whatever it writes afterwards cannot change the status any more.
        let mut bad_request: Option<String> = None;

        for (m, handlers) in &self.methods {
            if m != method {
                continue;
            }
            for (pattern, handler) in handlers.iter().rev() {
                let verb_at = verb_index(last, pattern.verb());
                if verb_at == Some(0) {
                    return finish(RouteOutcome::NotFound, bad_request);
                }
                let (comps, verb) = with_verb_split(&components, verb_at);
                match pattern.match_components(&comps, &verb, self.mode) {
                    Ok(params) => {
                        return finish(RouteOutcome::Matched { handler, params }, bad_request);
                    }
                    Err(MatchError::MalformedEscape(seq)) => {
                        bad_request.get_or_insert(seq);
                    }
                    Err(MatchError::NoMatch) => {}
                }
            }
        }

        // Another method's pattern matching means the method is wrong. Unlike
        // the first pass, a bare `:verb` is not a routing error here.
        for (m, handlers) in &self.methods {
            if m == method {
                continue;
            }
            for (pattern, _) in handlers.iter().rev() {
                let verb_at = verb_index(last, pattern.verb()).filter(|&i| i > 0);
                let (comps, verb) = with_verb_split(&components, verb_at);
                match pattern.match_components(&comps, &verb, self.mode) {
                    Ok(_) => return finish(RouteOutcome::MethodNotAllowed, bad_request),
                    Err(MatchError::MalformedEscape(seq)) => {
                        bad_request.get_or_insert(seq);
                    }
                    Err(MatchError::NoMatch) => {}
                }
            }
        }
        finish(RouteOutcome::NotFound, bad_request)
    }
}

fn finish<T>(outcome: RouteOutcome<'_, T>, bad_request: Option<String>) -> RouteOutcome<'_, T> {
    match bad_request {
        Some(seq) => RouteOutcome::BadRequest(seq),
        None => outcome,
    }
}

fn split(path: &[u8], on_encoded_slash: bool) -> Vec<Vec<u8>> {
    let mut out = Vec::new();
    let mut current = Vec::new();
    let mut i = 0;
    while i < path.len() {
        if path[i] == b'/' {
            out.push(std::mem::take(&mut current));
            i += 1;
        } else if on_encoded_slash && path[i..].starts_with(b"%2F") {
            // Case-sensitive, like grpc-gateway's `(/|%2F)`.
            out.push(std::mem::take(&mut current));
            i += 3;
        } else {
            current.push(path[i]);
            i += 1;
        }
    }
    out.push(current);
    out
}

fn verb_index(last: &[u8], verb: &str) -> Option<usize> {
    if verb.is_empty() {
        return None;
    }
    let suffix = [b":".as_slice(), verb.as_bytes()].concat();
    last.ends_with(&suffix).then(|| last.len() - suffix.len())
}

/// Splits the verb off the last component when `verb_at` points at its colon.
fn with_verb_split(components: &[Vec<u8>], verb_at: Option<usize>) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut comps = components.to_vec();
    match verb_at {
        Some(i) if i > 0 => {
            let last = comps.last_mut().expect("at least one component");
            let verb = last.split_off(i + 1);
            last.pop();
            (comps, verb)
        }
        _ => (comps, Vec::new()),
    }
}
