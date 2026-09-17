//! `net/http`'s `Request.ParseForm`, which the generated handlers and
//! `ServeMux` call: `url.ParseQuery` over the query and, for `POST`, `PUT` and
//! `PATCH` with a form content type, over the body.

use super::strconv::{decode_rune, quote};

/// Form values by key. Go keeps them in a map, so the order of keys carries no
/// meaning; abada keeps first appearance. The values of one key keep their
/// order: body values first, then query values.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Form {
    values: Vec<(Vec<u8>, Vec<Vec<u8>>)>,
}

impl Form {
    pub fn iter(&self) -> impl Iterator<Item = (&[u8], &[Vec<u8>])> {
        self.values
            .iter()
            .map(|(k, v)| (k.as_slice(), v.as_slice()))
    }

    pub fn get(&self, key: &[u8]) -> Option<&[Vec<u8>]> {
        self.values
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_slice())
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    fn add(&mut self, key: Vec<u8>, value: Vec<u8>) {
        match self.values.iter_mut().find(|(k, _)| *k == key) {
            Some((_, vs)) => vs.push(value),
            None => self.values.push((key, vec![value])),
        }
    }

    fn extend(&mut self, other: Form) {
        for (k, vs) in other.values {
            for v in vs {
                self.add(k.clone(), v);
            }
        }
    }
}

/// `url.QueryUnescape`.
fn query_unescape(s: &[u8]) -> Result<Vec<u8>, String> {
    let mut i = 0;
    while i < s.len() {
        if s[i] == b'%' {
            if i + 2 >= s.len() || !s[i + 1].is_ascii_hexdigit() || !s[i + 2].is_ascii_hexdigit() {
                let bad = &s[i..s.len().min(i + 3)];
                return Err(format!("invalid URL escape {}", quote(bad)));
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    let hex = |c: u8| (c as char).to_digit(16).expect("checked") as u8;
    let mut out = Vec::with_capacity(s.len());
    let mut i = 0;
    while i < s.len() {
        match s[i] {
            b'%' => {
                out.push(hex(s[i + 1]) << 4 | hex(s[i + 2]));
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// `url.ParseQuery`: every pair that parses is kept; the first error is
/// returned beside them.
pub(crate) fn parse_query(query: &[u8]) -> (Form, Option<String>) {
    let mut form = Form::default();
    let pairs = query.iter().filter(|&&c| c == b'&').count() + 1;
    if pairs > 10_000 {
        return (
            form,
            Some("number of URL query parameters exceeded limit".into()),
        );
    }
    let mut err: Option<String> = None;
    for key in query.split(|&c| c == b'&') {
        if key.contains(&b';') {
            err.get_or_insert_with(|| "invalid semicolon separator in query".into());
            continue;
        }
        if key.is_empty() {
            continue;
        }
        let (k, v) = match key.iter().position(|&c| c == b'=') {
            Some(i) => (&key[..i], &key[i + 1..]),
            None => (key, &key[key.len()..]),
        };
        let k = match query_unescape(k) {
            Ok(k) => k,
            Err(e) => {
                err.get_or_insert(e);
                continue;
            }
        };
        let v = match query_unescape(v) {
            Ok(v) => v,
            Err(e) => {
                err.get_or_insert(e);
                continue;
            }
        };
        form.add(k, v);
    }
    (form, err)
}

fn is_tspecial(c: u8) -> bool {
    b"()<>@,;:\\\"/[]?=".contains(&c)
}

fn is_token_char(c: u8) -> bool {
    c > 0x20 && c < 0x7f && !is_tspecial(c)
}

fn consume_token(v: &[u8]) -> (&[u8], &[u8]) {
    let n = v.iter().take_while(|&&c| is_token_char(c)).count();
    (&v[..n], &v[n..])
}

/// `strings.TrimLeftFunc(v, unicode.IsSpace)`.
fn trim_left_space(v: &[u8]) -> &[u8] {
    let mut i = 0;
    while i < v.len() {
        let (r, width) = decode_rune(&v[i..]);
        match r {
            Some(c) if c.is_whitespace() => i += width,
            _ => break,
        }
    }
    &v[i..]
}

fn consume_value(v: &[u8]) -> (Vec<u8>, &[u8]) {
    if v.is_empty() {
        return (Vec::new(), v);
    }
    if v[0] != b'"' {
        let (t, rest) = consume_token(v);
        return (t.to_vec(), rest);
    }
    let mut buf = Vec::new();
    let mut i = 1;
    while i < v.len() {
        let r = v[i];
        if r == b'"' {
            return (buf, &v[i + 1..]);
        }
        if r == b'\\' && i + 1 < v.len() && is_tspecial(v[i + 1]) {
            buf.push(v[i + 1]);
            i += 2;
            continue;
        }
        if r == b'\r' || r == b'\n' {
            return (Vec::new(), v);
        }
        buf.push(r);
        i += 1;
    }
    (Vec::new(), v)
}

fn consume_media_param(v: &[u8]) -> (Vec<u8>, Vec<u8>, &[u8]) {
    let rest = trim_left_space(v);
    let Some(rest) = rest.strip_prefix(b";") else {
        return (Vec::new(), Vec::new(), v);
    };
    let rest = trim_left_space(rest);
    let (param, rest) = consume_token(rest);
    let param = param.to_ascii_lowercase();
    if param.is_empty() {
        return (Vec::new(), Vec::new(), v);
    }
    let rest = trim_left_space(rest);
    let Some(rest) = rest.strip_prefix(b"=") else {
        return (Vec::new(), Vec::new(), v);
    };
    let rest = trim_left_space(rest);
    let (value, rest2) = consume_value(rest);
    if value.is_empty() && rest2.len() == rest.len() {
        return (Vec::new(), Vec::new(), v);
    }
    (param, value, rest2)
}

/// `strings.ToLower` then `strings.TrimSpace`: simple case mapping per rune,
/// invalid UTF-8 replaced, as Go's `strings.Map` does.
fn lower_trim(base: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(base);
    let lowered: String = text
        .chars()
        .map(|c| match c {
            '\u{130}' => 'i',
            c => c.to_lowercase().next().unwrap_or(c),
        })
        .collect();
    lowered
        .trim_matches(char::is_whitespace)
        .as_bytes()
        .to_vec()
}

/// Parameters by name, in order.
type Params = Vec<(Vec<u8>, Vec<u8>)>;

/// `mime.ParseMediaType`, keeping only what `parsePostForm` reads: the media
/// type, which Go also returns alongside `ErrInvalidMediaParameter`, and the
/// error.
pub(crate) fn parse_media_type(v: &[u8]) -> (Vec<u8>, Option<&'static str>) {
    let base_len = v.iter().position(|&c| c == b';').unwrap_or(v.len());
    let mediatype = lower_trim(&v[..base_len]);
    let (typ, rest) = consume_token(&mediatype);
    if typ.is_empty() {
        return (Vec::new(), Some("mime: no media type"));
    }
    if !rest.is_empty() {
        let Some(rest) = rest.strip_prefix(b"/") else {
            return (Vec::new(), Some("mime: expected slash after first token"));
        };
        let (subtype, rest) = consume_token(rest);
        if subtype.is_empty() {
            return (Vec::new(), Some("mime: expected token after slash"));
        }
        if !rest.is_empty() {
            return (
                Vec::new(),
                Some("mime: unexpected content after media subtype"),
            );
        }
    }
    let mut params: Params = Vec::new();
    let mut continuation: Vec<(Vec<u8>, Params)> = Vec::new();
    let mut v = &v[base_len..];
    while !v.is_empty() {
        v = trim_left_space(v);
        if v.is_empty() {
            break;
        }
        let (key, value, rest) = consume_media_param(v);
        if key.is_empty() {
            if String::from_utf8_lossy(rest).trim() == ";" {
                break;
            }
            return (mediatype, Some("mime: invalid media parameter"));
        }
        let pmap = match key.iter().position(|&c| c == b'*') {
            Some(star) => {
                let base = key[..star].to_vec();
                let idx = match continuation.iter().position(|(b, _)| *b == base) {
                    Some(i) => i,
                    None => {
                        continuation.push((base, Vec::new()));
                        continuation.len() - 1
                    }
                };
                &mut continuation[idx].1
            }
            None => &mut params,
        };
        if let Some((_, existing)) = pmap.iter().find(|(k, _)| *k == key) {
            if *existing != value {
                return (Vec::new(), Some("mime: duplicate parameter name"));
            }
        } else {
            pmap.push((key, value));
        }
        v = rest;
    }
    (mediatype, None)
}

/// `Request.ParseForm` on a request nobody parsed before. `body` is what is
/// left of the body when it is called: the generated handlers read it first
/// whenever the binding has one, and discard it otherwise.
pub(crate) fn parse_form(
    method: &str,
    content_type: Option<&[u8]>,
    raw_query: &[u8],
    body: &[u8],
) -> (Form, Option<String>) {
    let mut err: Option<String> = None;
    let mut form = Form::default();
    if matches!(method, "POST" | "PUT" | "PATCH") {
        let ct = match content_type {
            None | Some(b"") => b"application/octet-stream".as_slice(),
            Some(ct) => ct,
        };
        let (media, merr) = parse_media_type(ct);
        err = merr.map(str::to_string);
        if media == b"application/x-www-form-urlencoded" {
            if body.len() > 10 << 20 {
                return (form, Some("http: POST too large".into()));
            }
            let (post, e) = parse_query(body);
            if err.is_none() {
                err = e;
            }
            form = post;
        }
    }
    let (query, e) = parse_query(raw_query);
    if err.is_none() {
        err = e;
    }
    form.extend(query);
    (form, err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn media_types_like_go() {
        assert_eq!(parse_media_type(b";").1, Some("mime: no media type"));
        assert_eq!(parse_media_type(b"text").1, None);
        assert_eq!(
            parse_media_type(b"application/x-www-form-urlencoded; x"),
            (
                b"application/x-www-form-urlencoded".to_vec(),
                Some("mime: invalid media parameter")
            )
        );
        assert_eq!(
            parse_media_type(b"text/plain; a=1; a=2").1,
            Some("mime: duplicate parameter name")
        );
        assert_eq!(parse_media_type(b"text/plain;").1, None);
    }

    #[test]
    fn query_like_go() {
        let (form, err) = parse_query(b"a=1&a=2;&b=%zz&c=x+y");
        assert_eq!(err.as_deref(), Some("invalid semicolon separator in query"));
        assert_eq!(form.get(b"a"), Some([b"1".to_vec()].as_slice()));
        assert_eq!(form.get(b"c"), Some([b"x y".to_vec()].as_slice()));
    }
}
