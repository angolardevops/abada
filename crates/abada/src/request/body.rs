//! The body step of a generated handler:
//! `marshaler.NewDecoder(req.Body).Decode(&protoReq)` for `body: "*"`, or
//! `Decode(&protoReq.<Field>)` for `body: "<field>"`, with `io.EOF` ignored.
//!
//! grpc-gateway's `JSONPb` decoder hands a message to protojson. A field that
//! is not a message goes through `decodeNonProtoField`: plain `encoding/json`
//! into the Go type, with its own rules for maps (keys through
//! `runtime.String`/`Int64`/…), slices (items one by one), enums (numbers
//! only) and pointers (`proto3 optional`).

use prost_reflect::bytes::Bytes;
use prost_reflect::{DynamicMessage, FieldDescriptor, Kind, ReflectMessage, Value};

use super::RequestError;
use super::fields::{Ctx, is_message, map_key};
use super::json::{FirstValue, Json, Node, first_value, parse};
use super::strconv::{Alphabet, base64_decode, parse_bool, parse_float, parse_int, parse_uint};

fn go_type(kind: &Kind) -> String {
    match kind {
        Kind::Double => "float64".into(),
        Kind::Float => "float32".into(),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => "int32".into(),
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => "int64".into(),
        Kind::Uint32 | Kind::Fixed32 => "uint32".into(),
        Kind::Uint64 | Kind::Fixed64 => "uint64".into(),
        Kind::Bool => "bool".into(),
        Kind::String => "string".into(),
        Kind::Bytes => "[]uint8".into(),
        Kind::Enum(e) => e.full_name().into(),
        Kind::Message(m) => m.full_name().into(),
    }
}

fn cannot_unmarshal(node: &Node, kind: &Kind) -> RequestError {
    let what = match &node.json {
        Json::Number(lit) => format!("number {lit}"),
        other => other.kind().to_string(),
    };
    RequestError::decoder(format!(
        "json: cannot unmarshal {what} into Go value of type {}",
        go_type(kind)
    ))
}

/// A message through the codec.
fn decode_message(ctx: &Ctx<'_>, kind: &Kind, raw: &[u8]) -> Result<DynamicMessage, RequestError> {
    let Kind::Message(desc) = kind else {
        unreachable!("called for messages");
    };
    ctx.decoder.decode(desc, raw).map_err(RequestError::decoder)
}

/// `encoding/json` into a Go scalar (or `protoEnum`) of `kind`. `null` is the
/// caller's business.
fn decode_scalar(ctx: &mut Ctx<'_>, kind: &Kind, node: &Node) -> Result<Value, RequestError> {
    let mismatch = || cannot_unmarshal(node, kind);
    Ok(match (kind, &node.json) {
        (Kind::String, Json::String(s)) => Value::String(ctx.string(s.as_bytes())),
        (Kind::Bool, Json::Bool(b)) => Value::Bool(*b),
        (Kind::Int32 | Kind::Sint32 | Kind::Sfixed32, Json::Number(lit)) => {
            Value::I32(parse_int(lit.as_bytes(), false, 32).map_err(|_| mismatch())? as i32)
        }
        (Kind::Int64 | Kind::Sint64 | Kind::Sfixed64, Json::Number(lit)) => {
            Value::I64(parse_int(lit.as_bytes(), false, 64).map_err(|_| mismatch())?)
        }
        (Kind::Uint32 | Kind::Fixed32, Json::Number(lit)) => {
            Value::U32(parse_uint(lit.as_bytes(), false, 32).map_err(|_| mismatch())? as u32)
        }
        (Kind::Uint64 | Kind::Fixed64, Json::Number(lit)) => {
            Value::U64(parse_uint(lit.as_bytes(), false, 64).map_err(|_| mismatch())?)
        }
        (Kind::Float, Json::Number(lit)) => {
            Value::F32(parse_float(lit.as_bytes(), 32).map_err(|_| mismatch())? as f32)
        }
        (Kind::Double, Json::Number(lit)) => {
            Value::F64(parse_float(lit.as_bytes(), 64).map_err(|_| mismatch())?)
        }
        (Kind::Bytes, Json::String(s)) => Value::Bytes(Bytes::from(
            base64_decode(Alphabet::Std, s.as_bytes())
                .map_err(|e| RequestError::decoder(e.to_string()))?,
        )),
        (Kind::Enum(_), Json::Number(lit)) => {
            let f = parse_float(lit.as_bytes(), 64).map_err(|_| {
                RequestError::decoder(format!(
                    "json: cannot unmarshal number {lit} into Go value of type float64"
                ))
            })?;
            // `int32(f)`: amd64 truncates through a 64-bit conversion, which
            // yields the "integer indefinite" value out of range.
            let wide = if f.is_finite() && (i64::MIN as f64..-(i64::MIN as f64)).contains(&f) {
                f.trunc() as i64
            } else {
                i64::MIN
            };
            Value::EnumNumber(wide as i32)
        }
        (Kind::Enum(e), Json::String(s)) => {
            return Err(RequestError::decoder(format!(
                "unmarshaling of symbolic enum {:?} not supported: {}",
                s,
                e.full_name()
            )));
        }
        (Kind::Enum(e), other) => {
            return Err(RequestError::decoder(format!(
                "cannot assign {} into Go type {}",
                other.kind(),
                e.full_name()
            )));
        }
        (Kind::Message(_), _) => unreachable!("messages go through the codec"),
        _ => return Err(mismatch()),
    })
}

/// A list or map item through `unmarshalJSONPb`: a fresh decoder on its bytes.
fn decode_item(
    ctx: &mut Ctx<'_>,
    kind: &Kind,
    node: &Node,
    raw: &[u8],
) -> Result<Value, RequestError> {
    if matches!(kind, Kind::Message(_)) {
        return Ok(Value::Message(decode_message(
            ctx,
            kind,
            &raw[node.start..node.end],
        )?));
    }
    if node.json == Json::Null && !matches!(kind, Kind::Enum(_)) {
        return Ok(Value::default_value(kind));
    }
    decode_scalar(ctx, kind, node)
}

fn map_key_value(kind: &Kind, key: &str) -> Result<Value, RequestError> {
    let err = |e: super::strconv::NumError| RequestError::decoder(e.to_string());
    let b = key.as_bytes();
    Ok(match kind {
        Kind::String => Value::String(key.to_string()),
        Kind::Bool => Value::Bool(parse_bool(b).map_err(err)?),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            Value::I32(parse_int(b, true, 32).map_err(err)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            Value::I64(parse_int(b, true, 64).map_err(err)?)
        }
        Kind::Uint32 | Kind::Fixed32 => Value::U32(parse_uint(b, true, 32).map_err(err)? as u32),
        Kind::Uint64 | Kind::Fixed64 => Value::U64(parse_uint(b, true, 64).map_err(err)?),
        other => unreachable!("not a map key kind: {other:?}"),
    })
}

/// `body: "*"`.
pub(crate) fn decode_whole(
    ctx: &Ctx<'_>,
    msg: &mut DynamicMessage,
    body: &[u8],
) -> Result<(), RequestError> {
    match first_value(body) {
        FirstValue::Eof => Ok(()),
        FirstValue::Error(e) => Err(RequestError::decoder(e)),
        FirstValue::Value(raw) => {
            *msg = ctx
                .decoder
                .decode(&msg.descriptor(), raw)
                .map_err(RequestError::decoder)?;
            Ok(())
        }
    }
}

/// `body: "<field>"`, a top-level field of the request.
pub(crate) fn decode_field(
    ctx: &mut Ctx<'_>,
    msg: &mut DynamicMessage,
    fd: &FieldDescriptor,
    body: &[u8],
) -> Result<(), RequestError> {
    let kind = fd.kind();
    let first = first_value(body);
    if let FirstValue::Error(e) = first {
        // The pointer, the oneof wrapper or the map were already allocated,
        // but the error is all anyone sees.
        return Err(RequestError::decoder(e));
    }
    if fd.is_map() {
        let FirstValue::Value(raw) = first else {
            return Ok(());
        };
        let node = parse(raw);
        let members = match &node.json {
            Json::Null => return Ok(()),
            Json::Object(members) => members,
            other => {
                return Err(RequestError::decoder(format!(
                    "json: cannot unmarshal {} into Go value of type map[string]*json.RawMessage",
                    other.kind()
                )));
            }
        };
        let Kind::Message(entry) = &kind else {
            unreachable!("a map field is a message entry");
        };
        let key_kind = entry.map_entry_key_field().kind();
        let value_kind = entry.map_entry_value_field().kind();
        let mut entries = Vec::with_capacity(members.len());
        for (k, v) in members {
            let key = map_key_value(&key_kind, k)?;
            let value = decode_item(ctx, &value_kind, v, raw)?;
            entries.push((map_key(key), value));
        }
        let Value::Map(map) = msg.get_field_mut(fd) else {
            unreachable!("a map field holds a map");
        };
        map.extend(entries);
        return Ok(());
    }
    if fd.is_list() {
        let FirstValue::Value(raw) = first else {
            return Ok(());
        };
        let node = parse(raw);
        let items = match &node.json {
            Json::Null => return Ok(()),
            Json::Array(items) => items,
            other => {
                let target = if kind == Kind::Bytes {
                    "[][]uint8".to_string()
                } else {
                    "[]json.RawMessage".to_string()
                };
                return Err(RequestError::decoder(format!(
                    "json: cannot unmarshal {} into Go value of type {target}",
                    other.kind()
                )));
            }
        };
        let mut values = Vec::with_capacity(items.len());
        for item in items {
            values.push(decode_item(ctx, &kind, item, raw)?);
        }
        let Value::List(list) = msg.get_field_mut(fd) else {
            unreachable!("a list field holds a list");
        };
        list.extend(values);
        return Ok(());
    }
    if is_message(fd) {
        // `rv.Set(reflect.New(...))` happens before the decoder reads: an
        // empty body still leaves the message set.
        let value = match first {
            FirstValue::Value(raw) => decode_message(ctx, &kind, raw)?,
            _ => DynamicMessage::new(kind.as_message().expect("a message").clone()),
        };
        msg.set_field(fd, Value::Message(value));
        return Ok(());
    }
    let has_pointer = fd.supports_presence();
    if has_pointer {
        // A `*T` for proto3 optional, or the oneof wrapper: allocated first.
        msg.set_field(fd, Value::default_value(&kind));
    }
    let FirstValue::Value(raw) = first else {
        return Ok(());
    };
    let node = parse(raw);
    if node.json == Json::Null {
        match kind {
            Kind::Enum(_) => {}
            Kind::Bytes => return Ok(()),
            _ => {
                if fd.containing_oneof().is_some_and(|o| o.is_synthetic()) {
                    // `**T` set to nil.
                    msg.clear_field(fd);
                }
                return Ok(());
            }
        }
    }
    let value = decode_scalar(ctx, &kind, &node)?;
    msg.set_field(fd, value);
    Ok(())
}
