//! Go's `time.Parse(time.RFC3339Nano, …)` and `time.ParseDuration`, with the
//! error text grpc-gateway puts in its responses, and the two protojson
//! parsers `runtime.Timestamp` and `runtime.Duration` go through.

use super::strconv::{decode_rune, parse_int};

const LAYOUT: &str = "2006-01-02T15:04:05.999999999Z07:00";

/// Seconds since the Unix epoch and nanoseconds, as `t.Unix()` and
/// `t.Nanosecond()` give them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Instant {
    pub(crate) seconds: i64,
    pub(crate) nanos: i32,
}

/// `time`'s own `quote`, used in its error messages: every byte of a
/// non-ASCII or control rune is written as `\xNN`.
pub(crate) fn time_quote(s: &[u8]) -> String {
    let mut out = String::from("\"");
    let mut i = 0;
    while i < s.len() {
        let (r, width) = decode_rune(&s[i..]);
        match r {
            Some(c) if (c as u32) >= 0x20 && (c as u32) < 0x80 => {
                if c == '"' || c == '\\' {
                    out.push('\\');
                }
                out.push(c);
            }
            _ => {
                // Go escapes three bytes for a literal U+FFFD only when two
                // more bytes follow it, and one otherwise.
                let escaped = match r {
                    Some('\u{fffd}') if i + 2 < s.len() => 3,
                    Some('\u{fffd}') | None => 1,
                    Some(_) => width,
                };
                for b in &s[i..i + escaped] {
                    out.push_str(&format!("\\x{b:02x}"));
                }
            }
        }
        i += width;
    }
    out.push('"');
    out
}

fn parse_error(value: &[u8], layout_elem: &str, value_elem: &[u8]) -> String {
    format!(
        "parsing time {} as {}: cannot parse {} as {}",
        time_quote(value),
        time_quote(LAYOUT.as_bytes()),
        time_quote(value_elem),
        time_quote(layout_elem.as_bytes())
    )
}

fn range_error(value: &[u8], what: &str) -> String {
    format!("parsing time {}: {what} out of range", time_quote(value))
}

fn getnum(s: &[u8], fixed: bool) -> Option<(i64, &[u8])> {
    let digit = |i: usize| s.get(i).filter(|c| c.is_ascii_digit()).copied();
    let d0 = digit(0)?;
    match digit(1) {
        None if fixed => None,
        None => Some((i64::from(d0 - b'0'), &s[1..])),
        Some(d1) => Some((i64::from(d0 - b'0') * 10 + i64::from(d1 - b'0'), &s[2..])),
    }
}

fn is_leap(year: i64) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}

fn days_in(month: i64, year: i64) -> i64 {
    match month {
        2 if is_leap(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to a proleptic Gregorian date.
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

enum Std {
    LongYear,
    ZeroMonth,
    ZeroDay,
    Hour,
    ZeroMinute,
    ZeroSecond,
    FracSecond9,
    Iso8601ColonTz,
}

/// `time.Parse(time.RFC3339Nano, value)`. Its fast path accepts a subset of
/// what the general parser accepts, with the same result, so only the general
/// parser is written.
pub(crate) fn parse_rfc3339_nano(value: &[u8]) -> Result<Instant, String> {
    const CHUNKS: [(&str, Std, &str); 8] = [
        ("", Std::LongYear, "2006"),
        ("-", Std::ZeroMonth, "01"),
        ("-", Std::ZeroDay, "02"),
        ("T", Std::Hour, "15"),
        (":", Std::ZeroMinute, "04"),
        (":", Std::ZeroSecond, "05"),
        ("", Std::FracSecond9, ".999999999"),
        ("", Std::Iso8601ColonTz, "Z07:00"),
    ];
    let avalue = value;
    let mut v = value;
    let (mut year, mut month, mut day, mut hour, mut min, mut sec, mut nsec) =
        (0i64, -1i64, -1i64, 0i64, 0i64, 0i64, 0i64);
    let mut zone_offset: Option<i64> = None;
    for (prefix, std, stdstr) in CHUNKS {
        let Some(rest) = v.strip_prefix(prefix.as_bytes()) else {
            // `skip` stops at the first differing byte.
            let common = prefix
                .bytes()
                .zip(v.iter())
                .take_while(|(a, b)| a == *b)
                .count();
            return Err(parse_error(avalue, prefix, &v[common..]));
        };
        v = rest;
        let hold = v;
        let mut range_err: Option<&str> = None;
        let mut bad = false;
        match std {
            Std::LongYear => {
                if v.len() < 4 || !v[0].is_ascii_digit() {
                    bad = true;
                } else {
                    match parse_int(&v[..4], false, 64) {
                        Ok(y) if v[..4].iter().all(u8::is_ascii_digit) => year = y,
                        _ => bad = true,
                    }
                    v = &v[4..];
                }
            }
            Std::ZeroMonth | Std::ZeroDay | Std::ZeroMinute | Std::ZeroSecond | Std::Hour => {
                let fixed = !matches!(std, Std::Hour);
                match getnum(v, fixed) {
                    None => bad = true,
                    Some((n, rest)) => {
                        v = rest;
                        match std {
                            Std::ZeroMonth => {
                                month = n;
                                if !(1..=12).contains(&n) {
                                    range_err = Some("month");
                                }
                            }
                            Std::ZeroDay => day = n,
                            Std::Hour => {
                                hour = n;
                                if n >= 24 {
                                    range_err = Some("hour");
                                }
                            }
                            Std::ZeroMinute => {
                                min = n;
                                if n >= 60 {
                                    range_err = Some("minute");
                                }
                            }
                            _ => {
                                sec = n;
                                if n >= 60 {
                                    range_err = Some("second");
                                }
                            }
                        }
                    }
                }
            }
            Std::FracSecond9 => {
                if v.len() >= 2 && (v[0] == b'.' || v[0] == b',') && v[1].is_ascii_digit() {
                    let mut i = 0;
                    while i + 1 < v.len() && v[i + 1].is_ascii_digit() {
                        i += 1;
                    }
                    let nbytes = (1 + i).min(10);
                    let digits = &v[1..nbytes];
                    let mut ns: i64 = digits
                        .iter()
                        .fold(0, |acc, d| acc * 10 + i64::from(d - b'0'));
                    for _ in 0..10 - nbytes {
                        ns *= 10;
                    }
                    nsec = ns;
                    v = &v[1 + i..];
                }
            }
            Std::Iso8601ColonTz => {
                if v.first() == Some(&b'Z') {
                    v = &v[1..];
                    zone_offset = Some(0);
                } else if v.len() < 6 || v[3] != b':' {
                    bad = true;
                } else {
                    let (sign, hh, mm) = (v[0], &v[1..3], &v[4..6]);
                    v = &v[6..];
                    let hr = getnum(hh, true);
                    let mi = hr.and_then(|_| getnum(mm, true));
                    let (hr, mi) = match (hr, mi) {
                        (Some((h, _)), Some((m, _))) => (h, m),
                        (Some((h, _)), None) => {
                            bad = true;
                            (h, 0)
                        }
                        _ => {
                            bad = true;
                            (0, 0)
                        }
                    };
                    if hr > 24 {
                        range_err = Some("time zone offset hour");
                    }
                    if mi > 60 {
                        range_err = Some("time zone offset minute");
                    }
                    let offset = (hr * 60 + mi) * 60;
                    match sign {
                        b'+' => zone_offset = Some(offset),
                        b'-' => zone_offset = Some(-offset),
                        _ => bad = true,
                    }
                }
            }
        }
        if let Some(what) = range_err {
            return Err(range_error(avalue, what));
        }
        if bad {
            return Err(parse_error(avalue, stdstr, hold));
        }
    }
    if !v.is_empty() {
        return Err(format!(
            "parsing time {}: extra text: {}",
            time_quote(avalue),
            time_quote(v)
        ));
    }
    if month < 0 {
        month = 1;
    }
    if day < 0 {
        day = 1;
    }
    if day < 1 || day > days_in(month, year) {
        return Err(format!(
            "parsing time {}: day out of range",
            time_quote(avalue)
        ));
    }
    let local = days_from_civil(year, month, day) * 86400 + hour * 3600 + min * 60 + sec;
    Ok(Instant {
        seconds: local - zone_offset.unwrap_or(0),
        nanos: nsec as i32,
    })
}

const MIN_TIMESTAMP_SECONDS: i64 = -62135596800;
const MAX_TIMESTAMP_SECONDS: i64 = 253402300799;

/// `timestamppb.Timestamp.IsValid`.
pub(crate) fn timestamp_is_valid(t: Instant) -> bool {
    (MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&t.seconds)
        && (0..1_000_000_000).contains(&t.nanos)
}

/// `time.ParseDuration`, as nanoseconds.
pub(crate) fn parse_duration(orig: &[u8]) -> Result<i64, String> {
    let invalid = || format!("time: invalid duration {}", time_quote(orig));
    let mut s = orig;
    let mut d: u64 = 0;
    let mut neg = false;
    if let Some(&c) = s.first()
        && (c == b'-' || c == b'+')
    {
        neg = c == b'-';
        s = &s[1..];
    }
    if s == b"0" {
        return Ok(0);
    }
    if s.is_empty() {
        return Err(invalid());
    }
    while !s.is_empty() {
        if !(s[0] == b'.' || s[0].is_ascii_digit()) {
            return Err(invalid());
        }
        let pl = s.len();
        let mut v: u64 = 0;
        let mut i = 0;
        while i < s.len() && s[i].is_ascii_digit() {
            if v > (1 << 63) / 10 {
                return Err(invalid());
            }
            v = v * 10 + u64::from(s[i] - b'0');
            if v > 1 << 63 {
                return Err(invalid());
            }
            i += 1;
        }
        s = &s[i..];
        let pre = pl != s.len();
        let mut post = false;
        let mut f: u64 = 0;
        let mut scale: f64 = 1.0;
        if s.first() == Some(&b'.') {
            s = &s[1..];
            let pl = s.len();
            let mut overflow = false;
            let mut i = 0;
            while i < s.len() && s[i].is_ascii_digit() {
                if !overflow {
                    if f > ((1u64 << 63) - 1) / 10 {
                        overflow = true;
                    } else {
                        let y = f * 10 + u64::from(s[i] - b'0');
                        if y > 1 << 63 {
                            overflow = true;
                        } else {
                            f = y;
                            scale *= 10.0;
                        }
                    }
                }
                i += 1;
            }
            s = &s[i..];
            post = pl != s.len();
        }
        if !pre && !post {
            return Err(invalid());
        }
        let i = s
            .iter()
            .position(|&c| c == b'.' || c.is_ascii_digit())
            .unwrap_or(s.len());
        if i == 0 {
            return Err(format!(
                "time: missing unit in duration {}",
                time_quote(orig)
            ));
        }
        let u = &s[..i];
        s = &s[i..];
        let unit: u64 = match u {
            b"ns" => 1,
            // Also "\u{b5}s" and "\u{3bc}s", the two micro signs.
            b"us" | b"\xc2\xb5s" | b"\xce\xbcs" => 1_000,
            b"ms" => 1_000_000,
            b"s" => 1_000_000_000,
            b"m" => 60 * 1_000_000_000,
            b"h" => 3600 * 1_000_000_000,
            _ => {
                return Err(format!(
                    "time: unknown unit {} in duration {}",
                    time_quote(u),
                    time_quote(orig)
                ));
            }
        };
        if v > (1 << 63) / unit {
            return Err(invalid());
        }
        v *= unit;
        if f > 0 {
            v += (f as f64 * (unit as f64 / scale)) as u64;
            if v > 1 << 63 {
                return Err(invalid());
            }
        }
        d += v;
        if d > 1 << 63 {
            return Err(invalid());
        }
    }
    if neg {
        return Ok((d as i64).wrapping_neg());
    }
    if d > (1 << 63) - 1 {
        return Err(invalid());
    }
    Ok(d as i64)
}

/// What protojson's JSON reader makes of the string `runtime.Timestamp` and
/// `runtime.Duration` build with `strconv.Quote(strings.Trim(val, "\""))`:
/// the unescaped value, or the syntax error for an escape JSON lacks.
fn protojson_string(quoted: &str) -> Result<Vec<u8>, String> {
    let b = quoted.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 1;
    while i < b.len() - 1 {
        if b[i] != b'\\' {
            out.push(b[i]);
            i += 1;
            continue;
        }
        let esc = b[i + 1];
        let simple = match esc {
            b'"' => Some(b'"'),
            b'\\' => Some(b'\\'),
            b'/' => Some(b'/'),
            b'b' => Some(8),
            b'f' => Some(12),
            b'n' => Some(b'\n'),
            b'r' => Some(b'\r'),
            b't' => Some(b'\t'),
            _ => None,
        };
        if let Some(c) = simple {
            out.push(c);
            i += 2;
            continue;
        }
        if esc == b'u' {
            let hex = std::str::from_utf8(&b[i + 2..i + 6]).expect("Quote writes ASCII escapes");
            let r = u32::from_str_radix(hex, 16).expect("Quote writes hex");
            let c = char::from_u32(r).unwrap_or('\u{fffd}');
            let mut buf = [0u8; 4];
            out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
            i += 6;
            continue;
        }
        return Err(format!(
            "proto: syntax error (line 1:1): invalid escape code {} in string",
            super::strconv::quote(&b[i..i + 2])
        ));
    }
    Ok(out)
}

fn trim_quotes(val: &[u8]) -> &[u8] {
    let start = val.iter().position(|&c| c != b'"').unwrap_or(val.len());
    let end = val
        .iter()
        .rposition(|&c| c != b'"')
        .map_or(start, |i| i + 1);
    &val[start..end]
}

/// `runtime.Timestamp`.
pub(crate) fn runtime_timestamp(val: &[u8]) -> Result<Instant, String> {
    let quoted = super::strconv::quote(trim_quotes(val));
    let s = protojson_string(&quoted)?;
    let invalid = || format!("proto: (line 1:1): invalid google.protobuf.Timestamp value {quoted}");
    let t = parse_rfc3339_nano(&s).map_err(|_| invalid())?;
    if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&t.seconds) {
        return Err(format!(
            "proto: (line 1:1): google.protobuf.Timestamp value out of range: {quoted}"
        ));
    }
    let dot = s.iter().rposition(|&c| c == b'.');
    let zone = s.iter().rposition(|&c| matches!(c, b'Z' | b'-' | b'+'));
    if let (Some(i), Some(j)) = (dot, zone)
        && j >= i
        && j - i > ".999999999".len()
    {
        return Err(invalid());
    }
    Ok(t)
}

const MAX_SECONDS_IN_DURATION: i64 = 315576000000;

/// `runtime.Duration`, as seconds and nanoseconds.
pub(crate) fn runtime_duration(val: &[u8]) -> Result<(i64, i32), String> {
    let quoted = super::strconv::quote(trim_quotes(val));
    let s = protojson_string(&quoted)?;
    let Some((secs, nanos)) = protojson_parse_duration(&s) else {
        return Err(format!(
            "proto: (line 1:1): invalid google.protobuf.Duration value {quoted}"
        ));
    };
    if !(-MAX_SECONDS_IN_DURATION..=MAX_SECONDS_IN_DURATION).contains(&secs) {
        return Err(format!(
            "proto: (line 1:1): google.protobuf.Duration value out of range: {quoted}"
        ));
    }
    Ok((secs, nanos))
}

fn protojson_parse_duration(input: &[u8]) -> Option<(i64, i32)> {
    if input.len() < 2 || *input.last()? != b's' {
        return None;
    }
    let mut b = &input[..input.len() - 1];
    let mut neg = false;
    match b[0] {
        b'-' => {
            neg = true;
            b = &b[1..];
        }
        b'+' => b = &b[1..],
        _ => {}
    }
    if b.is_empty() {
        return None;
    }
    let mut intp: &[u8] = &[];
    match b[0] {
        b'0' => b = &b[1..],
        b'1'..=b'9' => {
            let n = b.iter().take_while(|c| c.is_ascii_digit()).count();
            intp = &b[..n];
            b = &b[n..];
        }
        b'.' => {}
        _ => return None,
    }
    let mut frac: Option<[u8; 9]> = None;
    if !b.is_empty() {
        if b[0] != b'.' {
            return None;
        }
        b = &b[1..];
        let mut digits = [b'0'; 9];
        let mut n = 0;
        while !b.is_empty() && n < 9 && b[0].is_ascii_digit() {
            digits[n] = b[0];
            n += 1;
            b = &b[1..];
        }
        if !b.is_empty() {
            return None;
        }
        frac = Some(digits);
    }
    let mut secs = 0i64;
    if !intp.is_empty() {
        secs = parse_int(intp, false, 64).ok()?;
    }
    let mut nanos = 0i64;
    if let Some(digits) = frac {
        let start = digits.iter().position(|&c| c != b'0');
        if let Some(start) = start {
            nanos = parse_int(&digits[start..], false, 32).ok()?;
        }
    }
    if neg {
        if secs > 0 {
            secs = -secs;
        }
        if nanos > 0 {
            nanos = -nanos;
        }
    }
    Some((secs, nanos as i32))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rfc3339_like_go() {
        assert_eq!(
            parse_rfc3339_nano(b"1970-01-01T00:00:01.5+00:00"),
            Ok(Instant {
                seconds: 1,
                nanos: 500_000_000
            })
        );
        assert_eq!(
            parse_rfc3339_nano(b"2024-01-02T03:04:05.Z").unwrap_err(),
            r#"parsing time "2024-01-02T03:04:05.Z" as "2006-01-02T15:04:05.999999999Z07:00": cannot parse ".Z" as "Z07:00""#
        );
        assert_eq!(
            parse_rfc3339_nano(b"2024-01-02t03:04:05z").unwrap_err(),
            r#"parsing time "2024-01-02t03:04:05z" as "2006-01-02T15:04:05.999999999Z07:00": cannot parse "t03:04:05z" as "T""#
        );
    }

    #[test]
    fn durations_like_go() {
        assert_eq!(parse_duration(b"1h30m"), Ok(5_400_000_000_000));
        assert_eq!(
            parse_duration(b"5").unwrap_err(),
            r#"time: missing unit in duration "5""#
        );
        assert_eq!(runtime_duration(b"-0.000000001s"), Ok((0, -1)));
    }
}
