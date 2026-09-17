//! The pieces of Go's standard library whose exact behaviour reaches a
//! grpc-gateway response: `strconv` number parsing and float formatting,
//! `encoding/base64`, `unicode/utf8` decoding, `time` for RFC 3339, and the
//! two JSON string escapers (protojson's and `encoding/json`'s). Each function
//! names the Go function it follows; they were read in Go 1.26.2 and
//! google.golang.org/protobuf v1.36.10, and the conformance vectors measure
//! them through grpc-gateway.

const HEX: &[u8; 16] = b"0123456789abcdef";

fn lower(c: u8) -> u8 {
    c | (b'x' - b'X')
}

/// `strconv.ParseUint`. `base` is 0 (prefixes and underscores allowed) or 10.
pub(crate) fn parse_uint(s: &[u8], base: u32, bits: u32) -> Option<u64> {
    if s.is_empty() {
        return None;
    }
    let base0 = base == 0;
    let s0 = s;
    let mut s = s;
    let mut base = base as u64;
    if base0 {
        base = 10;
        if s[0] == b'0' {
            if s.len() >= 3 && lower(s[1]) == b'b' {
                base = 2;
                s = &s[2..];
            } else if s.len() >= 3 && lower(s[1]) == b'o' {
                base = 8;
                s = &s[2..];
            } else if s.len() >= 3 && lower(s[1]) == b'x' {
                base = 16;
                s = &s[2..];
            } else {
                base = 8;
                s = &s[1..];
            }
        }
    }
    let cutoff = u64::MAX / base + 1;
    let max_val = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut underscores = false;
    let mut n: u64 = 0;
    for &c in s {
        let d = match c {
            b'_' if base0 => {
                underscores = true;
                continue;
            }
            b'0'..=b'9' => c - b'0',
            _ if lower(c).is_ascii_lowercase() => lower(c) - b'a' + 10,
            _ => return None,
        };
        if d as u64 >= base || n >= cutoff {
            return None;
        }
        n *= base;
        let n1 = n.checked_add(d as u64)?;
        if n1 > max_val {
            return None;
        }
        n = n1;
    }
    if underscores && !underscore_ok(s0) {
        return None;
    }
    Some(n)
}

/// `strconv.underscoreOK`.
fn underscore_ok(s: &[u8]) -> bool {
    let mut saw = b'^';
    let mut i = 0;
    let mut s = s;
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        s = &s[1..];
    }
    let mut hex = false;
    if s.len() >= 2 && s[0] == b'0' && matches!(lower(s[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = lower(s[1]) == b'x';
    }
    while i < s.len() {
        let c = s[i];
        i += 1;
        if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            saw = b'0';
            continue;
        }
        if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
            continue;
        }
        if saw == b'_' {
            return false;
        }
        saw = b'!';
    }
    saw != b'_'
}

/// `strconv.ParseInt`. `base` is 0 or 10.
pub(crate) fn parse_int(s: &[u8], base: u32, bits: u32) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    let (neg, digits) = match s[0] {
        b'+' => (false, &s[1..]),
        b'-' => (true, &s[1..]),
        _ => (false, s),
    };
    // A range error from ParseUint still fails the cutoff below.
    let un = parse_uint(digits, base, bits)?;
    let cutoff = 1u64 << (bits - 1);
    if !neg && un >= cutoff || neg && un > cutoff {
        return None;
    }
    Some(if neg {
        (un as i64).wrapping_neg()
    } else {
        un as i64
    })
}

/// `strconv.ParseBool`.
pub(crate) fn parse_bool(s: &[u8]) -> Option<bool> {
    match s {
        b"1" | b"t" | b"T" | b"true" | b"TRUE" | b"True" => Some(true),
        b"0" | b"f" | b"F" | b"false" | b"FALSE" | b"False" => Some(false),
        _ => None,
    }
}

/// `strconv.ParseFloat` for text that is already a JSON number (the only
/// callers): correctly rounded, and an error when the result overflows.
pub(crate) fn parse_float(s: &[u8], bits: u32) -> Option<f64> {
    let s = std::str::from_utf8(s).ok()?;
    // Rust accepts spellings Go does not ("inf", "1."): JSON syntax is
    // checked by the callers, and this guards the rest.
    if !s
        .bytes()
        .all(|b| b.is_ascii_digit() || matches!(b, b'-' | b'+' | b'.' | b'e' | b'E'))
    {
        return None;
    }
    if bits == 32 {
        let v: f32 = s.parse().ok()?;
        v.is_finite().then_some(v as f64)
    } else {
        let v: f64 = s.parse().ok()?;
        v.is_finite().then_some(v)
    }
}

/// `strconv.AppendFloat(b, v, fmt, -1, bits)` with the exponent choice and
/// clean-up of protojson's `appendFloat` and `encoding/json`'s
/// `floatEncoder`, which are the same. The caller handles NaN and infinities.
pub(crate) fn append_float(out: &mut Vec<u8>, v: f64, bits: u32) {
    use std::io::Write;
    let abs = v.abs();
    let exp = abs != 0.0
        && if bits == 64 {
            !(1e-6..1e21).contains(&abs)
        } else {
            let a = abs as f32;
            !(1e-6f32..1e21f32).contains(&a)
        };
    if !exp {
        if bits == 32 {
            let _ = write!(out, "{}", v as f32);
        } else {
            let _ = write!(out, "{v}");
        }
        return;
    }
    let text = if bits == 32 {
        format!("{:e}", v as f32)
    } else {
        format!("{v:e}")
    };
    // Rust writes `1.5e-7` / `1e21`; Go writes `1.5e-07` / `1e+21`, and the
    // clean-up turns `e-07` back into `e-7`.
    let (mantissa, exponent) = text.split_once('e').expect("exponent form");
    out.extend_from_slice(mantissa.as_bytes());
    out.push(b'e');
    match exponent.strip_prefix('-') {
        Some(digits) => {
            out.push(b'-');
            out.extend_from_slice(digits.as_bytes());
        }
        None => {
            out.push(b'+');
            if exponent.len() < 2 {
                out.push(b'0');
            }
            out.extend_from_slice(exponent.as_bytes());
        }
    }
}

/// Appends `s` with every invalid byte replaced by U+FFFD, one per byte, as
/// Go does when it converts or re-encodes bytes rune by rune.
pub(crate) fn push_lossy(out: &mut String, s: &[u8]) {
    let mut i = 0;
    while i < s.len() {
        match std::str::from_utf8(&s[i..]) {
            Ok(rest) => {
                out.push_str(rest);
                return;
            }
            Err(e) => {
                let valid = e.valid_up_to();
                out.push_str(std::str::from_utf8(&s[i..i + valid]).expect("valid prefix"));
                out.push('\u{fffd}');
                i += valid + 1;
            }
        }
    }
}

/// protojson's string encoding (`internal/encoding/json.appendString`): only
/// `"`, `\` and control characters are escaped; `<`, `>`, `&`, DEL and
/// U+2028 are written as they are.
pub(crate) fn append_protojson_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    let bytes = s.as_bytes();
    let mut start = 0;
    for (i, &b) in bytes.iter().enumerate() {
        let short = match b {
            b'"' => b'"',
            b'\\' => b'\\',
            0x08 => b'b',
            0x0c => b'f',
            b'\n' => b'n',
            b'\r' => b'r',
            b'\t' => b't',
            0x00..=0x1f => 0,
            _ => continue,
        };
        out.extend_from_slice(&bytes[start..i]);
        if short == 0 {
            out.extend_from_slice(&[
                b'\\',
                b'u',
                b'0',
                b'0',
                HEX[(b >> 4) as usize],
                HEX[(b & 15) as usize],
            ]);
        } else {
            out.extend_from_slice(&[b'\\', short]);
        }
        start = i + 1;
    }
    out.extend_from_slice(&bytes[start..]);
    out.push(b'"');
}

/// `encoding/json`'s string encoding with HTML escaping on (what
/// `json.Marshal` does): also `<`, `>`, `&`, U+2028 and U+2029.
pub(crate) fn append_encoding_json_string(out: &mut Vec<u8>, s: &str) {
    out.push(b'"');
    for c in s.chars() {
        match c {
            '"' => out.extend_from_slice(b"\\\""),
            '\\' => out.extend_from_slice(b"\\\\"),
            '\u{8}' => out.extend_from_slice(b"\\b"),
            '\u{c}' => out.extend_from_slice(b"\\f"),
            '\n' => out.extend_from_slice(b"\\n"),
            '\r' => out.extend_from_slice(b"\\r"),
            '\t' => out.extend_from_slice(b"\\t"),
            '<' | '>' | '&' | '\u{0}'..='\u{1f}' => {
                let b = c as u8;
                out.extend_from_slice(&[
                    b'\\',
                    b'u',
                    b'0',
                    b'0',
                    HEX[(b >> 4) as usize],
                    HEX[(b & 15) as usize],
                ]);
            }
            '\u{2028}' => out.extend_from_slice(b"\\u2028"),
            '\u{2029}' => out.extend_from_slice(b"\\u2029"),
            _ => {
                let mut buf = [0u8; 4];
                out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            }
        }
    }
    out.push(b'"');
}

const STD: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
const INVALID: u8 = 0xff;
const STD_VALUES: [u8; 256] = values(b'+', b'/');
const URL_VALUES: [u8; 256] = values(b'-', b'_');

const fn values(c62: u8, c63: u8) -> [u8; 256] {
    let mut t = [INVALID; 256];
    let mut i = 0;
    while i < 26 {
        t[(b'A' + i) as usize] = i;
        t[(b'a' + i) as usize] = 26 + i;
        i += 1;
    }
    let mut d = 0;
    while d < 10 {
        t[(b'0' + d) as usize] = 52 + d;
        d += 1;
    }
    t[c62 as usize] = 62;
    t[c63 as usize] = 63;
    t
}

/// `base64.StdEncoding.EncodeToString`.
pub(crate) fn append_base64(out: &mut Vec<u8>, data: &[u8]) {
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as usize) << 16 | (b[1] as usize) << 8 | b[2] as usize;
        out.push(STD[n >> 18]);
        out.push(STD[(n >> 12) & 63]);
        out.push(if chunk.len() > 1 {
            STD[(n >> 6) & 63]
        } else {
            b'='
        });
        out.push(if chunk.len() > 2 { STD[n & 63] } else { b'=' });
    }
}

/// Which `base64.Encoding` to decode with.
#[derive(Clone, Copy)]
pub(crate) struct Base64 {
    pub url: bool,
    pub padding: bool,
}

impl Base64 {
    fn table(self) -> &'static [u8; 256] {
        if self.url { &URL_VALUES } else { &STD_VALUES }
    }

    fn value(self, c: u8) -> Option<u8> {
        let v = self.table()[c as usize];
        (v != INVALID).then_some(v)
    }

    /// `Encoding.DecodeString`, not strict: `\r` and `\n` are skipped
    /// anywhere, and non-zero trailing bits are accepted.
    pub(crate) fn decode(self, src: &[u8]) -> Option<Vec<u8>> {
        let mut out = Vec::with_capacity(src.len() / 4 * 3 + 3);
        let table = self.table();
        let mut si = 0;
        // Whole quanta of alphabet characters, as Go's `assemble32` path.
        while si + 4 <= src.len() {
            let q = [
                table[src[si] as usize],
                table[src[si + 1] as usize],
                table[src[si + 2] as usize],
                table[src[si + 3] as usize],
            ];
            if q.contains(&INVALID) {
                break;
            }
            let val = (q[0] as u32) << 18 | (q[1] as u32) << 12 | (q[2] as u32) << 6 | q[3] as u32;
            out.extend_from_slice(&[(val >> 16) as u8, (val >> 8) as u8, val as u8]);
            si += 4;
        }
        while si < src.len() {
            let mut dbuf = [0u8; 4];
            let mut dlen = 4;
            let mut j = 0;
            while j < 4 {
                if si == src.len() {
                    if j == 0 {
                        return Some(out);
                    }
                    if j == 1 || self.padding {
                        return None;
                    }
                    dlen = j;
                    break;
                }
                let c = src[si];
                si += 1;
                if let Some(v) = self.value(c) {
                    dbuf[j] = v;
                    j += 1;
                    continue;
                }
                if c == b'\n' || c == b'\r' {
                    continue;
                }
                if !self.padding || c != b'=' {
                    return None;
                }
                match j {
                    0 | 1 => return None,
                    2 => {
                        while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                            si += 1;
                        }
                        if si == src.len() || src[si] != b'=' {
                            return None;
                        }
                        si += 1;
                    }
                    _ => {}
                }
                while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                    si += 1;
                }
                if si < src.len() {
                    return None;
                }
                dlen = j;
                break;
            }
            let val = (dbuf[0] as u32) << 18
                | (dbuf[1] as u32) << 12
                | (dbuf[2] as u32) << 6
                | dbuf[3] as u32;
            let bytes = [(val >> 16) as u8, (val >> 8) as u8, val as u8];
            out.extend_from_slice(&bytes[..dlen - 1]);
            if dlen < 4 {
                return Some(out);
            }
        }
        Some(out)
    }
}

/// Days since 1970-01-01 of a proleptic Gregorian date.
pub(crate) fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

/// The date of a day count since 1970-01-01.
pub(crate) fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    (
        if month <= 2 {
            yoe + era * 400 + 1
        } else {
            yoe + era * 400
        },
        month,
        day,
    )
}

fn days_in(month: i64, year: i64) -> i64 {
    match month {
        2 if year % 4 == 0 && (year % 100 != 0 || year % 400 == 0) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// `time.Parse(time.RFC3339Nano, s)`, as Unix seconds and nanoseconds: the
/// fast path (`parseRFC3339`), then the layout parser, which accepts more
/// (a one-digit hour, a comma before the fraction, a `+24:00` offset).
pub(crate) fn parse_rfc3339(s: &[u8]) -> Option<(i64, i32)> {
    parse_rfc3339_fast(s).or_else(|| parse_rfc3339_layout(s))
}

fn digits(s: &[u8], min: i64, max: i64) -> Option<i64> {
    let mut x = 0i64;
    for &c in s {
        if !c.is_ascii_digit() {
            return None;
        }
        x = x * 10 + (c - b'0') as i64;
    }
    (min..=max).contains(&x).then_some(x)
}

fn unix(year: i64, month: i64, day: i64, hour: i64, min: i64, sec: i64, offset: i64) -> i64 {
    days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec - offset
}

/// `parseNanoseconds(value, nbytes)`: `value[0]` is the separator.
fn nanoseconds(value: &[u8], nbytes: usize) -> Option<i64> {
    let nbytes = nbytes.min(10);
    let mut ns = 0i64;
    for &c in &value[1..nbytes] {
        if !c.is_ascii_digit() {
            return None;
        }
        ns = ns * 10 + (c - b'0') as i64;
    }
    for _ in 0..10 - nbytes {
        ns *= 10;
    }
    Some(ns)
}

fn parse_rfc3339_fast(s: &[u8]) -> Option<(i64, i32)> {
    if s.len() < 19 {
        return None;
    }
    let year = digits(&s[0..4], 0, 9999)?;
    let month = digits(&s[5..7], 1, 12)?;
    let day = digits(&s[8..10], 1, days_in(month, year))?;
    let hour = digits(&s[11..13], 0, 23)?;
    let min = digits(&s[14..16], 0, 59)?;
    let sec = digits(&s[17..19], 0, 59)?;
    if !(s[4] == b'-' && s[7] == b'-' && s[10] == b'T' && s[13] == b':' && s[16] == b':') {
        return None;
    }
    let mut s = &s[19..];
    let mut nsec = 0;
    if s.len() >= 2 && s[0] == b'.' && s[1].is_ascii_digit() {
        let mut n = 2;
        while n < s.len() && s[n].is_ascii_digit() {
            n += 1;
        }
        nsec = nanoseconds(s, n)?;
        s = &s[n..];
    }
    let mut offset = 0;
    if !(s.len() == 1 && s[0] == b'Z') {
        if s.len() != 6 {
            return None;
        }
        let hr = digits(&s[1..3], 0, 23)?;
        let mm = digits(&s[4..6], 0, 59)?;
        if !((s[0] == b'-' || s[0] == b'+') && s[3] == b':') {
            return None;
        }
        offset = (hr * 60 + mm) * 60;
        if s[0] == b'-' {
            offset = -offset;
        }
    }
    Some((unix(year, month, day, hour, min, sec, offset), nsec as i32))
}

/// `getnum`: one or two digits, or exactly two when `fixed`.
fn getnum(s: &[u8], fixed: bool) -> Option<(i64, &[u8])> {
    let d = |i: usize| {
        s.get(i)
            .filter(|c| c.is_ascii_digit())
            .map(|c| (c - b'0') as i64)
    };
    let first = d(0)?;
    match d(1) {
        Some(second) => Some((first * 10 + second, &s[2..])),
        None if fixed => None,
        None => Some((first, &s[1..])),
    }
}

fn skip(value: &[u8], prefix: u8) -> Option<&[u8]> {
    (value.first() == Some(&prefix)).then(|| &value[1..])
}

/// `time.parse` with the layout `2006-01-02T15:04:05.999999999Z07:00`.
fn parse_rfc3339_layout(value: &[u8]) -> Option<(i64, i32)> {
    // stdLongYear
    if value.len() < 4 || !value[0].is_ascii_digit() {
        return None;
    }
    let year = digits(&value[..4], 0, 9999)?;
    let value = skip(&value[4..], b'-')?;
    let (month, value) = getnum(value, true)?;
    if !(1..=12).contains(&month) {
        return None;
    }
    let value = skip(value, b'-')?;
    let (day, value) = getnum(value, true)?;
    let value = skip(value, b'T')?;
    let (hour, value) = getnum(value, false)?;
    if hour >= 24 {
        return None;
    }
    let value = skip(value, b':')?;
    let (min, value) = getnum(value, true)?;
    if min >= 60 {
        return None;
    }
    let value = skip(value, b':')?;
    let (sec, mut value) = getnum(value, true)?;
    if sec >= 60 {
        return None;
    }
    // stdFracSecond9
    let mut nsec = 0;
    if value.len() >= 2 && matches!(value[0], b'.' | b',') && value[1].is_ascii_digit() {
        let mut i = 0;
        while i + 1 < value.len() && value[i + 1].is_ascii_digit() {
            i += 1;
        }
        nsec = nanoseconds(value, 1 + i)?;
        value = &value[1 + i..];
    }
    // stdISO8601ColonTZ
    let offset;
    if value.first() == Some(&b'Z') {
        value = &value[1..];
        offset = 0;
    } else {
        if value.len() < 6 || value[3] != b':' {
            return None;
        }
        let (hr, _) = getnum(&value[1..3], true)?;
        let (mm, _) = getnum(&value[4..6], true)?;
        if hr > 24 || mm > 60 {
            return None;
        }
        offset = match value[0] {
            b'+' => (hr * 60 + mm) * 60,
            b'-' => -(hr * 60 + mm) * 60,
            _ => return None,
        };
        value = &value[6..];
    }
    if !value.is_empty() {
        return None;
    }
    if day < 1 || day > days_in(month, year) {
        return None;
    }
    Some((unix(year, month, day, hour, min, sec, offset), nsec as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn floats_are_formatted_like_strconv() {
        let f = |v: f64, bits| {
            let mut out = Vec::new();
            append_float(&mut out, v, bits);
            String::from_utf8(out).unwrap()
        };
        assert_eq!(f(1e21, 64), "1e+21");
        assert_eq!(f(1e-7, 64), "1e-7");
        assert_eq!(f(1.5e-10, 64), "1.5e-10");
        assert_eq!(f(-0.0, 64), "-0");
        assert_eq!(f(1e20, 64), "100000000000000000000");
        assert_eq!(f(5e-324, 64), "5e-324");
        assert_eq!(f(0.1f32 as f64, 32), "0.1");
    }

    #[test]
    fn integers_are_parsed_like_strconv() {
        assert_eq!(parse_int(b"0x_1f", 0, 32), Some(31));
        assert_eq!(parse_int(b"-0b1", 0, 32), Some(-1));
        assert_eq!(parse_int(b"010", 0, 32), Some(8));
        assert_eq!(parse_int(b"1_000", 0, 32), Some(1000));
        assert_eq!(parse_int(b"_1", 0, 32), None);
        assert_eq!(parse_int(b"-0x80000000", 0, 32), Some(i32::MIN as i64));
        assert_eq!(parse_uint(b"+1", 10, 32), None);
        assert_eq!(parse_int(b"+1", 10, 32), Some(1));
    }

    #[test]
    fn base64_follows_go() {
        let std = Base64 {
            url: false,
            padding: true,
        };
        assert_eq!(std.decode(b"AQ==\n\n"), Some(vec![1]));
        assert_eq!(std.decode(b"AQ"), None);
        assert_eq!(std.decode(b"AQJ="), Some(vec![1, 2]));
    }
}
