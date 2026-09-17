//! Matching request path components against a compiled template — grpc-gateway's
//! `runtime.Pattern`.

use std::collections::BTreeMap;
use std::fmt;

use super::template::{OpCode, ParseError, PathTemplate};

/// How percent-escapes in captured path values are decoded. The variants and
/// the default are grpc-gateway's, so a gateway moved from Go keeps its routes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnescapingMode {
    /// The whole path is decoded before routing, so `%2F` splits components.
    /// grpc-gateway v2's default.
    #[default]
    Legacy,
    /// Decode everything except RFC 6570 reserved characters in `**` captures.
    AllExceptReserved,
    /// Decode everything except `/` in `**` captures.
    AllExceptSlash,
    /// Decode everything; `%2F` also splits components.
    AllCharacters,
}

/// A template grpc-gateway parses but refuses as a pattern.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PatternError {
    Parse(ParseError),
    /// More than one `**`: the match would be ambiguous.
    DeepWildcardTwice {
        template: String,
    },
}

impl fmt::Display for PatternError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(e) => e.fmt(f),
            Self::DeepWildcardTwice { template } => {
                write!(f, "invalid path template {template:?}: ** appears twice")
            }
        }
    }
}

impl std::error::Error for PatternError {}

impl From<ParseError> for PatternError {
    fn from(e: ParseError) -> Self {
        Self::Parse(e)
    }
}

/// Why a path did not bind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    NoMatch,
    /// A `%` not followed by two hex digits; grpc-gateway answers 400.
    MalformedEscape(Vec<u8>),
}

/// A compiled template, ready to match.
#[derive(Debug, Clone)]
pub struct Pattern {
    template: PathTemplate,
    ops: Vec<(OpCode, usize)>,
    pool: Vec<Vec<u8>>,
    vars: Vec<String>,
    tail_len: usize,
}

/// Values bound by a match, keyed by field path. Values are bytes: a decoded
/// escape need not be UTF-8, and dropping or replacing it here would change
/// the request without telling anyone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PathParams(BTreeMap<String, Vec<u8>>);

impl PathParams {
    pub fn get(&self, field_path: &str) -> Option<&[u8]> {
        self.0.get(field_path).map(Vec::as_slice)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.0.iter().map(|(k, v)| (k.as_str(), v.as_slice()))
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

impl Pattern {
    pub fn new(template: &str) -> Result<Self, PatternError> {
        Self::from_template(PathTemplate::parse(template)?)
    }

    pub fn from_template(template: PathTemplate) -> Result<Self, PatternError> {
        let compiled = template.compile();
        let mut ops = Vec::with_capacity(compiled.op_codes.len() / 2);
        let mut vars = Vec::new();
        let mut tail_len = 0;
        let mut deep_seen = false;
        for pair in compiled.op_codes.chunks_exact(2) {
            let code = OpCode::from_i64(pair[0]).expect("compile emits known opcodes");
            let mut operand = pair[1] as usize;
            match code {
                OpCode::Push | OpCode::LitPush => {
                    if deep_seen {
                        tail_len += 1;
                    }
                }
                OpCode::PushM => {
                    if deep_seen {
                        return Err(PatternError::DeepWildcardTwice {
                            template: template.as_str().to_string(),
                        });
                    }
                    deep_seen = true;
                }
                OpCode::ConcatN => {}
                OpCode::Capture => {
                    vars.push(compiled.pool[operand].clone());
                    operand = vars.len() - 1;
                }
            }
            ops.push((code, operand));
        }
        Ok(Self {
            pool: compiled.pool.into_iter().map(String::into_bytes).collect(),
            template,
            ops,
            vars,
            tail_len,
        })
    }

    pub fn template(&self) -> &PathTemplate {
        &self.template
    }

    pub fn verb(&self) -> &str {
        self.template.verb()
    }

    /// Matches already-split components (no leading `/`) and the verb the
    /// caller split off the last one. This is `runtime.Pattern.MatchAndEscape`.
    pub fn match_components(
        &self,
        components: &[Vec<u8>],
        verb: &[u8],
        mode: UnescapingMode,
    ) -> Result<PathParams, MatchError> {
        let pattern_verb = self.verb().as_bytes();
        let joined;
        let components = if pattern_verb != verb {
            if !pattern_verb.is_empty() {
                return Err(MatchError::NoMatch);
            }
            // The request carried a verb the pattern does not know: it is
            // part of the last component, as grpc-gateway reads it.
            let mut owned = components.to_vec();
            match owned.last_mut() {
                Some(last) => {
                    last.push(b':');
                    last.extend_from_slice(verb);
                }
                None => owned.push([b":".as_slice(), verb].concat()),
            }
            joined = owned;
            &joined[..]
        } else {
            components
        };

        let mut pos = 0;
        let mut stack: Vec<Vec<u8>> = Vec::new();
        let mut captured: Vec<Vec<u8>> = vec![Vec::new(); self.vars.len()];
        let len = components.len();
        for &(code, operand) in &self.ops {
            match code {
                OpCode::Push | OpCode::LitPush => {
                    let Some(c) = components.get(pos) else {
                        return Err(MatchError::NoMatch);
                    };
                    if code == OpCode::LitPush {
                        if *c != self.pool[operand] {
                            return Err(MatchError::NoMatch);
                        }
                        stack.push(c.clone());
                    } else {
                        stack.push(unescape(c, mode, false)?);
                    }
                    pos += 1;
                }
                OpCode::PushM => {
                    if len < pos + self.tail_len {
                        return Err(MatchError::NoMatch);
                    }
                    let end = len - self.tail_len;
                    let value = components[pos..end].join(&b'/');
                    stack.push(unescape(&value, mode, true)?);
                    pos = end;
                }
                OpCode::ConcatN => {
                    let at = stack.len() - operand;
                    let tail = stack.split_off(at);
                    stack.push(tail.join(&b'/'));
                }
                OpCode::Capture => {
                    captured[operand] = stack.pop().expect("capture follows a push");
                }
            }
        }
        if pos < len {
            return Err(MatchError::NoMatch);
        }
        // A field bound twice keeps the last value, like the Go map it mirrors.
        let mut params = BTreeMap::new();
        for (name, value) in self.vars.iter().zip(captured) {
            params.insert(name.clone(), value);
        }
        Ok(PathParams(params))
    }
}

fn is_rfc6570_reserved(c: u8) -> bool {
    matches!(
        c,
        b'!' | b'#'
            | b'$'
            | b'&'
            | b'\''
            | b'('
            | b')'
            | b'*'
            | b'+'
            | b','
            | b'/'
            | b':'
            | b';'
            | b'='
            | b'?'
            | b'@'
            | b'['
            | b']'
    )
}

fn hex(c: u8) -> u8 {
    match c {
        b'0'..=b'9' => c - b'0',
        b'a'..=b'f' => c - b'a' + 10,
        _ => c - b'A' + 10,
    }
}

fn unescape(s: &[u8], mode: UnescapingMode, multisegment: bool) -> Result<Vec<u8>, MatchError> {
    if mode == UnescapingMode::Legacy {
        return Ok(s.to_vec());
    }
    let mode = if multisegment {
        mode
    } else {
        UnescapingMode::AllCharacters
    };

    let mut escapes = 0;
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            escapes += 1;
            if i + 2 >= s.len() || !s[i + 1].is_ascii_hexdigit() || !s[i + 2].is_ascii_hexdigit() {
                let bad = &s[i..s.len().min(i + 3)];
                return Err(MatchError::MalformedEscape(bad.to_vec()));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    if escapes == 0 {
        return Ok(s.to_vec());
    }

    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            let c = hex(s[i + 1]) << 4 | hex(s[i + 2]);
            let decode = match mode {
                UnescapingMode::AllExceptReserved => !is_rfc6570_reserved(c),
                UnescapingMode::AllExceptSlash => c != b'/',
                UnescapingMode::AllCharacters | UnescapingMode::Legacy => true,
            };
            if decode {
                out.push(c);
                i += 3;
                continue;
            }
        }
        out.push(s[i]);
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn comps(path: &str) -> Vec<Vec<u8>> {
        path.split('/').map(|c| c.as_bytes().to_vec()).collect()
    }

    #[test]
    fn a_deep_wildcard_leaves_room_for_the_tail() {
        let p = Pattern::new("/v1/{name=**}/end").unwrap();
        let params = p
            .match_components(&comps("v1/a/b/end"), b"", UnescapingMode::Legacy)
            .unwrap();
        assert_eq!(params.get("name"), Some(b"a/b".as_slice()));
    }

    #[test]
    fn two_deep_wildcards_are_refused() {
        assert!(matches!(
            Pattern::new("/v1/**/**"),
            Err(PatternError::DeepWildcardTwice { .. })
        ));
    }

    #[test]
    fn reserved_characters_stay_escaped_in_a_deep_capture() {
        let p = Pattern::new("/v1/{name=**}").unwrap();
        let params = p
            .match_components(
                &comps("v1/a%2Fb%20c"),
                b"",
                UnescapingMode::AllExceptReserved,
            )
            .unwrap();
        assert_eq!(params.get("name"), Some(b"a%2Fb c".as_slice()));
    }

    #[test]
    fn a_malformed_escape_is_not_a_plain_miss() {
        let p = Pattern::new("/v1/{name}").unwrap();
        assert_eq!(
            p.match_components(&comps("v1/a%zz"), b"", UnescapingMode::AllCharacters),
            Err(MatchError::MalformedEscape(b"%zz".to_vec()))
        );
    }
}
