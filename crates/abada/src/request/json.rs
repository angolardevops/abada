//! What `encoding/json` does before and around the proto3 JSON codec in the
//! generated handlers: `json.Decoder.Decode` reads the first JSON value of the
//! body (the rest is never read), and plain Go values — the fields a
//! `body: "<field>"` rule names when they are not messages, and the tree
//! `runtime.FieldMaskFromRequestBody` walks — are decoded with its rules.
//!
//! This is not the proto3 JSON codec, which is [`crate::json::Marshaler`].

use super::strconv::{decode_rune, quote};

/// A decoded JSON value. Numbers keep their literal, because Go parses the
/// literal for the target type; object members keep their order, and a
/// repeated key keeps its last value in the first position it took.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Json {
    Null,
    Bool(bool),
    Number(String),
    String(String),
    Array(Vec<Node>),
    Object(Vec<(String, Node)>),
}

/// A value and where its text is in the input, for the parts Go hands on as
/// a `json.RawMessage`.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Node {
    pub(crate) json: Json,
    pub(crate) start: usize,
    pub(crate) end: usize,
}

/// The outcome of `json.Decoder.Decode(&json.RawMessage)` on a body.
#[derive(Debug, PartialEq)]
pub(crate) enum FirstValue<'a> {
    /// Only whitespace: `io.EOF`, which every generated handler ignores.
    Eof,
    Value(&'a [u8]),
    /// `io.ErrUnexpectedEOF` or a `json.SyntaxError`.
    Error(String),
}

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

fn quote_char(c: u8) -> String {
    match c {
        b'\'' => "'\\''".into(),
        b'"' => "'\"'".into(),
        _ => {
            let s = quote(char::from(c).to_string().as_bytes());
            format!("'{}'", &s[1..s.len() - 1])
        }
    }
}

struct Scanner<'a> {
    s: &'a [u8],
    i: usize,
}

enum ScanError {
    Eof,
    Syntax(String),
}

impl Scanner<'_> {
    fn err(&self, context: &str) -> ScanError {
        ScanError::Syntax(format!(
            "invalid character {} {context}",
            quote_char(self.s[self.i])
        ))
    }

    fn peek(&self) -> Result<u8, ScanError> {
        self.s.get(self.i).copied().ok_or(ScanError::Eof)
    }

    fn skip_space(&mut self) {
        while self.i < self.s.len() && is_space(self.s[self.i]) {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Result<(), ScanError> {
        self.skip_space();
        match self.peek()? {
            b'{' => {
                self.i += 1;
                self.skip_space();
                if self.peek()? == b'}' {
                    self.i += 1;
                    return Ok(());
                }
                loop {
                    self.skip_space();
                    if self.peek()? != b'"' {
                        return Err(self.err("looking for beginning of object key string"));
                    }
                    self.string()?;
                    self.skip_space();
                    if self.peek()? != b':' {
                        return Err(self.err("after object key"));
                    }
                    self.i += 1;
                    self.value()?;
                    self.skip_space();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b'}' => {
                            self.i += 1;
                            return Ok(());
                        }
                        _ => return Err(self.err("after object key:value pair")),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                self.skip_space();
                if self.peek()? == b']' {
                    self.i += 1;
                    return Ok(());
                }
                loop {
                    self.value()?;
                    self.skip_space();
                    match self.peek()? {
                        b',' => self.i += 1,
                        b']' => {
                            self.i += 1;
                            return Ok(());
                        }
                        _ => return Err(self.err("after array element")),
                    }
                }
            }
            b'"' => self.string(),
            b't' => self.literal(b"true"),
            b'f' => self.literal(b"false"),
            b'n' => self.literal(b"null"),
            b'-' | b'0'..=b'9' => self.number(),
            _ => Err(self.err("looking for beginning of value")),
        }
    }

    fn literal(&mut self, word: &[u8]) -> Result<(), ScanError> {
        let name = std::str::from_utf8(word).expect("ASCII");
        for (k, &expected) in word.iter().enumerate() {
            let c = self.peek()?;
            if c != expected {
                return Err(if k == 0 {
                    self.err("looking for beginning of value")
                } else {
                    self.err(&format!(
                        "in literal {name} (expecting {})",
                        quote_char(expected)
                    ))
                });
            }
            self.i += 1;
        }
        Ok(())
    }

    fn string(&mut self) -> Result<(), ScanError> {
        self.i += 1;
        loop {
            let c = self.peek()?;
            match c {
                b'"' => {
                    self.i += 1;
                    return Ok(());
                }
                b'\\' => {
                    self.i += 1;
                    match self.peek()? {
                        b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => self.i += 1,
                        b'u' => {
                            self.i += 1;
                            for _ in 0..4 {
                                if !self.peek()?.is_ascii_hexdigit() {
                                    return Err(self.err("in \\u hexadecimal character escape"));
                                }
                                self.i += 1;
                            }
                        }
                        _ => return Err(self.err("in string escape code")),
                    }
                }
                c if c < 0x20 => return Err(self.err("in string literal")),
                _ => self.i += 1,
            }
        }
    }

    /// Numbers end at the first byte that cannot continue them; at the top
    /// level that byte is not an error for this value.
    fn number(&mut self) -> Result<(), ScanError> {
        if self.peek()? == b'-' {
            self.i += 1;
            match self.peek()? {
                b'0'..=b'9' => {}
                _ => return Err(self.err("in numeric literal")),
            }
        }
        if self.peek()? == b'0' {
            self.i += 1;
        } else {
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.i < self.s.len() && self.s[self.i] == b'.' {
            self.i += 1;
            if !self.peek()?.is_ascii_digit() {
                return Err(self.err("after decimal point in numeric literal"));
            }
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        if self.i < self.s.len() && matches!(self.s[self.i], b'e' | b'E') {
            self.i += 1;
            if matches!(self.peek()?, b'+' | b'-') {
                self.i += 1;
            }
            if !self.peek()?.is_ascii_digit() {
                return Err(self.err("in exponent of numeric literal"));
            }
            while self.i < self.s.len() && self.s[self.i].is_ascii_digit() {
                self.i += 1;
            }
        }
        Ok(())
    }
}

/// `json.NewDecoder(body).Decode(&raw)`.
pub(crate) fn first_value(body: &[u8]) -> FirstValue<'_> {
    let mut scanner = Scanner { s: body, i: 0 };
    scanner.skip_space();
    if scanner.i == body.len() {
        return FirstValue::Eof;
    }
    let start = scanner.i;
    match scanner.value() {
        Ok(()) => FirstValue::Value(&body[start..scanner.i]),
        Err(ScanError::Eof) => FirstValue::Error("unexpected EOF".into()),
        Err(ScanError::Syntax(msg)) => FirstValue::Error(msg),
    }
}

struct Parser<'a> {
    s: &'a [u8],
    i: usize,
}

impl Parser<'_> {
    fn skip_space(&mut self) {
        while self.i < self.s.len() && is_space(self.s[self.i]) {
            self.i += 1;
        }
    }

    fn value(&mut self) -> Node {
        self.skip_space();
        let start = self.i;
        let json = match self.s[self.i] {
            b'{' => {
                self.i += 1;
                let mut members: Vec<(String, Node)> = Vec::new();
                loop {
                    self.skip_space();
                    if self.s[self.i] == b'}' {
                        self.i += 1;
                        break Json::Object(members);
                    }
                    if self.s[self.i] == b',' {
                        self.i += 1;
                        continue;
                    }
                    let key = self.string();
                    self.skip_space();
                    self.i += 1; // ':'
                    let value = self.value();
                    match members.iter_mut().find(|(k, _)| *k == key) {
                        Some((_, v)) => *v = value,
                        None => members.push((key, value)),
                    }
                }
            }
            b'[' => {
                self.i += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_space();
                    if self.s[self.i] == b']' {
                        self.i += 1;
                        break Json::Array(items);
                    }
                    if self.s[self.i] == b',' {
                        self.i += 1;
                        continue;
                    }
                    items.push(self.value());
                }
            }
            b'"' => Json::String(self.string()),
            b't' => {
                self.i += 4;
                Json::Bool(true)
            }
            b'f' => {
                self.i += 5;
                Json::Bool(false)
            }
            b'n' => {
                self.i += 4;
                Json::Null
            }
            _ => {
                while self.i < self.s.len()
                    && matches!(
                        self.s[self.i],
                        b'-' | b'+' | b'.' | b'e' | b'E' | b'0'..=b'9'
                    )
                {
                    self.i += 1;
                }
                Json::Number(String::from_utf8(self.s[start..self.i].to_vec()).expect("ASCII"))
            }
        };
        Node {
            json,
            start,
            end: self.i,
        }
    }

    /// `encoding/json`'s `unquote`: invalid UTF-8 and unpaired surrogates
    /// become U+FFFD.
    fn string(&mut self) -> String {
        self.i += 1;
        let mut out = String::new();
        loop {
            match self.s[self.i] {
                b'"' => {
                    self.i += 1;
                    return out;
                }
                b'\\' => {
                    let esc = self.s[self.i + 1];
                    self.i += 2;
                    match esc {
                        b'b' => out.push('\x08'),
                        b'f' => out.push('\x0c'),
                        b'n' => out.push('\n'),
                        b'r' => out.push('\r'),
                        b't' => out.push('\t'),
                        b'u' => {
                            let r = self.hex4();
                            if (0xd800..0xdc00).contains(&r)
                                && self.s.get(self.i) == Some(&b'\\')
                                && self.s.get(self.i + 1) == Some(&b'u')
                            {
                                let save = self.i;
                                self.i += 2;
                                let r2 = self.hex4();
                                if (0xdc00..0xe000).contains(&r2) {
                                    let c = 0x10000 + ((r - 0xd800) << 10) + (r2 - 0xdc00);
                                    out.push(char::from_u32(c).unwrap_or('\u{fffd}'));
                                    continue;
                                }
                                self.i = save;
                            }
                            out.push(char::from_u32(r).unwrap_or('\u{fffd}'));
                        }
                        c => out.push(char::from(c)),
                    }
                }
                _ => {
                    let (r, width) = decode_rune(&self.s[self.i..]);
                    out.push(r.unwrap_or('\u{fffd}'));
                    self.i += width;
                }
            }
        }
    }

    fn hex4(&mut self) -> u32 {
        let hex = std::str::from_utf8(&self.s[self.i..self.i + 4]).expect("scanned");
        self.i += 4;
        u32::from_str_radix(hex, 16).expect("scanned")
    }
}

/// Parses a value [`first_value`] accepted.
pub(crate) fn parse(value: &[u8]) -> Node {
    Parser { s: value, i: 0 }.value()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_value_is_all_that_is_read() {
        assert_eq!(first_value(b" \n"), FirstValue::Eof);
        assert_eq!(first_value(b"{} x"), FirstValue::Value(b"{}"));
        assert_eq!(first_value(b"123abc"), FirstValue::Value(b"123"));
        assert_eq!(first_value(b"01"), FirstValue::Value(b"0"));
        assert_eq!(
            first_value(b"{"),
            FirstValue::Error("unexpected EOF".into())
        );
        assert_eq!(
            first_value(b"f_int32=5"),
            FirstValue::Error("invalid character '_' in literal false (expecting 'a')".into())
        );
        assert_eq!(
            first_value(b"x"),
            FirstValue::Error("invalid character 'x' looking for beginning of value".into())
        );
    }

    #[test]
    fn repeated_keys_keep_the_last_value() {
        let input = br#"{"a":1,"b":"\ud83d\ude00","a":null}"#;
        let Json::Object(members) = parse(input).json else {
            panic!("an object");
        };
        assert_eq!(members.len(), 2);
        assert_eq!(members[0].0, "a");
        assert_eq!(members[0].1.json, Json::Null);
        assert_eq!(&input[members[0].1.start..members[0].1.end], b"null");
        assert_eq!(members[1].1.json, Json::String("\u{1f600}".into()));
    }
}
