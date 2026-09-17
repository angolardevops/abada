//! What grpc-gateway's `runtime.JSONPb` does around protojson: the
//! `encoding/json` `Decoder` that reads a request body, and the
//! `encoding/json` paths it takes for a `body` or `response_body` field that
//! is not a message (`decodeNonProtoField`, `marshalNonProtoField` in
//! `runtime/marshal_jsonpb.go`). Those depend on the Go type protoc-gen-go
//! generates for the field, which is derived here from the descriptor.

use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MapKey, MessageDescriptor, ReflectMessage, Value,
};

use super::encode::{Encoder, enum_name};
use super::{JsonError, Marshaler, go, go_has};

// ------------------------------------------------ encoding/json's scanner

const MAX_NESTING: usize = 10_000;

fn is_space(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n')
}

fn syntax(offset: usize, what: &str) -> JsonError {
    JsonError::syntax(format!("{what} (offset {offset})"))
}

/// The first JSON value of `body`, as `json.Decoder.Decode` reads it into a
/// `json.RawMessage`: leading space skipped, the value checked by
/// `encoding/json`'s scanner, whatever follows it ignored. `None` for a body
/// that is empty or only space (`io.EOF`).
pub(crate) fn first_value(body: &[u8]) -> Result<Option<&[u8]>, JsonError> {
    let start = body
        .iter()
        .position(|&c| !is_space(c))
        .unwrap_or(body.len());
    if start == body.len() {
        return Ok(None);
    }
    let end = scan_value(body, start)?;
    Ok(Some(&body[start..end]))
}

/// The end of the value starting at `i`, which is not space.
fn scan_value(s: &[u8], mut i: usize) -> Result<usize, JsonError> {
    // Containers are tracked on an explicit stack: `encoding/json` allows
    // 10 000 levels, which recursion here could not.
    let mut stack: Vec<u8> = Vec::new();
    loop {
        // A value.
        i = skip_space(s, i);
        let Some(&c) = s.get(i) else {
            return Err(JsonError::eof());
        };
        match c {
            b'{' | b'[' => {
                stack.push(c);
                if stack.len() > MAX_NESTING {
                    return Err(syntax(i, "exceeded max depth"));
                }
                i = skip_space(s, i + 1);
                let close = if c == b'{' { b'}' } else { b']' };
                match s.get(i) {
                    None => return Err(JsonError::eof()),
                    Some(&x) if x == close => {
                        stack.pop();
                        i += 1;
                    }
                    Some(_) if c == b'{' => {
                        i = object_key(s, i)?;
                        continue;
                    }
                    Some(_) => continue,
                }
            }
            b'"' => i = scan_string(s, i)?,
            b'-' | b'0'..=b'9' => i = scan_number(s, i)?,
            b't' => i = scan_literal(s, i, b"true")?,
            b'f' => i = scan_literal(s, i, b"false")?,
            b'n' => i = scan_literal(s, i, b"null")?,
            _ => {
                return Err(syntax(
                    i,
                    "invalid character looking for beginning of value",
                ));
            }
        }
        // After a value: close containers or go to the next element.
        loop {
            let Some(&top) = stack.last() else {
                return Ok(i);
            };
            i = skip_space(s, i);
            let Some(&c) = s.get(i) else {
                return Err(JsonError::eof());
            };
            match (top, c) {
                (b'{', b'}') | (b'[', b']') => {
                    stack.pop();
                    i += 1;
                }
                (b'{', b',') => {
                    i = object_key(s, skip_space(s, i + 1))?;
                    break;
                }
                (b'[', b',') => {
                    i += 1;
                    break;
                }
                _ => return Err(syntax(i, "invalid character after value")),
            }
        }
    }
}

fn skip_space(s: &[u8], mut i: usize) -> usize {
    while i < s.len() && is_space(s[i]) {
        i += 1;
    }
    i
}

/// A key and its colon; returns the position of the value.
fn object_key(s: &[u8], i: usize) -> Result<usize, JsonError> {
    match s.get(i) {
        None => return Err(JsonError::eof()),
        Some(b'"') => {}
        Some(_) => {
            return Err(syntax(
                i,
                "invalid character looking for beginning of object key",
            ));
        }
    }
    let i = skip_space(s, scan_string(s, i)?);
    match s.get(i) {
        None => Err(JsonError::eof()),
        Some(b':') => Ok(i + 1),
        Some(_) => Err(syntax(i, "invalid character after object key")),
    }
}

fn scan_string(s: &[u8], mut i: usize) -> Result<usize, JsonError> {
    i += 1;
    loop {
        let Some(&c) = s.get(i) else {
            return Err(JsonError::eof());
        };
        match c {
            b'"' => return Ok(i + 1),
            b'\\' => {
                let Some(&e) = s.get(i + 1) else {
                    return Err(JsonError::eof());
                };
                match e {
                    b'b' | b'f' | b'n' | b'r' | b't' | b'\\' | b'/' | b'"' => i += 2,
                    b'u' => {
                        for k in 0..4 {
                            match s.get(i + 2 + k) {
                                None => return Err(JsonError::eof()),
                                Some(h) if h.is_ascii_hexdigit() => {}
                                Some(_) => {
                                    return Err(syntax(i, "invalid character in \\u escape"));
                                }
                            }
                        }
                        i += 6;
                    }
                    _ => return Err(syntax(i, "invalid character in string escape code")),
                }
            }
            0..=0x1f => return Err(syntax(i, "invalid character in string literal")),
            _ => i += 1,
        }
    }
}

fn scan_number(s: &[u8], mut i: usize) -> Result<usize, JsonError> {
    let digit = |i: usize| s.get(i).is_some_and(|c| c.is_ascii_digit());
    // Where the number cannot end: at the end of the input it is truncated,
    // anywhere else it is invalid.
    let need = |i: usize| {
        if i >= s.len() {
            JsonError::eof()
        } else {
            syntax(i, "invalid character in numeric literal")
        }
    };
    if s[i] == b'-' {
        i += 1;
    }
    match s.get(i) {
        Some(b'0') => i += 1,
        Some(b'1'..=b'9') => {
            while digit(i) {
                i += 1;
            }
        }
        _ => return Err(need(i)),
    }
    if s.get(i) == Some(&b'.') {
        i += 1;
        if !digit(i) {
            return Err(need(i));
        }
        while digit(i) {
            i += 1;
        }
    }
    if matches!(s.get(i), Some(b'e' | b'E')) {
        i += 1;
        if matches!(s.get(i), Some(b'+' | b'-')) {
            i += 1;
        }
        if !digit(i) {
            return Err(need(i));
        }
        while digit(i) {
            i += 1;
        }
    }
    Ok(i)
}

fn scan_literal(s: &[u8], i: usize, lit: &[u8]) -> Result<usize, JsonError> {
    for (k, &want) in lit.iter().enumerate() {
        match s.get(i + k) {
            None => return Err(JsonError::eof()),
            Some(&c) if c == want => {}
            Some(_) => return Err(syntax(i + k, "invalid character in literal")),
        }
    }
    Ok(i + lit.len())
}

/// The elements of an array that already passed the scanner.
fn array_items(raw: &[u8]) -> Vec<&[u8]> {
    let mut items = Vec::new();
    let mut i = skip_space(raw, 1);
    if raw.get(i) == Some(&b']') {
        return items;
    }
    loop {
        let end = scan_value(raw, i).expect("already scanned");
        items.push(&raw[i..end]);
        i = skip_space(raw, end);
        if raw[i] == b']' {
            return items;
        }
        i = skip_space(raw, i + 1);
    }
}

/// The members of an object that already passed the scanner: the key
/// literal (with quotes) and the value.
fn object_members(raw: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut members = Vec::new();
    let mut i = skip_space(raw, 1);
    if raw.get(i) == Some(&b'}') {
        return members;
    }
    loop {
        let key_end = scan_string(raw, i).expect("already scanned");
        let key = &raw[i..key_end];
        let v = skip_space(raw, skip_space(raw, key_end) + 1);
        let end = scan_value(raw, v).expect("already scanned");
        members.push((key, &raw[v..end]));
        i = skip_space(raw, end);
        if raw[i] == b'}' {
            return members;
        }
        i = skip_space(raw, i + 1);
    }
}

/// `unquoteBytes`: escapes resolved, invalid UTF-8 and unpaired surrogates
/// replaced by U+FFFD.
fn unquote(literal: &[u8]) -> String {
    let s = &literal[1..literal.len() - 1];
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let hex4 = |i: usize| -> Option<u32> {
        let h = s.get(i..i + 6)?;
        if h[0] != b'\\' || h[1] != b'u' {
            return None;
        }
        u32::from_str_radix(std::str::from_utf8(&h[2..]).ok()?, 16).ok()
    };
    while i < s.len() {
        let c = s[i];
        if c != b'\\' {
            let end = s[i..]
                .iter()
                .position(|&c| c == b'\\')
                .map_or(s.len(), |p| i + p);
            go::push_lossy(&mut out, &s[i..end]);
            i = end;
            continue;
        }
        match s[i + 1] {
            b'u' => {
                let r = hex4(i).expect("scanned escape");
                i += 6;
                if (0xd800..0xe000).contains(&r) {
                    if let Some(lo) = hex4(i) {
                        if (0xd800..0xdc00).contains(&r) && (0xdc00..0xe000).contains(&lo) {
                            out.push(
                                char::from_u32(0x10000 + ((r - 0xd800) << 10) + (lo - 0xdc00))
                                    .expect("valid pair"),
                            );
                            i += 6;
                            continue;
                        }
                    }
                    out.push('\u{fffd}');
                } else {
                    out.push(char::from_u32(r).expect("not a surrogate"));
                }
            }
            e => {
                out.push(match e {
                    b'b' => '\u{8}',
                    b'f' => '\u{c}',
                    b'n' => '\n',
                    b'r' => '\r',
                    b't' => '\t',
                    other => other as char,
                });
                i += 2;
            }
        }
    }
    out
}

// ------------------------------------------- body: "<field>" (decoding)

/// The Go type protoc-gen-go gives a field's element.
#[derive(Clone)]
enum Elem {
    /// `*M`.
    Message(MessageDescriptor),
    /// `[]byte`.
    Bytes,
    /// A generated enum type.
    Enum,
    /// `string`, `bool`, an integer or a float.
    Scalar(Kind),
}

fn elem(kind: &Kind) -> Elem {
    match kind {
        Kind::Message(m) => Elem::Message(m.clone()),
        Kind::Bytes => Elem::Bytes,
        Kind::Enum(_) => Elem::Enum,
        other => Elem::Scalar(other.clone()),
    }
}

fn type_error(what: &str) -> JsonError {
    JsonError::invalid(format!("json: cannot unmarshal {what}"))
}

/// `literalStore` into a Go string, bool, integer or float: `None` for
/// `null`, which leaves the target unchanged.
fn store_scalar(raw: &[u8], kind: &Kind) -> Result<Option<Value>, JsonError> {
    let what = || {
        type_error(&format!(
            "{} into a {kind:?} field",
            String::from_utf8_lossy(raw)
        ))
    };
    Ok(Some(match (raw[0], kind) {
        (b'n', _) => return Ok(None),
        (b't' | b'f', Kind::Bool) => Value::Bool(raw[0] == b't'),
        (b'"', Kind::String) => Value::String(unquote(raw)),
        (b'-' | b'0'..=b'9', _) => match kind {
            Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => go::parse_int(raw, 10, 64)
                .and_then(|n| i32::try_from(n).ok())
                .map(Value::I32)
                .ok_or_else(what)?,
            Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => go::parse_int(raw, 10, 64)
                .map(Value::I64)
                .ok_or_else(what)?,
            Kind::Uint32 | Kind::Fixed32 => go::parse_uint(raw, 10, 64)
                .and_then(|n| u32::try_from(n).ok())
                .map(Value::U32)
                .ok_or_else(what)?,
            Kind::Uint64 | Kind::Fixed64 => go::parse_uint(raw, 10, 64)
                .map(Value::U64)
                .ok_or_else(what)?,
            Kind::Float => go::parse_float(raw, 32)
                .map(|v| Value::F32(v as f32))
                .ok_or_else(what)?,
            Kind::Double => go::parse_float(raw, 64).map(Value::F64).ok_or_else(what)?,
            _ => return Err(what()),
        },
        _ => return Err(what()),
    }))
}

/// `[]byte` from `encoding/json`: strict padded standard base64 from a
/// string, or an array of numbers. `None` for `null`.
fn store_bytes(raw: &[u8]) -> Result<Option<Vec<u8>>, JsonError> {
    match raw[0] {
        b'n' => Ok(None),
        b'"' => go::Base64 {
            url: false,
            padding: true,
        }
        .decode(unquote(raw).as_bytes())
        .map(Some)
        .ok_or_else(|| JsonError::invalid("illegal base64 data")),
        b'[' => {
            let mut out = Vec::new();
            for item in array_items(raw) {
                match item[0] {
                    b'n' => out.push(0),
                    b'-' | b'0'..=b'9' => out.push(
                        go::parse_uint(item, 10, 64)
                            .and_then(|n| u8::try_from(n).ok())
                            .ok_or_else(|| type_error("number into a byte"))?,
                    ),
                    _ => return Err(type_error("value into a byte")),
                }
            }
            Ok(Some(out))
        }
        _ => Err(type_error("value into []byte")),
    }
}

/// A generated enum through `decodeNonProtoField`: decoded into an
/// `interface{}`, then `int32(v)` of the float64 — which on amd64 is
/// `-2147483648` when `v` is out of range.
fn store_enum(raw: &[u8]) -> Result<Value, JsonError> {
    match raw[0] {
        b'-' | b'0'..=b'9' => {
            let v = go::parse_float(raw, 64).ok_or_else(|| type_error("number"))?;
            let n = if v > -2_147_483_649.0 && v < 2_147_483_648.0 {
                v.trunc() as i32
            } else {
                i32::MIN
            };
            Ok(Value::EnumNumber(n))
        }
        b'"' => Err(JsonError::invalid(
            "unmarshaling of symbolic enum not supported",
        )),
        _ => Err(type_error("value into an enum")),
    }
}

/// The value of a freshly allocated Go variable: zero, not the proto2
/// default, and an empty message.
fn zero(kind: &Kind) -> Value {
    match kind {
        Kind::Enum(_) => Value::EnumNumber(0),
        Kind::Message(md) => Value::Message(DynamicMessage::new(md.clone())),
        other => Value::default_value(other),
    }
}

/// `unmarshalJSONPb` of one list element or map value into a new Go value.
fn element(marshaler: &Marshaler, raw: &[u8], e: &Elem) -> Result<Value, JsonError> {
    Ok(match e {
        Elem::Message(md) => {
            let mut m = DynamicMessage::new(md.clone());
            marshaler.unmarshal_into(&mut m, raw)?;
            Value::Message(m)
        }
        Elem::Bytes => Value::Bytes(store_bytes(raw)?.unwrap_or_default().into()),
        Elem::Enum => {
            if raw[0] == b'n' {
                return Err(type_error("null into an enum"));
            }
            store_enum(raw)?
        }
        Elem::Scalar(kind) => match store_scalar(raw, kind)? {
            Some(v) => v,
            None => zero(kind),
        },
    })
}

/// A map key through grpc-gateway's `convFromType` (`runtime.Int32` and
/// friends): `strconv` with base 0, so `0x10` and `1_000` are numbers.
fn map_key(key: &str, kind: &Kind) -> Result<MapKey, JsonError> {
    let b = key.as_bytes();
    let bad = || JsonError::invalid(format!("invalid map key {key:?}"));
    Ok(match kind {
        Kind::String => MapKey::String(key.to_owned()),
        Kind::Bool => MapKey::Bool(go::parse_bool(b).ok_or_else(bad)?),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            MapKey::I32(go::parse_int(b, 0, 32).ok_or_else(bad)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            MapKey::I64(go::parse_int(b, 0, 64).ok_or_else(bad)?)
        }
        Kind::Uint32 | Kind::Fixed32 => {
            MapKey::U32(go::parse_uint(b, 0, 32).ok_or_else(bad)? as u32)
        }
        Kind::Uint64 | Kind::Fixed64 => MapKey::U64(go::parse_uint(b, 0, 64).ok_or_else(bad)?),
        _ => return Err(JsonError::unsupported("map key kind")),
    })
}

fn is_real_oneof(field: &FieldDescriptor) -> bool {
    field.containing_oneof().is_some_and(|o| !o.is_synthetic())
}

pub(crate) fn decode_field(
    marshaler: &Marshaler,
    msg: &mut DynamicMessage,
    field: &FieldDescriptor,
    body: &[u8],
) -> Result<(), JsonError> {
    if is_real_oneof(field) {
        return Err(JsonError::unsupported(
            "a body field inside a oneof is not supported",
        ));
    }
    let kind = field.kind();
    let pointer = !field.is_list()
        && !field.is_map()
        && (matches!(kind, Kind::Message(_))
            || field.supports_presence() && !matches!(kind, Kind::Bytes));
    // `decodeNonProtoField` allocates a nil pointer before it reads: the
    // field is present afterwards whatever the body holds.
    if pointer && !msg.has_field(field) {
        msg.set_field(field, zero(&kind));
    }
    let Some(raw) = first_value(body)? else {
        return Ok(());
    };
    if field.is_map() {
        let Kind::Message(entry) = &kind else {
            unreachable!("map entry")
        };
        match raw[0] {
            b'n' => return Ok(()),
            b'{' => {}
            _ => return Err(type_error("value into a map")),
        }
        let key_kind = entry.map_entry_key_field().kind();
        let value_elem = elem(&entry.map_entry_value_field().kind());
        for (key, value) in object_members(raw) {
            let key = map_key(&unquote(key), &key_kind)?;
            let value = element(marshaler, value, &value_elem)?;
            msg.get_field_mut(field)
                .as_map_mut()
                .expect("map")
                .insert(key, value);
        }
        return Ok(());
    }
    if field.is_list() {
        match raw[0] {
            b'n' => return Ok(()),
            b'[' => {}
            _ => return Err(type_error("value into a slice")),
        }
        msg.get_field_mut(field)
            .as_list_mut()
            .expect("list")
            .clear();
        let e = elem(&kind);
        for item in array_items(raw) {
            let value = element(marshaler, item, &e)?;
            msg.get_field_mut(field)
                .as_list_mut()
                .expect("list")
                .push(value);
        }
        return Ok(());
    }
    match elem(&kind) {
        Elem::Message(md) => {
            let mut sub = DynamicMessage::new(md);
            let result = marshaler.unmarshal_into(&mut sub, raw);
            msg.set_field(field, Value::Message(sub));
            result
        }
        Elem::Bytes => {
            if let Some(bytes) = store_bytes(raw)? {
                msg.set_field(field, Value::Bytes(bytes.into()));
            }
            Ok(())
        }
        Elem::Enum => {
            msg.set_field(field, store_enum(raw)?);
            Ok(())
        }
        Elem::Scalar(kind) => {
            match store_scalar(raw, &kind)? {
                Some(v) => msg.set_field(field, v),
                // `json.Unmarshal` of `null` into the `**T` sets the `*T`
                // back to nil: the allocation above is undone.
                None if pointer => msg.clear_field(field),
                None => {}
            }
            Ok(())
        }
    }
}

// ------------------------------------ response_body: "<field>" (encoding)

/// `json.Marshal` of a Go scalar with HTML escaping.
fn marshal_scalar(out: &mut Vec<u8>, value: &Value) -> Result<(), JsonError> {
    use std::io::Write;
    match value {
        Value::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
        Value::String(s) => go::append_encoding_json_string(out, s),
        Value::I32(v) => {
            let _ = write!(out, "{v}");
        }
        Value::I64(v) => {
            let _ = write!(out, "{v}");
        }
        Value::U32(v) => {
            let _ = write!(out, "{v}");
        }
        Value::U64(v) => {
            let _ = write!(out, "{v}");
        }
        Value::F32(v) => float(out, *v as f64, 32)?,
        Value::F64(v) => float(out, *v, 64)?,
        Value::Bytes(b) => {
            out.push(b'"');
            go::append_base64(out, b);
            out.push(b'"');
        }
        _ => unreachable!("not a scalar"),
    }
    Ok(())
}

fn float(out: &mut Vec<u8>, v: f64, bits: u32) -> Result<(), JsonError> {
    if !v.is_finite() {
        return Err(JsonError::marshal(format!(
            "json: unsupported value: {}",
            if v.is_nan() {
                "NaN"
            } else if v > 0.0 {
                "+Inf"
            } else {
                "-Inf"
            }
        )));
    }
    go::append_float(out, v, bits);
    Ok(())
}

/// A generated enum's `String()`: the first name declared for the number,
/// or the number.
fn enum_string(kind: &Kind, n: i32) -> String {
    let Kind::Enum(e) = kind else {
        unreachable!("enum kind")
    };
    enum_name(e, n).map_or_else(|| n.to_string(), str::to_owned)
}

/// `JSONPb.Marshal` of one element's Go value.
fn marshal_elem(
    marshaler: &Marshaler,
    out: &mut Vec<u8>,
    kind: &Kind,
    value: &Value,
) -> Result<(), JsonError> {
    match value {
        Value::Message(m) => {
            let mut enc = Encoder { marshaler, out };
            enc.message(m, None)?;
            if marshaler
                .registry()
                .may_miss_required(m.descriptor().parent_pool())
            {
                super::decode::check_initialized(m)?;
            }
            Ok(())
        }
        Value::EnumNumber(n) => {
            if marshaler.marshal.use_enum_numbers {
                marshal_scalar(out, &Value::I32(*n))
            } else {
                go::append_encoding_json_string(out, &enum_string(kind, *n));
                Ok(())
            }
        }
        other => marshal_scalar(out, other),
    }
}

/// `encoding/json`'s compaction of a `json.RawMessage` with HTML escaping.
fn append_escaped_raw(out: &mut Vec<u8>, raw: &[u8]) {
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        match c {
            b'<' | b'>' | b'&' => {
                out.extend_from_slice(b"\\u00");
                out.push(b"0123456789abcdef"[(c >> 4) as usize]);
                out.push(b"0123456789abcdef"[(c & 15) as usize]);
            }
            0xe2 if i + 2 < raw.len() && raw[i + 1] == 0x80 && raw[i + 2] & !1 == 0xa8 => {
                out.extend_from_slice(b"\\u202");
                out.push(b"0123456789abcdef"[(raw[i + 2] & 15) as usize]);
                i += 2;
            }
            _ => out.push(c),
        }
        i += 1;
    }
}

pub(crate) fn encode_field(
    marshaler: &Marshaler,
    msg: &DynamicMessage,
    field: &FieldDescriptor,
) -> Result<Vec<u8>, JsonError> {
    if is_real_oneof(field) {
        return Err(JsonError::unsupported(
            "a response_body field inside a oneof is not supported",
        ));
    }
    let mut out = Vec::with_capacity(64);
    let kind = field.kind();
    let emit = marshaler.marshal.emit_unpopulated;
    let value = msg.get_field(field);
    if field.is_map() {
        let Kind::Message(entry) = &kind else {
            unreachable!("map entry")
        };
        let value_kind = entry.map_entry_value_field().kind();
        let mut members: Vec<(String, Vec<u8>)> = Vec::new();
        for (key, v) in value.as_map().expect("map") {
            let name = match key {
                MapKey::Bool(b) => b.to_string(),
                MapKey::I32(n) => n.to_string(),
                MapKey::I64(n) => n.to_string(),
                MapKey::U32(n) => n.to_string(),
                MapKey::U64(n) => n.to_string(),
                MapKey::String(s) => s.clone(),
            };
            let mut raw = Vec::new();
            marshal_elem(marshaler, &mut raw, &value_kind, v)?;
            members.push((name, raw));
        }
        members.sort_by(|a, b| a.0.as_bytes().cmp(b.0.as_bytes()));
        out.push(b'{');
        for (i, (name, raw)) in members.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            go::append_encoding_json_string(&mut out, name);
            out.push(b':');
            append_escaped_raw(&mut out, raw);
        }
        out.push(b'}');
        return Ok(out);
    }
    if field.is_list() {
        let items = value.as_list().expect("list");
        if items.is_empty() {
            out.extend_from_slice(if emit { b"[]" } else { b"null" });
            return Ok(out);
        }
        out.push(b'[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                out.push(b',');
            }
            match (&kind, item) {
                // `"` + String() + `"`, not escaped.
                (Kind::Enum(_), Value::EnumNumber(n)) if !marshaler.marshal.use_enum_numbers => {
                    out.push(b'"');
                    out.extend_from_slice(enum_string(&kind, *n).as_bytes());
                    out.push(b'"');
                }
                _ => marshal_elem(marshaler, &mut out, &kind, item)?,
            }
        }
        out.push(b']');
        return Ok(out);
    }
    match &kind {
        Kind::Message(md) => {
            if go_has(msg, field) {
                marshal_elem(marshaler, &mut out, &kind, &value)?;
            } else {
                // A nil `*M` marshals like an empty message.
                marshal_elem(
                    marshaler,
                    &mut out,
                    &kind,
                    &Value::Message(DynamicMessage::new(md.clone())),
                )?;
            }
        }
        Kind::Bytes => {
            // nil unless present; a nil []byte is a nil slice.
            let present = if field.supports_presence() {
                msg.has_field(field)
            } else {
                go_has(msg, field)
            };
            if present {
                marshal_scalar(&mut out, &value)?;
            } else {
                out.extend_from_slice(if emit { b"[]" } else { b"null" });
            }
        }
        _ => {
            if field.supports_presence() && !msg.has_field(field) {
                out.extend_from_slice(b"null");
            } else {
                marshal_elem(marshaler, &mut out, &kind, &value)?;
            }
        }
    }
    Ok(out)
}
