//! Path templates as defined by `google/api/http.proto`.
//!
//! The grammar is the one in the proto, but what is accepted is what
//! grpc-gateway's `internal/httprule` parser accepts, quirks included: a
//! template that parses there parses here, and one that fails there fails here.
//! `conformance/vectors/path.json` holds grpc-gateway's own answers and the
//! tests replay them.

use std::fmt;

/// Terminal symbol appended to every token sequence, as in grpc-gateway.
const EOF: &str = "\0";

/// One `/`-separated piece of a template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Segment {
    /// `*`: exactly one path component.
    Wildcard,
    /// `**`: zero or more path components.
    DeepWildcard,
    /// A literal component. The root template `/` is a single empty literal.
    Literal(String),
    /// `{field.path=segments}`; `{field}` is `{field=*}`.
    Variable {
        field_path: String,
        segments: Vec<Segment>,
    },
}

/// A parsed path template, e.g. `/v1/{name=projects/*}:start`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathTemplate {
    segments: Vec<Segment>,
    verb: String,
    source: String,
}

/// A template grpc-gateway would also refuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseError {
    template: String,
    message: String,
}

impl ParseError {
    fn new(template: &str, message: impl Into<String>) -> Self {
        Self {
            template: template.to_string(),
            message: message.into(),
        }
    }

    /// The template that failed to parse.
    pub fn template(&self) -> &str {
        &self.template
    }
}

impl fmt::Display for ParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "invalid path template {:?}: {}",
            self.template, self.message
        )
    }
}

impl std::error::Error for ParseError {}

impl PathTemplate {
    /// Parses a template.
    pub fn parse(template: &str) -> Result<Self, ParseError> {
        let Some(rest) = template.strip_prefix('/') else {
            return Err(ParseError::new(template, "no leading /"));
        };
        let (tokens, verb) = tokenize(rest);
        let mut parser = Parser {
            tokens,
            pos: 0,
            accepted: Vec::new(),
        };
        let segments = parser
            .top_level_segments()
            .map_err(|message| ParseError::new(template, message))?;
        Ok(Self {
            segments,
            verb,
            source: template.to_string(),
        })
    }

    pub fn segments(&self) -> &[Segment] {
        &self.segments
    }

    /// The custom verb after the last `:`, or `""`.
    pub fn verb(&self) -> &str {
        &self.verb
    }

    /// The template exactly as it was written.
    pub fn as_str(&self) -> &str {
        &self.source
    }

    /// Every field path bound by a variable, in template order.
    pub fn fields(&self) -> Vec<&str> {
        fn walk<'a>(segments: &'a [Segment], out: &mut Vec<&'a str>) {
            for s in segments {
                if let Segment::Variable {
                    field_path,
                    segments,
                } = s
                {
                    walk(segments, out);
                    out.push(field_path);
                }
            }
        }
        let mut out = Vec::new();
        walk(&self.segments, &mut out);
        out
    }

    /// The opcode form grpc-gateway's `httprule.Template` carries. Exposed so
    /// the conformance suite can compare it op for op.
    #[doc(hidden)]
    pub fn compile(&self) -> Compiled {
        let mut raw = Vec::new();
        for s in &self.segments {
            compile_segment(s, &mut raw);
        }
        let mut compiled = Compiled {
            op_codes: Vec::with_capacity(raw.len() * 2),
            pool: Vec::new(),
            verb: self.verb.clone(),
            fields: Vec::new(),
        };
        for op in raw {
            compiled.op_codes.push(op.code as i64);
            match op.operand {
                Operand::Num(n) => compiled.op_codes.push(n as i64),
                Operand::Str(s) => {
                    let index = match compiled.pool.iter().position(|p| *p == s) {
                        Some(i) => i,
                        None => {
                            compiled.pool.push(s.clone());
                            compiled.pool.len() - 1
                        }
                    };
                    compiled.op_codes.push(index as i64);
                    if op.code == OpCode::Capture {
                        compiled.fields.push(s);
                    }
                }
            }
        }
        compiled
    }
}

impl fmt::Display for PathTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "/")?;
        write_segments(f, &self.segments)?;
        if !self.verb.is_empty() {
            write!(f, ":{}", self.verb)?;
        }
        Ok(())
    }
}

fn write_segments(f: &mut fmt::Formatter<'_>, segments: &[Segment]) -> fmt::Result {
    for (i, s) in segments.iter().enumerate() {
        if i > 0 {
            write!(f, "/")?;
        }
        match s {
            Segment::Wildcard => write!(f, "*")?,
            Segment::DeepWildcard => write!(f, "**")?,
            Segment::Literal(l) => write!(f, "{l}")?,
            Segment::Variable {
                field_path,
                segments,
            } => {
                write!(f, "{{{field_path}=")?;
                write_segments(f, segments)?;
                write!(f, "}}")?;
            }
        }
    }
    Ok(())
}

/// grpc-gateway's compiled template.
#[doc(hidden)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Compiled {
    pub op_codes: Vec<i64>,
    pub pool: Vec<String>,
    pub verb: String,
    pub fields: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum OpCode {
    Push = 1,
    LitPush = 2,
    PushM = 3,
    ConcatN = 4,
    Capture = 5,
}

impl OpCode {
    pub(crate) fn from_i64(v: i64) -> Option<Self> {
        Some(match v {
            1 => Self::Push,
            2 => Self::LitPush,
            3 => Self::PushM,
            4 => Self::ConcatN,
            5 => Self::Capture,
            _ => return None,
        })
    }
}

enum Operand {
    Num(usize),
    Str(String),
}

struct RawOp {
    code: OpCode,
    operand: Operand,
}

fn compile_segment(segment: &Segment, out: &mut Vec<RawOp>) {
    match segment {
        Segment::Wildcard => out.push(RawOp {
            code: OpCode::Push,
            operand: Operand::Num(0),
        }),
        Segment::DeepWildcard => out.push(RawOp {
            code: OpCode::PushM,
            operand: Operand::Num(0),
        }),
        // Always through the pool, including the root's empty literal:
        // grpc-gateway only turns its EOF literal into "" after choosing the pool.
        Segment::Literal(l) => out.push(RawOp {
            code: OpCode::LitPush,
            operand: Operand::Str(l.clone()),
        }),
        Segment::Variable {
            field_path,
            segments,
        } => {
            for s in segments {
                compile_segment(s, out);
            }
            out.push(RawOp {
                code: OpCode::ConcatN,
                operand: Operand::Num(segments.len()),
            });
            out.push(RawOp {
                code: OpCode::Capture,
                operand: Operand::Str(field_path.clone()),
            });
        }
    }
}

fn tokenize(path: &str) -> (Vec<String>, String) {
    if path.is_empty() {
        return (vec![EOF.to_string()], String::new());
    }

    enum State {
        Init,
        Field,
        Nested,
    }
    let mut state = State::Init;
    let mut tokens: Vec<String> = Vec::new();
    let mut path = path;
    while !path.is_empty() {
        let delimiters: &[u8] = match state {
            State::Init => b"/{",
            State::Field => b".=}",
            State::Nested => b"/}",
        };
        let Some(idx) = path.bytes().position(|b| delimiters.contains(&b)) else {
            tokens.push(path.to_string());
            break;
        };
        match path.as_bytes()[idx] {
            b'{' => state = State::Field,
            b'=' => state = State::Nested,
            b'}' => state = State::Init,
            _ => {}
        }
        if idx > 0 {
            tokens.push(path[..idx].to_string());
        }
        tokens.push(path[idx..idx + 1].to_string());
        path = &path[idx + 1..];
    }

    // A variable followed by `:` makes everything after the FIRST colon the
    // verb; otherwise the verb starts after the last one (grpc-gateway#1947).
    let last = tokens.len() - 1;
    let after_variable = tokens.len() >= 2 && tokens[last - 1] == "}";
    let token = tokens[last].clone();
    let colon = if after_variable {
        token.find(':')
    } else {
        token.rfind(':')
    };
    let mut verb = String::new();
    match colon {
        Some(0) => {
            tokens.pop();
            verb = token[1..].to_string();
        }
        Some(i) => {
            tokens[last] = token[..i].to_string();
            verb = token[i + 1..].to_string();
        }
        None => {}
    }
    tokens.push(EOF.to_string());
    (tokens, verb)
}

#[derive(Clone, Copy)]
enum Term {
    Symbol(&'static str),
    Ident,
    Literal,
    Eof,
}

struct Parser {
    tokens: Vec<String>,
    pos: usize,
    accepted: Vec<String>,
}

impl Parser {
    fn top_level_segments(&mut self) -> Result<Vec<Segment>, String> {
        if self.accept(Term::Eof).is_ok() {
            return Ok(vec![Segment::Literal(String::new())]);
        }
        let segments = self.segments()?;
        if self.accept(Term::Eof).is_err() {
            return Err(format!(
                "unexpected token {:?} after segments {:?}",
                self.current(),
                self.accepted.concat()
            ));
        }
        Ok(segments)
    }

    fn segments(&mut self) -> Result<Vec<Segment>, String> {
        let mut segments = vec![self.segment()?];
        while self.accept(Term::Symbol("/")).is_ok() {
            segments.push(self.segment()?);
        }
        Ok(segments)
    }

    fn segment(&mut self) -> Result<Segment, String> {
        if self.accept(Term::Symbol("*")).is_ok() {
            return Ok(Segment::Wildcard);
        }
        if self.accept(Term::Symbol("**")).is_ok() {
            return Ok(Segment::DeepWildcard);
        }
        if let Ok(literal) = self.accept(Term::Literal) {
            return Ok(Segment::Literal(literal));
        }
        self.variable()
            .map_err(|e| format!("segment neither wildcards, literal or variable: {e}"))
    }

    fn variable(&mut self) -> Result<Segment, String> {
        self.accept(Term::Symbol("{"))?;
        let field_path = self.field_path()?;
        let segments = if self.accept(Term::Symbol("=")).is_ok() {
            self.segments()
                .map_err(|e| format!("invalid segment in variable {field_path:?}: {e}"))?
        } else {
            vec![Segment::Wildcard]
        };
        if self.accept(Term::Symbol("}")).is_err() {
            return Err(format!("unterminated variable segment: {field_path}"));
        }
        Ok(Segment::Variable {
            field_path,
            segments,
        })
    }

    fn field_path(&mut self) -> Result<String, String> {
        let mut components = vec![self.accept(Term::Ident)?];
        while self.accept(Term::Symbol(".")).is_ok() {
            let c = self
                .accept(Term::Ident)
                .map_err(|e| format!("invalid field path component: {e}"))?;
            components.push(c);
        }
        Ok(components.join("."))
    }

    fn current(&self) -> &str {
        self.tokens.get(self.pos).map_or(EOF, String::as_str)
    }

    fn accept(&mut self, term: Term) -> Result<String, String> {
        let token = self.current();
        match term {
            // grpc-gateway lets a "/" token stand in for any symbol. It is
            // kept because a template's meaning must not change between the two.
            Term::Symbol(s) => {
                if token != s && token != "/" {
                    return Err(format!("expected {s:?} but got {token:?}"));
                }
            }
            Term::Eof => {
                if token != EOF {
                    return Err(format!("expected EOF but got {token:?}"));
                }
            }
            Term::Ident => expect_ident(token)?,
            Term::Literal => expect_pchars(token)?,
        }
        let token = token.to_string();
        self.pos += 1;
        self.accepted.push(token.clone());
        Ok(token)
    }
}

/// `pchar` from RFC 3986: unreserved, pct-encoded, sub-delims, `:` and `@`.
fn expect_pchars(token: &str) -> Result<(), String> {
    let mut pending_hex = 0;
    for c in token.chars() {
        if pending_hex > 0 {
            if !c.is_ascii_hexdigit() {
                return Err(format!("invalid hexdigit: {c:?}"));
            }
            pending_hex -= 1;
            continue;
        }
        match c {
            'A'..='Z' | 'a'..='z' | '0'..='9' => {}
            '-' | '.' | '_' | '~' => {}
            '!' | '$' | '&' | '\'' | '(' | ')' | '*' | '+' | ',' | ';' | '=' => {}
            ':' | '@' => {}
            '%' => pending_hex = 2,
            _ => return Err(format!("invalid character in path segment: {c:?}")),
        }
    }
    if pending_hex > 0 {
        return Err(format!("invalid percent-encoding in {token:?}"));
    }
    Ok(())
}

/// A `.proto` identifier: `[A-Za-z_][A-Za-z0-9_]*`.
fn expect_ident(token: &str) -> Result<(), String> {
    if token.is_empty() {
        return Err("empty identifier".into());
    }
    for (i, c) in token.chars().enumerate() {
        match c {
            '0'..='9' if i == 0 => {
                return Err(format!("identifier starting with digit: {token}"));
            }
            '0'..='9' | 'A'..='Z' | 'a'..='z' | '_' => {}
            _ => return Err(format!("invalid character {c:?} in identifier: {token}")),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_resource_name_with_a_verb() {
        let t = PathTemplate::parse("/v1/{name=projects/*/vms/*}:start").unwrap();
        assert_eq!(t.verb(), "start");
        assert_eq!(t.fields(), ["name"]);
        assert_eq!(t.to_string(), "/v1/{name=projects/*/vms/*}:start");
    }

    #[test]
    fn a_colon_inside_the_last_literal_is_the_verb_boundary() {
        let t = PathTemplate::parse("/v1/a:b:c").unwrap();
        assert_eq!(t.verb(), "c");
    }

    #[test]
    fn nested_variables_are_refused() {
        assert!(PathTemplate::parse("/v1/{a={b}}").is_err());
    }

    #[test]
    fn the_root_is_one_empty_literal() {
        let t = PathTemplate::parse("/").unwrap();
        assert_eq!(t.segments(), [Segment::Literal(String::new())]);
        assert_eq!(t.to_string(), "/");
    }
}
