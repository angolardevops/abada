//! Setting fields from strings: `runtime.PopulateFieldFromPath` and the query
//! parser's `populateFieldValueFromPath`, and the typed converters
//! (`runtime.Int32`, `runtime.Enum`, `runtime.Timestamp`, …) the generated code
//! calls for a top-level path variable.

use prost_reflect::bytes::Bytes;
use prost_reflect::{
    DynamicMessage, EnumDescriptor, FieldDescriptor, Kind, MapKey, MessageDescriptor,
    ReflectMessage, Value,
};

use super::gotime::{parse_duration, parse_rfc3339_nano, runtime_duration, runtime_timestamp};
use super::strconv::{atoi, parse_bool, parse_float, parse_int, parse_uint, quote, runtime_bytes};
use super::{ErrorOrigin, RequestError};
use crate::json::Marshaler;

/// State carried through one request.
pub(crate) struct Ctx<'d> {
    /// The proto3 JSON codec: `Struct` and `Value` query values go through
    /// `protojson.Unmarshal`.
    pub(crate) marshaler: &'d Marshaler,
    /// A string field received bytes that are not UTF-8.
    pub(crate) invalid_utf8: bool,
}

impl Ctx<'_> {
    pub(crate) fn string(&mut self, bytes: &[u8]) -> String {
        match std::str::from_utf8(bytes) {
            Ok(s) => s.to_string(),
            Err(_) => {
                self.invalid_utf8 = true;
                String::from_utf8_lossy(bytes).into_owned()
            }
        }
    }
}

/// An error on its way up, before the caller wraps it.
pub(crate) struct FieldError {
    pub(crate) message: String,
    pub(crate) origin: ErrorOrigin,
}

impl FieldError {
    pub(crate) fn gateway(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
            origin: ErrorOrigin::Gateway,
        }
    }

    pub(crate) fn wrap(self, prefix: impl std::fmt::Display) -> Self {
        Self {
            message: format!("{prefix}{}", self.message),
            origin: self.origin,
        }
    }

    pub(crate) fn into_request_error(self) -> RequestError {
        RequestError::invalid(self.message).with_origin(self.origin)
    }
}

fn lossy(b: &[u8]) -> String {
    String::from_utf8_lossy(b).into_owned()
}

pub(crate) fn is_message(fd: &FieldDescriptor) -> bool {
    matches!(fd.kind(), Kind::Message(_))
}

pub(crate) fn wkt_message(desc: &MessageDescriptor, fields: &[(u32, Value)]) -> DynamicMessage {
    let mut m = DynamicMessage::new(desc.clone());
    for (number, value) in fields {
        m.set_field_by_number(*number, value.clone());
    }
    m
}

/// `runtime.parseField`: the conversion `populateFieldValueFromPath` applies.
pub(crate) fn parse_field(
    ctx: &mut Ctx<'_>,
    kind: &Kind,
    value: &[u8],
) -> Result<Value, FieldError> {
    let num = |e: super::strconv::NumError| FieldError::gateway(e.to_string());
    Ok(match kind {
        Kind::Bool => Value::Bool(parse_bool(value).map_err(num)?),
        Kind::Enum(e) => Value::EnumNumber(enum_by_name_or_atoi(e, value)?),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            Value::I32(parse_int(value, false, 32).map_err(num)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            Value::I64(parse_int(value, false, 64).map_err(num)?)
        }
        Kind::Uint32 | Kind::Fixed32 => {
            Value::U32(parse_uint(value, false, 32).map_err(num)? as u32)
        }
        Kind::Uint64 | Kind::Fixed64 => Value::U64(parse_uint(value, false, 64).map_err(num)?),
        Kind::Float => Value::F32(parse_float(value, 32).map_err(num)? as f32),
        Kind::Double => Value::F64(parse_float(value, 64).map_err(num)?),
        Kind::String => Value::String(ctx.string(value)),
        Kind::Bytes => Value::Bytes(Bytes::from(
            runtime_bytes(value).map_err(|e| FieldError::gateway(e.to_string()))?,
        )),
        Kind::Message(m) => Value::Message(parse_message(ctx, m, value)?),
    })
}

fn enum_by_name_or_atoi(e: &EnumDescriptor, value: &[u8]) -> Result<i32, FieldError> {
    let invalid = || FieldError::gateway(format!("{} is not a valid value", quote(value)));
    if let Some(v) = std::str::from_utf8(value)
        .ok()
        .and_then(|name| e.get_value_by_name(name))
    {
        return Ok(v.number());
    }
    let i = atoi(value).ok_or_else(invalid)?;
    // `protoreflect.EnumNumber(i)` truncates to 32 bits.
    let n = i as i32;
    e.get_value(n).map(|v| v.number()).ok_or_else(invalid)
}

fn parse_message(
    ctx: &mut Ctx<'_>,
    desc: &MessageDescriptor,
    value: &[u8],
) -> Result<DynamicMessage, FieldError> {
    let num = |e: super::strconv::NumError| FieldError::gateway(e.to_string());
    let wrapper = |v: Value| wkt_message(desc, &[(1, v)]);
    Ok(match desc.full_name() {
        "google.protobuf.Timestamp" => {
            let t = parse_rfc3339_nano(value).map_err(FieldError::gateway)?;
            if !super::gotime::timestamp_is_valid(t) {
                return Err(FieldError::gateway(format!(
                    "{} before 0001-01-01",
                    lossy(value)
                )));
            }
            wkt_message(
                desc,
                &[(1, Value::I64(t.seconds)), (2, Value::I32(t.nanos))],
            )
        }
        "google.protobuf.Duration" => {
            let d = parse_duration(value).map_err(FieldError::gateway)?;
            wkt_message(
                desc,
                &[
                    (1, Value::I64(d / 1_000_000_000)),
                    (2, Value::I32((d % 1_000_000_000) as i32)),
                ],
            )
        }
        "google.protobuf.DoubleValue" => wrapper(Value::F64(parse_float(value, 64).map_err(num)?)),
        "google.protobuf.FloatValue" => {
            wrapper(Value::F32(parse_float(value, 32).map_err(num)? as f32))
        }
        "google.protobuf.Int64Value" => {
            wrapper(Value::I64(parse_int(value, false, 64).map_err(num)?))
        }
        "google.protobuf.Int32Value" => {
            wrapper(Value::I32(parse_int(value, false, 32).map_err(num)? as i32))
        }
        "google.protobuf.UInt64Value" => {
            wrapper(Value::U64(parse_uint(value, false, 64).map_err(num)?))
        }
        "google.protobuf.UInt32Value" => {
            wrapper(Value::U32(parse_uint(value, false, 32).map_err(num)? as u32))
        }
        "google.protobuf.BoolValue" => wrapper(Value::Bool(parse_bool(value).map_err(num)?)),
        "google.protobuf.StringValue" => wrapper(Value::String(ctx.string(value))),
        "google.protobuf.BytesValue" => wrapper(Value::Bytes(Bytes::from(
            runtime_bytes(value).map_err(|e| FieldError::gateway(e.to_string()))?,
        ))),
        "google.protobuf.FieldMask" => {
            let paths = value
                .split(|&c| c == b',')
                .map(|p| Value::String(ctx.string(p)))
                .collect();
            wkt_message(desc, &[(1, Value::List(paths))])
        }
        "google.protobuf.Value" | "google.protobuf.Struct" => {
            let mut m = DynamicMessage::new(desc.clone());
            ctx.marshaler
                .unmarshal_into(&mut m, value)
                .map_err(|e| FieldError {
                    message: e.to_string(),
                    origin: ErrorOrigin::Decoder,
                })?;
            m
        }
        other => {
            return Err(FieldError::gateway(format!(
                "unsupported message type: {}",
                quote(other.as_bytes())
            )));
        }
    })
}

fn lookup(desc: &MessageDescriptor, name: &[u8]) -> Option<FieldDescriptor> {
    let name = std::str::from_utf8(name).ok()?;
    desc.get_field_by_name(name)
        .or_else(|| desc.get_field_by_json_name(name))
}

/// The deepest message nesting a request may be built with, the root counted:
/// the same 100 the JSON codec allows (`abada::json::DEFAULT_RECURSION_LIMIT`).
/// prost, which decodes what abada sends, accepts 101 (measured), so a request
/// built from scalars and well-known types is never refused downstream for its
/// depth. A `Struct` or `Value` last field is the exception: the JSON codec
/// fills it, and for prost an object level is three messages (`Value`, `Struct`,
/// the map entry) and an array level two, so it decodes at most 33 object or 50
/// array levels, fewer the deeper the field sits — beyond that the backend's
/// decoder answers 400 (measured on `prost_types::Value`); no crash.
///
/// grpc-gateway has no such limit: its walk is a loop, and Go's stack grows.
/// Here a loop alone would not be enough — the message it built would be
/// dropped and encoded by recursive code on a 2 MiB stack, and 4 000 levels
/// abort the process. Written deviation: docs/DESIGN.md, "Query field paths".
pub(crate) const MAX_MESSAGE_DEPTH: usize = 100;

fn too_deep() -> FieldError {
    FieldError::gateway("exceeded max recursion depth")
}

/// What the rest of a path past the limit would have answered in Go, told from
/// the descriptors alone (nothing deeper is built, and a message Go creates
/// there has no oneof set): a name that is not a singular message is still
/// "is not a message"; everything else is the limit.
fn past_the_limit(fd: &FieldDescriptor, rest: &[&[u8]]) -> FieldError {
    let Kind::Message(mut desc) = fd.kind() else {
        unreachable!("the walk only descends through message fields");
    };
    for (j, name) in rest.iter().enumerate() {
        let Some(next) = lookup(&desc, name) else {
            break;
        };
        if j == rest.len() - 1 {
            break;
        }
        match next.kind() {
            Kind::Message(m) if !next.is_list() && !next.is_map() => desc = m,
            _ => {
                return FieldError::gateway(format!(
                    "invalid path: {} is not a message",
                    quote(name)
                ));
            }
        }
    }
    too_deep()
}

/// `runtime.populateFieldValueFromPath`: walk `path` (proto or JSON names),
/// creating messages on the way, and set the last field from `values`. An
/// unknown name ends the walk without an error. A walk that would nest the
/// request deeper than [`MAX_MESSAGE_DEPTH`] fails. A path that ends before
/// that, on an unknown name or an error, answers as grpc-gateway does. Past the
/// limit a name that is not a message answers as grpc-gateway does too; an
/// unknown name, or any error of the last field, is the limit's 400 (Go builds
/// a message that deep first).
pub(crate) fn populate_field_value_from_path(
    ctx: &mut Ctx<'_>,
    msg: &mut DynamicMessage,
    path: &[&[u8]],
    values: &[Vec<u8>],
) -> Result<(), FieldError> {
    if path.is_empty() {
        return Err(FieldError::gateway("no field path"));
    }
    if values.is_empty() {
        return Err(FieldError::gateway("no value provided"));
    }
    let mut msg = msg;
    // Levels of message so far: the root is the first.
    let mut level = 1;
    let mut i = 0;
    let fd = loop {
        let name = path[i];
        let Some(fd) = lookup(&msg.descriptor(), name) else {
            return Ok(());
        };
        if let Some(oneof) = fd.containing_oneof() {
            if !oneof.is_synthetic() {
                if let Some(set) = oneof.fields().find(|f| msg.has_field(f)) {
                    if !is_message(&fd) || fd.full_name() != set.full_name() {
                        return Err(FieldError::gateway(format!(
                            "field already set for oneof {}",
                            quote(oneof.name().as_bytes())
                        )));
                    }
                }
            }
        }
        if i == path.len() - 1 {
            break fd;
        }
        if !is_message(&fd) || fd.is_list() || fd.is_map() {
            return Err(FieldError::gateway(format!(
                "invalid path: {} is not a message",
                quote(name)
            )));
        }
        level += 1;
        if level > MAX_MESSAGE_DEPTH {
            return Err(past_the_limit(&fd, &path[i + 1..]));
        }
        let Value::Message(child) = msg.get_field_mut(&fd) else {
            unreachable!("a singular message field holds a message");
        };
        msg = child;
        i += 1;
    };
    // A message-typed value (a well-known type; the element of a list, the value
    // of a map) is one level more. A map's entry is not a level of its own.
    let value_kind = if fd.is_map() {
        let Kind::Message(entry) = fd.kind() else {
            unreachable!("a map field is a message entry");
        };
        entry.map_entry_value_field().kind()
    } else {
        fd.kind()
    };
    if matches!(value_kind, Kind::Message(_)) && level + 1 > MAX_MESSAGE_DEPTH {
        return Err(too_deep());
    }

    let field_name = quote(fd.name().as_bytes());
    if fd.is_list() {
        let kind = fd.kind();
        let mut parsed = Vec::with_capacity(values.len());
        for v in values {
            parsed.push(
                parse_field(ctx, &kind, v)
                    .map_err(|e| e.wrap(format_args!("parsing list {field_name}: ")))?,
            );
        }
        let Value::List(list) = msg.get_field_mut(&fd) else {
            unreachable!("a list field holds a list");
        };
        list.extend(parsed);
        return Ok(());
    }
    if fd.is_map() {
        if values.len() != 2 {
            return Err(FieldError::gateway(format!(
                "more than one value provided for key {} in map {}",
                quote(&values[0]),
                quote(fd.full_name().as_bytes())
            )));
        }
        let Kind::Message(entry) = fd.kind() else {
            unreachable!("a map field is a message entry");
        };
        let key = parse_field(ctx, &entry.map_entry_key_field().kind(), &values[0])
            .map_err(|e| e.wrap(format_args!("parsing map key {field_name}: ")))?;
        let value = parse_field(ctx, &entry.map_entry_value_field().kind(), &values[1])
            .map_err(|e| e.wrap(format_args!("parsing map value {field_name}: ")))?;
        let Value::Map(map) = msg.get_field_mut(&fd) else {
            unreachable!("a map field holds a map");
        };
        map.insert(map_key(key), value);
        return Ok(());
    }
    if values.len() > 1 {
        let joined: Vec<String> = values.iter().map(|v| lossy(v)).collect();
        return Err(FieldError::gateway(format!(
            "too many values for field {field_name}: {}",
            joined.join(", ")
        )));
    }
    let v = parse_field(ctx, &fd.kind(), &values[0])
        .map_err(|e| e.wrap(format_args!("parsing field {field_name}: ")))?;
    msg.set_field(&fd, v);
    Ok(())
}

pub(crate) fn map_key(v: Value) -> MapKey {
    match v {
        Value::Bool(b) => MapKey::Bool(b),
        Value::I32(i) => MapKey::I32(i),
        Value::I64(i) => MapKey::I64(i),
        Value::U32(i) => MapKey::U32(i),
        Value::U64(i) => MapKey::U64(i),
        Value::String(s) => MapKey::String(s),
        other => unreachable!("not a map key: {other:?}"),
    }
}

/// `runtime.Enum`: a name, or a number (`strconv.ParseInt` base 0) the enum
/// declares.
pub(crate) fn runtime_enum(e: &EnumDescriptor, val: &[u8]) -> Result<i32, String> {
    if let Some(v) = std::str::from_utf8(val)
        .ok()
        .and_then(|name| e.get_value_by_name(name))
    {
        return Ok(v.number());
    }
    let invalid = || format!("{} is not valid", lossy(val));
    let i = parse_int(val, true, 32).map_err(|_| invalid())? as i32;
    e.values()
        .find(|v| v.number() == i)
        .map(|v| v.number())
        .ok_or_else(invalid)
}

/// The converter the generated code calls for a top-level path variable of
/// `kind` (`runtime.<Kind>`, base 0 for integers), or of a well-known type.
pub(crate) fn runtime_convert(ctx: &mut Ctx<'_>, kind: &Kind, val: &[u8]) -> Result<Value, String> {
    let num = |e: super::strconv::NumError| e.to_string();
    Ok(match kind {
        Kind::Bool => Value::Bool(parse_bool(val).map_err(num)?),
        Kind::Enum(e) => Value::EnumNumber(runtime_enum(e, val)?),
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            Value::I32(parse_int(val, true, 32).map_err(num)? as i32)
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => {
            Value::I64(parse_int(val, true, 64).map_err(num)?)
        }
        Kind::Uint32 | Kind::Fixed32 => Value::U32(parse_uint(val, true, 32).map_err(num)? as u32),
        Kind::Uint64 | Kind::Fixed64 => Value::U64(parse_uint(val, true, 64).map_err(num)?),
        Kind::Float => Value::F32(parse_float(val, 32).map_err(num)? as f32),
        Kind::Double => Value::F64(parse_float(val, 64).map_err(num)?),
        Kind::String => Value::String(ctx.string(val)),
        Kind::Bytes => Value::Bytes(Bytes::from(runtime_bytes(val).map_err(|e| e.to_string())?)),
        Kind::Message(m) => {
            let wrapper = |v: Value| Value::Message(wkt_message(m, &[(1, v)]));
            match m.full_name() {
                "google.protobuf.Timestamp" => {
                    let t = runtime_timestamp(val)?;
                    Value::Message(wkt_message(
                        m,
                        &[(1, Value::I64(t.seconds)), (2, Value::I32(t.nanos))],
                    ))
                }
                "google.protobuf.Duration" => {
                    let (s, n) = runtime_duration(val)?;
                    Value::Message(wkt_message(m, &[(1, Value::I64(s)), (2, Value::I32(n))]))
                }
                "google.protobuf.StringValue" => wrapper(Value::String(ctx.string(val))),
                "google.protobuf.FloatValue" => {
                    wrapper(Value::F32(parse_float(val, 32).map_err(num)? as f32))
                }
                "google.protobuf.DoubleValue" => {
                    wrapper(Value::F64(parse_float(val, 64).map_err(num)?))
                }
                "google.protobuf.BoolValue" => wrapper(Value::Bool(parse_bool(val).map_err(num)?)),
                "google.protobuf.BytesValue" => wrapper(Value::Bytes(Bytes::from(
                    runtime_bytes(val).map_err(|e| e.to_string())?,
                ))),
                "google.protobuf.Int32Value" => {
                    wrapper(Value::I32(parse_int(val, true, 32).map_err(num)? as i32))
                }
                "google.protobuf.UInt32Value" => {
                    wrapper(Value::U32(parse_uint(val, true, 32).map_err(num)? as u32))
                }
                "google.protobuf.Int64Value" => {
                    wrapper(Value::I64(parse_int(val, true, 64).map_err(num)?))
                }
                "google.protobuf.UInt64Value" => {
                    wrapper(Value::U64(parse_uint(val, true, 64).map_err(num)?))
                }
                other => unreachable!("binding refused a {other} path variable"),
            }
        }
    })
}

/// The well-known types a top-level path variable may have: the generator's
/// `wellKnownTypeConv`.
pub(crate) fn has_runtime_converter(full_name: &str) -> bool {
    matches!(
        full_name,
        "google.protobuf.Timestamp"
            | "google.protobuf.Duration"
            | "google.protobuf.StringValue"
            | "google.protobuf.FloatValue"
            | "google.protobuf.DoubleValue"
            | "google.protobuf.BoolValue"
            | "google.protobuf.BytesValue"
            | "google.protobuf.Int32Value"
            | "google.protobuf.UInt32Value"
            | "google.protobuf.Int64Value"
            | "google.protobuf.UInt64Value"
    )
}
