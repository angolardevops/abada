//! protojson's `marshal` (`encoding/protojson/encode.go` and the encoding
//! half of `well_known_types.go`), without the random spaces
//! (`internal/detrand`) that make Go's bytes differ between builds.

use std::borrow::Cow;

use prost_reflect::{DynamicMessage, Kind, ReflectMessage, Value};

use super::decode::{
    MAX_DURATION_SECONDS, MAX_TIMESTAMP_SECONDS, MIN_TIMESTAMP_SECONDS, check_initialized,
    is_valid_full_name, json_camel_case, json_snake_case,
};
use super::{Field, JsonError, Marshaler, Wkt, go, go_has, is_null_value, wire, wkt};

/// `MarshalOptions.marshal`: the message, then the required-field check.
pub(crate) fn marshal(
    marshaler: &Marshaler,
    msg: &DynamicMessage,
    out: &mut Vec<u8>,
) -> Result<(), JsonError> {
    Encoder { marshaler, out }.message(msg, None)?;
    check_initialized(msg)
}

pub(crate) struct Encoder<'m, 'o> {
    pub(crate) marshaler: &'m Marshaler,
    pub(crate) out: &'o mut Vec<u8>,
}

impl Encoder<'_, '_> {
    fn comma(&mut self, first: &mut bool) {
        if !*first {
            self.out.push(b',');
        }
        *first = false;
    }

    fn name(&mut self, name: &str) {
        go::append_protojson_string(self.out, name);
        self.out.push(b':');
    }

    /// `marshalMessage`; `type_url` adds the `@type` of an `Any`.
    pub(crate) fn message(
        &mut self,
        m: &DynamicMessage,
        type_url: Option<&str>,
    ) -> Result<(), JsonError> {
        let desc = m.descriptor();
        if let Some(w) = wkt(&desc) {
            return self.well_known(w, m);
        }
        let opts = self.marshaler.marshal;
        self.out.push(b'{');
        let mut first = true;
        if let Some(url) = type_url {
            self.comma(&mut first);
            self.name("@type");
            go::append_protojson_string(self.out, url);
        }
        let emit = opts.emit_unpopulated || opts.emit_default_values;
        let fields = desc.descriptor_proto().field.iter().map(|f| {
            desc.get_field(f.number() as u32)
                .expect("a declared field has a descriptor")
        });
        for fd in fields {
            let populated = go_has(m, &fd);
            let null = if populated {
                false
            } else {
                // Unset oneof members, proto3 `optional` fields included, are
                // never written; unset fields with presence are `null`.
                if !emit || fd.containing_oneof().is_some() {
                    continue;
                }
                if fd.supports_presence() && !fd.is_list() && !fd.is_map() {
                    if !opts.emit_unpopulated {
                        continue;
                    }
                    true
                } else {
                    false
                }
            };
            self.comma(&mut first);
            self.name(if opts.use_proto_names {
                fd.name()
            } else {
                fd.json_name()
            });
            if null {
                self.out.extend_from_slice(b"null");
            } else {
                let value = m.get_field(&fd);
                self.value(&Field::Plain(fd), &value)?;
            }
        }
        let mut extensions: Vec<_> = m.extensions().collect();
        extensions.sort_by(|a, b| a.0.full_name().cmp(b.0.full_name()));
        for (ext, value) in extensions {
            self.comma(&mut first);
            self.name(&format!("[{}]", ext.full_name()));
            self.value(&Field::Extension(ext), value)?;
        }
        self.out.push(b'}');
        Ok(())
    }

    /// `marshalValue`.
    fn value(&mut self, field: &Field, value: &Value) -> Result<(), JsonError> {
        let kind = field.kind();
        if field.is_map() {
            let Kind::Message(entry) = &kind else {
                unreachable!("map entry")
            };
            return self.map(
                value.as_map().expect("map"),
                &entry.map_entry_value_field().kind(),
            );
        }
        if field.is_list() {
            return self.list(value.as_list().expect("list"), &kind);
        }
        self.singular(&kind, value)
    }

    fn list(&mut self, items: &[Value], kind: &Kind) -> Result<(), JsonError> {
        self.out.push(b'[');
        for (i, item) in items.iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            self.singular(kind, item)?;
        }
        self.out.push(b']');
        Ok(())
    }

    /// `marshalMap`: keys in `order.GenericKeyOrder`.
    fn map(
        &mut self,
        map: &std::collections::HashMap<prost_reflect::MapKey, Value>,
        kind: &Kind,
    ) -> Result<(), JsonError> {
        let mut entries: Vec<_> = map.iter().collect();
        entries.sort_by(|a, b| wire::cmp_keys(a.0, b.0));
        self.out.push(b'{');
        for (i, (key, value)) in entries.into_iter().enumerate() {
            if i > 0 {
                self.out.push(b',');
            }
            let name: Cow<str> = match key {
                prost_reflect::MapKey::String(s) => Cow::Borrowed(s),
                prost_reflect::MapKey::Bool(v) => Cow::Owned(v.to_string()),
                prost_reflect::MapKey::I32(v) => Cow::Owned(v.to_string()),
                prost_reflect::MapKey::I64(v) => Cow::Owned(v.to_string()),
                prost_reflect::MapKey::U32(v) => Cow::Owned(v.to_string()),
                prost_reflect::MapKey::U64(v) => Cow::Owned(v.to_string()),
            };
            self.name(&name);
            self.singular(kind, value)?;
        }
        self.out.push(b'}');
        Ok(())
    }

    /// `marshalSingular`.
    fn singular(&mut self, kind: &Kind, value: &Value) -> Result<(), JsonError> {
        use std::io::Write;
        let out = &mut *self.out;
        match value {
            Value::Bool(v) => out.extend_from_slice(if *v { b"true" } else { b"false" }),
            Value::String(s) => go::append_protojson_string(out, s),
            Value::I32(v) => {
                let _ = write!(out, "{v}");
            }
            Value::U32(v) => {
                let _ = write!(out, "{v}");
            }
            Value::I64(v) => {
                let _ = write!(out, "\"{v}\"");
            }
            Value::U64(v) => {
                let _ = write!(out, "\"{v}\"");
            }
            Value::F32(v) => float(out, *v as f64, 32),
            Value::F64(v) => float(out, *v, 64),
            Value::Bytes(b) => {
                out.push(b'"');
                go::append_base64(out, b);
                out.push(b'"');
            }
            Value::EnumNumber(n) => {
                if is_null_value(kind) {
                    out.extend_from_slice(b"null");
                } else {
                    let Kind::Enum(e) = kind else {
                        unreachable!("enum value of a non-enum kind")
                    };
                    match enum_name(e, *n) {
                        Some(name) if !self.marshaler.marshal.use_enum_numbers => {
                            go::append_protojson_string(out, name)
                        }
                        _ => {
                            let _ = write!(out, "{n}");
                        }
                    }
                }
            }
            Value::Message(m) => self.message(m, None)?,
            Value::List(_) | Value::Map(_) => unreachable!("not a singular value"),
        }
        Ok(())
    }

    fn well_known(&mut self, w: Wkt, m: &DynamicMessage) -> Result<(), JsonError> {
        let desc = m.descriptor();
        let field = |n| desc.get_field(n).expect("well-known field");
        match w {
            Wkt::Any => self.any(m),
            Wkt::Timestamp => {
                let secs = m.get_field(&field(1)).as_i64().expect("seconds");
                let nanos = m.get_field(&field(2)).as_i32().expect("nanos");
                if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&secs) {
                    return Err(JsonError::marshal(format!(
                        "google.protobuf.Timestamp: seconds out of range {secs}"
                    )));
                }
                if !(0..=999_999_999).contains(&nanos) {
                    return Err(JsonError::marshal(format!(
                        "google.protobuf.Timestamp: nanos out of range {nanos}"
                    )));
                }
                let (y, mo, d) = go::civil_from_days(secs.div_euclid(86400));
                let s = secs.rem_euclid(86400);
                let text = format!(
                    "{y:04}-{mo:02}-{d:02}T{:02}:{:02}:{:02}{}Z",
                    s / 3600,
                    s / 60 % 60,
                    s % 60,
                    fraction(nanos as i64)
                );
                go::append_protojson_string(self.out, &text);
                Ok(())
            }
            Wkt::Duration => {
                let mut secs = m.get_field(&field(1)).as_i64().expect("seconds");
                let mut nanos = m.get_field(&field(2)).as_i32().expect("nanos") as i64;
                if !(-MAX_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&secs) {
                    return Err(JsonError::marshal(format!(
                        "google.protobuf.Duration: seconds out of range {secs}"
                    )));
                }
                if !(-999_999_999..=999_999_999).contains(&nanos) {
                    return Err(JsonError::marshal(format!(
                        "google.protobuf.Duration: nanos out of range {nanos}"
                    )));
                }
                if secs > 0 && nanos < 0 || secs < 0 && nanos > 0 {
                    return Err(JsonError::marshal(
                        "google.protobuf.Duration: signs of seconds and nanos do not match",
                    ));
                }
                let mut sign = "";
                if secs < 0 || nanos < 0 {
                    sign = "-";
                    secs = -secs;
                    nanos = -nanos;
                }
                let text = format!("{sign}{secs}{}s", fraction(nanos));
                go::append_protojson_string(self.out, &text);
                Ok(())
            }
            Wkt::Wrapper => {
                let fd = field(1);
                let value = m.get_field(&fd);
                self.singular(&fd.kind(), &value)
            }
            Wkt::Struct => {
                let fd = field(1);
                let Kind::Message(entry) = fd.kind() else {
                    unreachable!("map entry")
                };
                self.map(
                    m.get_field(&fd).as_map().expect("map"),
                    &entry.map_entry_value_field().kind(),
                )
            }
            Wkt::ListValue => {
                let fd = field(1);
                self.list(m.get_field(&fd).as_list().expect("list"), &fd.kind())
            }
            Wkt::Value => {
                let Some(fd) = desc
                    .oneofs()
                    .next()
                    .and_then(|o| o.fields().find(|f| m.has_field(f)))
                else {
                    return Err(JsonError::marshal(
                        "google.protobuf.Value: none of the oneof fields is set",
                    ));
                };
                let value = m.get_field(&fd);
                if let Value::F64(v) = &*value {
                    if !v.is_finite() {
                        return Err(JsonError::marshal(format!(
                            "google.protobuf.Value.number_value: invalid {v} value"
                        )));
                    }
                }
                self.singular(&fd.kind(), &value)
            }
            Wkt::FieldMask => {
                let paths = m.get_field(&field(1));
                let mut joined = String::new();
                for (i, path) in paths.as_list().expect("list").iter().enumerate() {
                    let s = path.as_str().expect("string path");
                    if !is_valid_full_name(s) {
                        return Err(JsonError::marshal(format!(
                            "google.protobuf.FieldMask.paths contains invalid path: {s:?}"
                        )));
                    }
                    let camel = json_camel_case(s);
                    if s != json_snake_case(&camel) {
                        return Err(JsonError::marshal(format!(
                            "google.protobuf.FieldMask.paths contains irreversible value {s:?}"
                        )));
                    }
                    if i > 0 {
                        joined.push(',');
                    }
                    joined.push_str(&camel);
                }
                go::append_protojson_string(self.out, &joined);
                Ok(())
            }
            Wkt::Empty => {
                self.out.extend_from_slice(b"{}");
                Ok(())
            }
        }
    }

    /// `marshalAny`.
    fn any(&mut self, m: &DynamicMessage) -> Result<(), JsonError> {
        let desc = m.descriptor();
        let type_fd = desc.get_field(1).expect("Any.type_url");
        let value_fd = desc.get_field(2).expect("Any.value");
        if !go_has(m, &type_fd) {
            if !go_has(m, &value_fd) {
                self.out.extend_from_slice(b"{}");
                return Ok(());
            }
            return Err(JsonError::marshal(
                "google.protobuf.Any: type_url is not set",
            ));
        }
        let url_value = m.get_field(&type_fd);
        let url = url_value.as_str().expect("type_url");
        let Some(embedded_desc) = self.marshaler.registry().find_message_by_url(url) else {
            return Err(JsonError::unresolvable(format!(
                "google.protobuf.Any: unable to resolve {url:?}: not found"
            )));
        };
        let bytes = m.get_field(&value_fd);
        let embedded = wire::decode(
            &embedded_desc,
            bytes.as_bytes().expect("value"),
            self.marshaler.unmarshal.recursion_limit,
        )
        .map_err(|e| {
            JsonError::marshal(format!(
                "google.protobuf.Any: unable to unmarshal {url:?}: {e}"
            ))
        })?;
        match wkt(&embedded_desc) {
            Some(w) => {
                self.out.push(b'{');
                self.name("@type");
                go::append_protojson_string(self.out, url);
                self.out.push(b',');
                self.name("value");
                self.well_known(w, &embedded)?;
                self.out.push(b'}');
                Ok(())
            }
            None => self.message(&embedded, Some(url)),
        }
    }
}

fn float(out: &mut Vec<u8>, v: f64, bits: u32) {
    if v.is_nan() {
        out.extend_from_slice(b"\"NaN\"");
    } else if v == f64::INFINITY {
        out.extend_from_slice(b"\"Infinity\"");
    } else if v == f64::NEG_INFINITY {
        out.extend_from_slice(b"\"-Infinity\"");
    } else {
        go::append_float(out, v, bits);
    }
}

/// `.%09d` trimmed to 0, 3, 6 or 9 digits.
fn fraction(nanos: i64) -> String {
    if nanos == 0 {
        String::new()
    } else if nanos % 1_000_000 == 0 {
        format!(".{:03}", nanos / 1_000_000)
    } else if nanos % 1000 == 0 {
        format!(".{:06}", nanos / 1000)
    } else {
        format!(".{nanos:09}")
    }
}

/// `Values().ByNumber`: the first value declared with that number.
pub(crate) fn enum_name(e: &prost_reflect::EnumDescriptor, number: i32) -> Option<&str> {
    e.enum_descriptor_proto()
        .value
        .iter()
        .find(|v| v.number() == number)
        .map(|v| v.name())
}
