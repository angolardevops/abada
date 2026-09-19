//! The parts of Go's `strconv` and `encoding/base64` grpc-gateway's converters
//! call, with Go's grammar and error text. Values are bytes: a Go string need
//! not be UTF-8, and a percent-decoded query value often is not.

use std::fmt;

use super::isprint::NOT_PRINTABLE;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NumErrorKind {
    Syntax,
    Range,
}

/// `strconv.NumError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NumError {
    func: &'static str,
    num: Vec<u8>,
    kind: NumErrorKind,
}

impl fmt::Display for NumError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "strconv.{}: parsing {}: {}",
            self.func,
            quote(&self.num),
            match self.kind {
                NumErrorKind::Syntax => "invalid syntax",
                NumErrorKind::Range => "value out of range",
            }
        )
    }
}

fn lower(c: u8) -> u8 {
    c | 0x20
}

fn syntax(func: &'static str, s: &[u8]) -> NumError {
    NumError {
        func,
        num: s.to_vec(),
        kind: NumErrorKind::Syntax,
    }
}

fn range(func: &'static str, s: &[u8]) -> NumError {
    NumError {
        func,
        num: s.to_vec(),
        kind: NumErrorKind::Range,
    }
}

/// `strconv.ParseUint(s, base, bits)` for base 0 (`base0`) or 10.
pub(crate) fn parse_uint(s: &[u8], base0: bool, bits: u32) -> Result<u64, NumError> {
    parse_uint_as("ParseUint", s, s, base0, bits)
}

fn parse_uint_as(
    func: &'static str,
    s0: &[u8],
    s: &[u8],
    base0: bool,
    bits: u32,
) -> Result<u64, NumError> {
    if s.is_empty() {
        return Err(syntax(func, s0));
    }
    let digits_from = s;
    let mut s = s;
    let mut base: u64 = 10;
    if base0 && s[0] == b'0' {
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
    let cutoff = u64::MAX / base + 1;
    let max_val = if bits == 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    };
    let mut underscores = false;
    let mut n: u64 = 0;
    for &c in s {
        let d = if c == b'_' && base0 {
            underscores = true;
            continue;
        } else if c.is_ascii_digit() {
            c - b'0'
        } else if lower(c).is_ascii_lowercase() {
            lower(c) - b'a' + 10
        } else {
            return Err(syntax(func, s0));
        };
        if u64::from(d) >= base {
            return Err(syntax(func, s0));
        }
        if n >= cutoff {
            return Err(range(func, s0));
        }
        n *= base;
        let n1 = n.wrapping_add(u64::from(d));
        if n1 < n || n1 > max_val {
            return Err(range(func, s0));
        }
        n = n1;
    }
    if underscores && !underscore_ok(digits_from) {
        return Err(syntax(func, s0));
    }
    Ok(n)
}

/// `strconv.ParseInt(s, base, bits)` for base 0 (`base0`) or 10.
pub(crate) fn parse_int(s0: &[u8], base0: bool, bits: u32) -> Result<i64, NumError> {
    const FUNC: &str = "ParseInt";
    if s0.is_empty() {
        return Err(syntax(FUNC, s0));
    }
    let (neg, s) = match s0[0] {
        b'+' => (false, &s0[1..]),
        b'-' => (true, &s0[1..]),
        _ => (false, s0),
    };
    let un = match parse_uint_as(FUNC, s0, s, base0, bits) {
        Ok(v) => v,
        Err(e) if e.kind == NumErrorKind::Range => u64::MAX,
        Err(e) => return Err(e),
    };
    let cutoff = 1u64 << (bits - 1);
    if !neg && un >= cutoff {
        return Err(range(FUNC, s0));
    }
    if neg && un > cutoff {
        return Err(range(FUNC, s0));
    }
    Ok(if neg {
        (un as i64).wrapping_neg()
    } else {
        un as i64
    })
}

/// `strconv.Atoi`: base 10, the platform's 64-bit int.
pub(crate) fn atoi(s: &[u8]) -> Option<i64> {
    parse_int(s, false, 64).ok()
}

fn underscore_ok(s: &[u8]) -> bool {
    let mut saw = b'^';
    let mut s = s;
    if !s.is_empty() && (s[0] == b'-' || s[0] == b'+') {
        s = &s[1..];
    }
    let mut hex = false;
    let mut i = 0;
    if s.len() >= 2 && s[0] == b'0' && matches!(lower(s[1]), b'b' | b'o' | b'x') {
        i = 2;
        saw = b'0';
        hex = lower(s[1]) == b'x';
    }
    while i < s.len() {
        let c = s[i];
        if c.is_ascii_digit() || hex && (b'a'..=b'f').contains(&lower(c)) {
            saw = b'0';
        } else if c == b'_' {
            if saw != b'0' {
                return false;
            }
            saw = b'_';
        } else {
            if saw == b'_' {
                return false;
            }
            saw = b'!';
        }
        i += 1;
    }
    saw != b'_'
}

/// `strconv.ParseBool`.
pub(crate) fn parse_bool(s: &[u8]) -> Result<bool, NumError> {
    match s {
        b"1" | b"t" | b"T" | b"true" | b"TRUE" | b"True" => Ok(true),
        b"0" | b"f" | b"F" | b"false" | b"FALSE" | b"False" => Ok(false),
        _ => Err(syntax("ParseBool", s)),
    }
}

/// `strconv.ParseFloat(s, bits)` for 32 and 64 bits. A 32-bit result is
/// returned widened, as Go does.
pub(crate) fn parse_float(s: &[u8], bits: u32) -> Result<f64, NumError> {
    const FUNC: &str = "ParseFloat";
    if let Some((f, n)) = special(s) {
        if n != s.len() {
            return Err(syntax(FUNC, s));
        }
        return Ok(f);
    }
    let Some(r) = read_float(s) else {
        return Err(syntax(FUNC, s));
    };
    if r.end != s.len() {
        return Err(syntax(FUNC, s));
    }
    let f = if r.hex {
        atof_hex(bits, r.mantissa, r.exp, r.neg, r.trunc)
    } else {
        // Correctly rounded in both languages; only the spelling differs.
        let mut text = String::with_capacity(s.len());
        for &c in s {
            if c != b'_' {
                text.push(c as char);
            }
        }
        if bits == 32 {
            text.parse::<f32>().map(f64::from).ok()
        } else {
            text.parse::<f64>().ok()
        }
    };
    match f {
        Some(f) if f.is_infinite() => Err(range(FUNC, s)),
        Some(f) => Ok(f),
        None => Err(syntax(FUNC, s)),
    }
}

fn special(s: &[u8]) -> Option<(f64, usize)> {
    let first = *s.first()?;
    let common = |s: &[u8], word: &[u8]| {
        s.iter()
            .zip(word)
            .take_while(|(a, b)| a.to_ascii_lowercase() == **b)
            .count()
    };
    match first {
        b'+' | b'-' | b'i' | b'I' => {
            let (sign, nsign, rest) = match first {
                b'+' => (1.0, 1, &s[1..]),
                b'-' => (-1.0, 1, &s[1..]),
                _ => (1.0, 0, s),
            };
            let mut n = common(rest, b"infinity");
            if 3 < n && n < 8 {
                n = 3;
            }
            if n == 3 || n == 8 {
                return Some((sign * f64::INFINITY, nsign + n));
            }
            None
        }
        // `math.NaN()` is 0x7FF8000000000001, and Go keeps that payload.
        b'n' | b'N' if common(s, b"nan") == 3 => Some((f64::from_bits(0x7ff8_0000_0000_0001), 3)),
        _ => None,
    }
}

struct ReadFloat {
    mantissa: u64,
    exp: i64,
    neg: bool,
    trunc: bool,
    hex: bool,
    end: usize,
}

fn read_float(s: &[u8]) -> Option<ReadFloat> {
    let mut underscores = false;
    let mut i = 0;
    let mut neg = false;
    if i >= s.len() {
        return None;
    }
    match s[i] {
        b'+' => i += 1,
        b'-' => {
            i += 1;
            neg = true;
        }
        _ => {}
    }
    let mut base: u64 = 10;
    let mut max_mant_digits = 19;
    let mut exp_char = b'e';
    let mut hex = false;
    if i + 2 < s.len() && s[i] == b'0' && lower(s[i + 1]) == b'x' {
        base = 16;
        max_mant_digits = 16;
        i += 2;
        exp_char = b'p';
        hex = true;
    }
    let mut sawdot = false;
    let mut sawdigits = false;
    let mut nd: i64 = 0;
    let mut nd_mant: i64 = 0;
    let mut dp: i64 = 0;
    let mut mantissa: u64 = 0;
    let mut trunc = false;
    while i < s.len() {
        let c = s[i];
        if c == b'_' {
            underscores = true;
        } else if c == b'.' {
            if sawdot {
                break;
            }
            sawdot = true;
            dp = nd;
        } else if c.is_ascii_digit() {
            sawdigits = true;
            if c == b'0' && nd == 0 {
                dp -= 1;
                i += 1;
                continue;
            }
            nd += 1;
            if nd_mant < max_mant_digits {
                mantissa = mantissa * base + u64::from(c - b'0');
                nd_mant += 1;
            } else if c != b'0' {
                trunc = true;
            }
        } else if base == 16 && (b'a'..=b'f').contains(&lower(c)) {
            sawdigits = true;
            nd += 1;
            if nd_mant < max_mant_digits {
                mantissa = mantissa * 16 + u64::from(lower(c) - b'a' + 10);
                nd_mant += 1;
            } else {
                trunc = true;
            }
        } else {
            break;
        }
        i += 1;
    }
    if !sawdigits {
        return None;
    }
    if !sawdot {
        dp = nd;
    }
    if base == 16 {
        dp *= 4;
        nd_mant *= 4;
    }
    if i < s.len() && lower(s[i]) == exp_char {
        i += 1;
        if i >= s.len() {
            return None;
        }
        let mut esign = 1;
        match s[i] {
            b'+' => i += 1,
            b'-' => {
                i += 1;
                esign = -1;
            }
            _ => {}
        }
        if i >= s.len() || !s[i].is_ascii_digit() {
            return None;
        }
        let mut e: i64 = 0;
        while i < s.len() && (s[i].is_ascii_digit() || s[i] == b'_') {
            if s[i] == b'_' {
                underscores = true;
            } else if e < 10000 {
                e = e * 10 + i64::from(s[i] - b'0');
            }
            i += 1;
        }
        dp += e * esign;
    } else if base == 16 {
        return None;
    }
    let exp = if mantissa != 0 { dp - nd_mant } else { 0 };
    if underscores && !underscore_ok(&s[..i]) {
        return None;
    }
    Some(ReadFloat {
        mantissa,
        exp,
        neg,
        trunc,
        hex,
        end: i,
    })
}

fn atof_hex(bits: u32, mut mantissa: u64, mut exp: i64, neg: bool, trunc: bool) -> Option<f64> {
    let (mantbits, expbits, bias): (u32, u32, i64) = if bits == 32 {
        (23, 8, -127)
    } else {
        (52, 11, -1023)
    };
    let max_exp = (1i64 << expbits) + bias - 2;
    let min_exp = bias + 1;
    exp += i64::from(mantbits);
    while mantissa != 0 && mantissa >> (mantbits + 2) == 0 {
        mantissa <<= 1;
        exp -= 1;
    }
    if trunc {
        mantissa |= 1;
    }
    while mantissa >> (1 + mantbits + 2) != 0 {
        mantissa = mantissa >> 1 | mantissa & 1;
        exp += 1;
    }
    while mantissa > 1 && exp < min_exp - 2 {
        mantissa = mantissa >> 1 | mantissa & 1;
        exp += 1;
    }
    let mut round = mantissa & 3;
    mantissa >>= 2;
    round |= mantissa & 1;
    exp += 2;
    if round == 3 {
        mantissa += 1;
        if mantissa == 1 << (1 + mantbits) {
            mantissa >>= 1;
            exp += 1;
        }
    }
    if mantissa >> mantbits == 0 {
        exp = bias;
    }
    if exp > max_exp {
        return Some(if neg {
            f64::NEG_INFINITY
        } else {
            f64::INFINITY
        });
    }
    let mut out = mantissa & ((1 << mantbits) - 1);
    out |= (((exp - bias) as u64) & ((1 << expbits) - 1)) << mantbits;
    if neg {
        out |= 1 << mantbits << expbits;
    }
    Some(if bits == 32 {
        f64::from(f32::from_bits(out as u32))
    } else {
        f64::from_bits(out)
    })
}

/// Decodes one UTF-8 sequence the way Go's `utf8.DecodeRune` does: `None`
/// with width 1 for anything invalid (surrogates and overlong forms included).
pub(crate) fn decode_rune(s: &[u8]) -> (Option<char>, usize) {
    let len = match s.first() {
        None => return (None, 0),
        Some(b) if *b < 0x80 => return (Some(*b as char), 1),
        Some(0xc2..=0xdf) => 2,
        Some(0xe0..=0xef) => 3,
        Some(0xf0..=0xf4) => 4,
        Some(_) => return (None, 1),
    };
    match s
        .get(..len)
        .and_then(|b| std::str::from_utf8(b).ok())
        .and_then(|t| t.chars().next())
    {
        Some(c) => (Some(c), len),
        None => (None, 1),
    }
}

pub(crate) fn is_print(r: u32) -> bool {
    !NOT_PRINTABLE
        .binary_search_by(|&(lo, hi)| {
            if hi < r {
                std::cmp::Ordering::Less
            } else if lo > r {
                std::cmp::Ordering::Greater
            } else {
                std::cmp::Ordering::Equal
            }
        })
        .is_ok()
}

const LOWERHEX: &[u8; 16] = b"0123456789abcdef";

/// `strconv.Quote`.
pub(crate) fn quote(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut rest = s;
    while !rest.is_empty() {
        let (r, width) = decode_rune(rest);
        match r {
            None => {
                out.push_str("\\x");
                out.push(LOWERHEX[(rest[0] >> 4) as usize] as char);
                out.push(LOWERHEX[(rest[0] & 15) as usize] as char);
            }
            Some(c) => escape_rune(c, &mut out),
        }
        rest = &rest[width..];
    }
    out.push('"');
    out
}

fn escape_rune(c: char, out: &mut String) {
    let r = c as u32;
    if c == '"' || c == '\\' {
        out.push('\\');
        out.push(c);
        return;
    }
    if is_print(r) {
        out.push(c);
        return;
    }
    match c {
        '\x07' => out.push_str("\\a"),
        '\x08' => out.push_str("\\b"),
        '\x0c' => out.push_str("\\f"),
        '\n' => out.push_str("\\n"),
        '\r' => out.push_str("\\r"),
        '\t' => out.push_str("\\t"),
        '\x0b' => out.push_str("\\v"),
        _ if r < 0x20 || r == 0x7f => {
            out.push_str("\\x");
            out.push(LOWERHEX[(r >> 4) as usize] as char);
            out.push(LOWERHEX[(r & 15) as usize] as char);
        }
        _ if r < 0x10000 => {
            out.push_str("\\u");
            for shift in [12, 8, 4, 0] {
                out.push(LOWERHEX[((r >> shift) & 15) as usize] as char);
            }
        }
        _ => {
            out.push_str("\\U");
            for shift in [28, 24, 20, 16, 12, 8, 4, 0] {
                out.push(LOWERHEX[((r >> shift) & 15) as usize] as char);
            }
        }
    }
}

/// `base64.CorruptInputError`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct CorruptInput(pub(crate) usize);

impl fmt::Display for CorruptInput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "illegal base64 data at input byte {}", self.0)
    }
}

#[derive(Clone, Copy)]
pub(crate) enum Alphabet {
    Std,
    Url,
}

fn decode_char(alphabet: Alphabet, c: u8) -> Option<u8> {
    match c {
        b'A'..=b'Z' => Some(c - b'A'),
        b'a'..=b'z' => Some(c - b'a' + 26),
        b'0'..=b'9' => Some(c - b'0' + 52),
        b'+' if matches!(alphabet, Alphabet::Std) => Some(62),
        b'/' if matches!(alphabet, Alphabet::Std) => Some(63),
        b'-' if matches!(alphabet, Alphabet::Url) => Some(62),
        b'_' if matches!(alphabet, Alphabet::Url) => Some(63),
        _ => None,
    }
}

/// `base64.StdEncoding.DecodeString` / `URLEncoding.DecodeString`: padded,
/// not strict, `\r` and `\n` skipped.
pub(crate) fn base64_decode(alphabet: Alphabet, src: &[u8]) -> Result<Vec<u8>, CorruptInput> {
    let mut out = Vec::with_capacity(src.len() / 4 * 3);
    let mut si = 0;
    while si < src.len() {
        let mut dbuf = [0u8; 4];
        let mut dlen = 4;
        let mut j = 0;
        let mut trailing: Option<CorruptInput> = None;
        while j < 4 {
            if src.len() == si {
                if j == 0 {
                    return Ok(out);
                }
                return Err(CorruptInput(si - j));
            }
            let c = src[si];
            si += 1;
            if let Some(v) = decode_char(alphabet, c) {
                dbuf[j] = v;
                j += 1;
                continue;
            }
            if c == b'\n' || c == b'\r' {
                continue;
            }
            if c != b'=' {
                return Err(CorruptInput(si - 1));
            }
            match j {
                0 | 1 => return Err(CorruptInput(si - 1)),
                2 => {
                    while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                        si += 1;
                    }
                    if si == src.len() {
                        return Err(CorruptInput(src.len()));
                    }
                    if src[si] != b'=' {
                        return Err(CorruptInput(si - 1));
                    }
                    si += 1;
                }
                _ => {}
            }
            while si < src.len() && (src[si] == b'\n' || src[si] == b'\r') {
                si += 1;
            }
            if si < src.len() {
                trailing = Some(CorruptInput(si));
            }
            dlen = j;
            break;
        }
        let val = u32::from(dbuf[0]) << 18
            | u32::from(dbuf[1]) << 12
            | u32::from(dbuf[2]) << 6
            | u32::from(dbuf[3]);
        let bytes = [(val >> 16) as u8, (val >> 8) as u8, val as u8];
        out.extend_from_slice(&bytes[..dlen - 1]);
        if let Some(e) = trailing {
            return Err(e);
        }
    }
    Ok(out)
}

/// `runtime.Bytes`: standard encoding, then URL-safe; the URL-safe error wins.
pub(crate) fn runtime_bytes(val: &[u8]) -> Result<Vec<u8>, CorruptInput> {
    base64_decode(Alphabet::Std, val).or_else(|_| base64_decode(Alphabet::Url, val))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ints_follow_go() {
        assert_eq!(parse_int(b"0x1f", true, 32), Ok(31));
        assert_eq!(parse_int(b"017", true, 32), Ok(15));
        assert_eq!(parse_int(b"-0x80000000", true, 32), Ok(-2147483648));
        assert_eq!(
            parse_int(b"08", true, 32).unwrap_err().to_string(),
            r#"strconv.ParseInt: parsing "08": invalid syntax"#
        );
        assert_eq!(parse_uint(b"0x_ff", true, 32), Ok(255));
        assert!(parse_int(b"1_000", false, 32).is_err());
    }

    #[test]
    fn floats_follow_go() {
        assert_eq!(parse_float(b"0x1p-2", 64), Ok(0.25));
        assert_eq!(parse_float(b"1_0", 64), Ok(10.0));
        assert!(parse_float(b"1__0", 64).is_err());
        assert!(parse_float(b"infx", 64).is_err());
        assert_eq!(parse_float(b"-Infinity", 64), Ok(f64::NEG_INFINITY));
        assert_eq!(
            parse_float(b"3.5e38", 32).unwrap_err().to_string(),
            r#"strconv.ParseFloat: parsing "3.5e38": value out of range"#
        );
    }

    #[test]
    fn quote_escapes_like_go() {
        assert_eq!(quote(b"a\"\x07\xff\xc3\xa9"), r#""a\"\a\xffé""#);
        assert_eq!(quote("\u{85}".as_bytes()), concat!("\"\\", "u0085\""));
    }

    #[test]
    fn base64_errors_point_where_go_does() {
        assert_eq!(runtime_bytes(b"aGVsbG8"), Err(CorruptInput(4)));
        assert_eq!(runtime_bytes(b"_-8="), Ok(vec![0xff, 0xef]));
        assert_eq!(runtime_bytes(b"Y\nQ=="), Ok(b"a".to_vec()));
    }
}
