//! protojson's tokenizer (`google.golang.org/protobuf/internal/encoding/json`
//! `Decoder`), ported token for token: what it accepts decides what a
//! grpc-gateway body may contain, including its quirks (a number read out of
//! a JSON string stops at the first delimiter, `"1,"` is the number 1).

use std::borrow::Cow;

use super::JsonError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Kind {
    Eof,
    Null,
    Bool,
    Number,
    String,
    Name,
    ObjectOpen,
    ObjectClose,
    ArrayOpen,
    ArrayClose,
    Comma,
}

impl Kind {
    fn is_scalar(self) -> bool {
        matches!(self, Kind::Null | Kind::Bool | Kind::Number | Kind::String)
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Token<'a> {
    pub kind: Kind,
    pub pos: usize,
    pub raw: &'a [u8],
    pub boolean: bool,
    /// The parsed string, for `String` and `Name`.
    pub text: Cow<'a, str>,
}

impl<'a> Token<'a> {
    fn new(kind: Kind, pos: usize, raw: &'a [u8]) -> Self {
        Self {
            kind,
            pos,
            raw,
            boolean: false,
            text: Cow::Borrowed(""),
        }
    }

    pub(crate) fn raw_str(&self) -> Cow<'a, str> {
        String::from_utf8_lossy(self.raw)
    }
}

#[derive(Clone)]
pub(crate) struct Decoder<'a> {
    orig: &'a [u8],
    at: usize,
    last_peek: bool,
    last: Result<Token<'a>, JsonError>,
    last_kind: Option<Kind>,
    open: Vec<Kind>,
    /// Reject the one number spelling protojson reads and `encoding/json`
    /// does not: an exponent without digits (`1e`, `1e+`).
    strict_numbers: bool,
}

fn is_not_delim(c: u8) -> bool {
    c == b'-' || c == b'+' || c == b'.' || c == b'_' || c.is_ascii_alphanumeric()
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\n' | b'\r' | b'\t')
}

/// `parseNumber`: the length of the JSON number at the start of `input`.
pub(crate) fn parse_number(input: &[u8]) -> Option<usize> {
    let mut s = input;
    let mut n = 0;
    if s.is_empty() {
        return None;
    }
    if s[0] == b'-' {
        s = &s[1..];
        n += 1;
        if s.is_empty() {
            return None;
        }
    }
    match s[0] {
        b'0' => {
            s = &s[1..];
            n += 1;
        }
        b'1'..=b'9' => {
            s = &s[1..];
            n += 1;
            while !s.is_empty() && s[0].is_ascii_digit() {
                s = &s[1..];
                n += 1;
            }
        }
        _ => return None,
    }
    if s.len() >= 2 && s[0] == b'.' && s[1].is_ascii_digit() {
        s = &s[2..];
        n += 2;
        while !s.is_empty() && s[0].is_ascii_digit() {
            s = &s[1..];
            n += 1;
        }
    }
    if s.len() >= 2 && (s[0] == b'e' || s[0] == b'E') {
        s = &s[1..];
        n += 1;
        if s[0] == b'+' || s[0] == b'-' {
            s = &s[1..];
            n += 1;
            if s.is_empty() {
                return None;
            }
        }
        while !s.is_empty() && s[0].is_ascii_digit() {
            s = &s[1..];
            n += 1;
        }
    }
    if n < input.len() && is_not_delim(input[n]) {
        return None;
    }
    Some(n)
}

impl<'a> Decoder<'a> {
    pub(crate) fn new(input: &'a [u8]) -> Self {
        Self {
            orig: input,
            at: 0,
            last_peek: false,
            last: Ok(Token::new(Kind::Eof, 0, b"")),
            last_kind: None,
            open: Vec::new(),
            strict_numbers: false,
        }
    }

    /// A tokenizer for a request body that `encoding/json` has not checked.
    pub(crate) fn new_strict(input: &'a [u8]) -> Self {
        Self {
            strict_numbers: true,
            ..Self::new(input)
        }
    }

    fn syntax(&self, pos: usize, what: impl std::fmt::Display) -> JsonError {
        JsonError::syntax(format!("syntax error (offset {pos}): {what}"))
    }

    pub(crate) fn peek(&mut self) -> Result<Token<'a>, JsonError> {
        if !self.last_peek {
            self.last = self.read_inner();
        }
        self.last_peek = true;
        self.last.clone()
    }

    /// The kind of the next token, without cloning it.
    pub(crate) fn peek_kind(&mut self) -> Result<Kind, JsonError> {
        if !self.last_peek {
            self.last = self.read_inner();
        }
        self.last_peek = true;
        match &self.last {
            Ok(t) => Ok(t.kind),
            Err(e) => Err(e.clone()),
        }
    }

    pub(crate) fn read(&mut self) -> Result<Token<'a>, JsonError> {
        if self.last_peek {
            self.last_peek = false;
            return std::mem::replace(&mut self.last, Ok(Token::new(Kind::Eof, 0, b"")));
        }
        self.read_inner()
    }

    fn is_value_next(&self) -> bool {
        match self.open.last() {
            None => self.last_kind.is_none(),
            Some(Kind::ObjectOpen) => self.last_kind == Some(Kind::Name),
            Some(_) => matches!(self.last_kind, Some(Kind::ArrayOpen | Kind::Comma)),
        }
    }

    fn read_inner(&mut self) -> Result<Token<'a>, JsonError> {
        loop {
            let mut tok = self.parse_next()?;
            let unexpected = |d: &Self, t: &Token| {
                d.syntax(t.pos, format_args!("unexpected token {}", t.raw_str()))
            };
            match tok.kind {
                Kind::Eof => {
                    if !self.open.is_empty() {
                        return Err(JsonError::eof());
                    }
                }
                Kind::Null | Kind::Bool | Kind::Number => {
                    if !self.is_value_next() {
                        return Err(unexpected(self, &tok));
                    }
                }
                Kind::String => {
                    if !self.is_value_next() {
                        if !matches!(self.last_kind, Some(Kind::ObjectOpen | Kind::Comma)) {
                            return Err(unexpected(self, &tok));
                        }
                        match self.orig.get(self.at) {
                            None => return Err(JsonError::eof()),
                            Some(b':') => {}
                            Some(&c) => {
                                return Err(self.syntax(
                                    self.at,
                                    format_args!(
                                        "unexpected character {}, missing \":\" after field name",
                                        c as char
                                    ),
                                ));
                            }
                        }
                        tok.kind = Kind::Name;
                        self.consume(1);
                    }
                }
                Kind::ObjectOpen | Kind::ArrayOpen => {
                    if !self.is_value_next() {
                        return Err(unexpected(self, &tok));
                    }
                    self.open.push(tok.kind);
                }
                Kind::ObjectClose => {
                    if self.open.last() != Some(&Kind::ObjectOpen)
                        || matches!(self.last_kind, Some(Kind::Name | Kind::Comma))
                    {
                        return Err(unexpected(self, &tok));
                    }
                    self.open.pop();
                }
                Kind::ArrayClose => {
                    if self.open.last() != Some(&Kind::ArrayOpen)
                        || self.last_kind == Some(Kind::Comma)
                    {
                        return Err(unexpected(self, &tok));
                    }
                    self.open.pop();
                }
                Kind::Comma => {
                    let after_value = self.last_kind.is_some_and(|k| {
                        k.is_scalar() || matches!(k, Kind::ObjectClose | Kind::ArrayClose)
                    });
                    if self.open.is_empty() || !after_value {
                        return Err(unexpected(self, &tok));
                    }
                }
                Kind::Name => unreachable!("names are made from strings"),
            }
            self.last_kind = Some(tok.kind);
            if tok.kind != Kind::Comma {
                return Ok(tok);
            }
        }
    }

    fn consume(&mut self, n: usize) {
        self.at += n;
        while self.at < self.orig.len() && is_space(self.orig[self.at]) {
            self.at += 1;
        }
    }

    fn token(&mut self, kind: Kind, size: usize) -> Token<'a> {
        let tok = Token::new(kind, self.at, &self.orig[self.at..self.at + size]);
        self.consume(size);
        tok
    }

    fn parse_next(&mut self) -> Result<Token<'a>, JsonError> {
        self.consume(0);
        let input = &self.orig[self.at..];
        let Some(&first) = input.first() else {
            return Ok(self.token(Kind::Eof, 0));
        };
        let with_delim = |lit: &[u8]| {
            input.starts_with(lit) && !input.get(lit.len()).is_some_and(|&c| is_not_delim(c))
        };
        match first {
            b'n' if with_delim(b"null") => return Ok(self.token(Kind::Null, 4)),
            b't' if with_delim(b"true") => {
                let mut t = self.token(Kind::Bool, 4);
                t.boolean = true;
                return Ok(t);
            }
            b'f' if with_delim(b"false") => return Ok(self.token(Kind::Bool, 5)),
            b'-' | b'0'..=b'9' => {
                if let Some(n) = parse_number(input) {
                    let exponent_without_digits = matches!(input[n - 1], b'e' | b'E' | b'+' | b'-');
                    if !(self.strict_numbers && exponent_without_digits) {
                        return Ok(self.token(Kind::Number, n));
                    }
                }
            }
            b'"' => {
                let (text, n) = self.parse_string(input)?;
                let mut t = self.token(Kind::String, n);
                t.text = text;
                return Ok(t);
            }
            b'{' => return Ok(self.token(Kind::ObjectOpen, 1)),
            b'}' => return Ok(self.token(Kind::ObjectClose, 1)),
            b'[' => return Ok(self.token(Kind::ArrayOpen, 1)),
            b']' => return Ok(self.token(Kind::ArrayClose, 1)),
            b',' => return Ok(self.token(Kind::Comma, 1)),
            _ => {}
        }
        Err(self.syntax(self.at, "invalid value"))
    }

    /// `parseString`: the unescaped text and the length of the literal.
    fn parse_string(&self, input: &'a [u8]) -> Result<(Cow<'a, str>, usize), JsonError> {
        let bad = |what: &str| Err(self.syntax(self.at, what));
        let body = &input[1..];
        // The common case: no escapes, valid UTF-8, no control characters.
        let plain = body
            .iter()
            .position(|&c| c == b'"' || c == b'\\' || c < 0x20)
            .unwrap_or(body.len());
        if body.get(plain) == Some(&b'"') {
            return match std::str::from_utf8(&body[..plain]) {
                Ok(s) => Ok((Cow::Borrowed(s), plain + 2)),
                Err(_) => bad("invalid UTF-8 in string"),
            };
        }
        let mut out = String::new();
        let mut i = 0;
        loop {
            let start = i;
            while i < body.len() && body[i] != b'"' && body[i] != b'\\' && body[i] >= 0x20 {
                i += 1;
            }
            match std::str::from_utf8(&body[start..i]) {
                Ok(s) => out.push_str(s),
                Err(_) => return bad("invalid UTF-8 in string"),
            }
            let Some(&c) = body.get(i) else {
                return Err(JsonError::eof());
            };
            match c {
                b'"' => return Ok((Cow::Owned(out), i + 2)),
                b'\\' => {
                    let Some(&esc) = body.get(i + 1) else {
                        return Err(JsonError::eof());
                    };
                    let short = match esc {
                        b'"' => '"',
                        b'\\' => '\\',
                        b'/' => '/',
                        b'b' => '\u{8}',
                        b'f' => '\u{c}',
                        b'n' => '\n',
                        b'r' => '\r',
                        b't' => '\t',
                        b'u' => {
                            let r = hex4(body.get(i + 2..i + 6))?;
                            let Some(r) = r else {
                                return bad("invalid escape code in string");
                            };
                            i += 6;
                            if (0xd800..0xe000).contains(&r) {
                                let Some(next) = body.get(i..i + 6) else {
                                    return Err(JsonError::eof());
                                };
                                let low = hex4(Some(&next[2..6]))?;
                                let pair = match low {
                                    Some(lo)
                                        if next[0] == b'\\'
                                            && next[1] == b'u'
                                            && (0xd800..0xdc00).contains(&r)
                                            && (0xdc00..0xe000).contains(&lo) =>
                                    {
                                        char::from_u32(
                                            0x10000 + ((r - 0xd800) << 10) + (lo - 0xdc00),
                                        )
                                    }
                                    _ => None,
                                };
                                let Some(ch) = pair else {
                                    return bad("invalid escape code in string");
                                };
                                out.push(ch);
                                i += 6;
                            } else {
                                out.push(char::from_u32(r).expect("not a surrogate"));
                            }
                            continue;
                        }
                        _ => return bad("invalid escape code in string"),
                    };
                    out.push(short);
                    i += 2;
                }
                _ => return bad("invalid character in string"),
            }
        }
    }
}

/// Four hex digits, as `strconv.ParseUint(s, 16, 16)`; `Err` when the input
/// ends first.
fn hex4(s: Option<&[u8]>) -> Result<Option<u32>, JsonError> {
    let Some(s) = s else {
        return Err(JsonError::eof());
    };
    let mut v = 0;
    for &c in s {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            _ => return Ok(None),
        };
        v = v * 16 + d as u32;
    }
    Ok(Some(v))
}

/// A number token that `normalizeToIntString` would return unchanged: an
/// optional `-`, then `0` or digits without a leading zero, and nothing else.
fn plain_integer(raw: &[u8]) -> bool {
    let digits = raw.strip_prefix(b"-").unwrap_or(raw);
    !digits.is_empty()
        && digits.iter().all(u8::is_ascii_digit)
        && (digits[0] != b'0' || digits.len() == 1)
        && raw != b"-0"
}

/// Parts of a number token, for integer conversion (`parseNumberParts`).
struct NumberParts<'a> {
    neg: bool,
    intp: &'a [u8],
    frac: &'a [u8],
    exp: &'a [u8],
}

fn number_parts(input: &[u8]) -> Option<NumberParts<'_>> {
    let mut s = input;
    let mut neg = false;
    if s.is_empty() {
        return None;
    }
    if s[0] == b'-' {
        neg = true;
        s = &s[1..];
        if s.is_empty() {
            return None;
        }
    }
    let mut intp: &[u8] = b"";
    match s[0] {
        b'0' => s = &s[1..],
        b'1'..=b'9' => {
            let n = 1 + s[1..].iter().take_while(|c| c.is_ascii_digit()).count();
            intp = &s[..n];
            s = &s[n..];
        }
        _ => return None,
    }
    let mut frac: &[u8] = b"";
    if s.len() >= 2 && s[0] == b'.' && s[1].is_ascii_digit() {
        let n = 1 + s[2..].iter().take_while(|c| c.is_ascii_digit()).count();
        frac = &s[1..1 + n];
        s = &s[1 + n..];
    }
    let mut exp: &[u8] = b"";
    if s.len() >= 2 && (s[0] == b'e' || s[0] == b'E') {
        s = &s[1..];
        let mut n = 0;
        if s[0] == b'+' || s[0] == b'-' {
            n += 1;
            if s.len() == 1 {
                return None;
            }
        }
        n += s[n..].iter().take_while(|c| c.is_ascii_digit()).count();
        exp = &s[..n];
    }
    while let [rest @ .., b'0'] = frac {
        frac = rest;
    }
    Some(NumberParts {
        neg,
        intp,
        frac,
        exp,
    })
}

/// `normalizeToIntString`: the integer a number token spells, without
/// E-notation, or `None` when it is not an integer.
fn normalize_to_int_string(n: &NumberParts) -> Option<Vec<u8>> {
    if n.intp.is_empty() && n.frac.is_empty() {
        return Some(b"0".to_vec());
    }
    let mut exp: i64 = 0;
    if !n.exp.is_empty() {
        exp = super::go::parse_int(n.exp, 10, 32)?;
    }
    let mut num = Vec::new();
    if n.neg {
        num.push(b'-');
    }
    if exp >= 0 {
        let exp = exp as usize;
        if n.frac.len() > exp || n.intp.len() + exp > 20 {
            return None;
        }
        num.extend_from_slice(n.intp);
        num.extend_from_slice(n.frac);
        num.resize(num.len() + exp - n.frac.len(), b'0');
    } else {
        if !n.frac.is_empty() {
            return None;
        }
        let index = n.intp.len() as i64 + exp;
        if index < 0 {
            return None;
        }
        let index = index as usize;
        if n.intp[index..].iter().any(|&c| c != b'0') {
            return None;
        }
        num.extend_from_slice(&n.intp[..index]);
    }
    Some(num)
}

impl Token<'_> {
    /// `Token.Int(bitSize)`.
    pub(crate) fn int(&self, bits: u32) -> Option<i64> {
        if self.kind != Kind::Number {
            return None;
        }
        // Plain digits are already in normal form.
        if plain_integer(self.raw) {
            return super::go::parse_int(self.raw, 10, bits);
        }
        let s = normalize_to_int_string(&number_parts(self.raw)?)?;
        super::go::parse_int(&s, 10, bits)
    }

    /// `Token.Uint(bitSize)`.
    pub(crate) fn uint(&self, bits: u32) -> Option<u64> {
        if self.kind != Kind::Number {
            return None;
        }
        if plain_integer(self.raw) {
            return super::go::parse_uint(self.raw, 10, bits);
        }
        let s = normalize_to_int_string(&number_parts(self.raw)?)?;
        super::go::parse_uint(&s, 10, bits)
    }

    /// `Token.Float(bitSize)`.
    pub(crate) fn float(&self, bits: u32) -> Option<f64> {
        if self.kind != Kind::Number {
            return None;
        }
        super::go::parse_float(self.raw, bits)
    }
}
