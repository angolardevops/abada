//! The protobuf binary form, as Go's generated code reads and writes it,
//! where protojson needs it: the bytes of an `Any`.
//!
//! prost-reflect's own decoder rejects a known field sent with another wire
//! type; Go keeps it as an unknown field, so an `Any` from a Go server that
//! renders there must render here. Its encoder skips `-0.0` in a proto3
//! field and writes map entries in `HashMap` order; `Any.value` built from
//! JSON must be Go's deterministic bytes. Hence this module.

use prost_reflect::{
    DynamicMessage, FieldDescriptor, Kind, MapKey, MessageDescriptor, ReflectMessage, Value,
};

use super::{Field, JsonError, go_has};

const MAX_FIELD_NUMBER: u64 = (1 << 29) - 1;

// ---------------------------------------------------------------- encoding

fn put_varint(out: &mut Vec<u8>, mut v: u64) {
    while v >= 0x80 {
        out.push(v as u8 | 0x80);
        v >>= 7;
    }
    out.push(v as u8);
}

fn put_tag(out: &mut Vec<u8>, number: u32, wire_type: u8) {
    put_varint(out, (number as u64) << 3 | wire_type as u64);
}

fn wire_type(kind: &Kind) -> u8 {
    match kind {
        Kind::Double | Kind::Fixed64 | Kind::Sfixed64 => 1,
        Kind::Float | Kind::Fixed32 | Kind::Sfixed32 => 5,
        Kind::String | Kind::Bytes | Kind::Message(_) => 2,
        _ => 0,
    }
}

/// A scalar without its tag.
fn put_scalar(out: &mut Vec<u8>, kind: &Kind, value: &Value) {
    match (kind, value) {
        (Kind::Int32 | Kind::Sfixed32, Value::I32(v)) if matches!(kind, Kind::Sfixed32) => {
            out.extend_from_slice(&v.to_le_bytes())
        }
        (Kind::Int32, Value::I32(v)) => put_varint(out, *v as i64 as u64),
        (Kind::Sint32, Value::I32(v)) => put_varint(out, ((v << 1) ^ (v >> 31)) as u32 as u64),
        (Kind::Int64, Value::I64(v)) => put_varint(out, *v as u64),
        (Kind::Sint64, Value::I64(v)) => put_varint(out, ((v << 1) ^ (v >> 63)) as u64),
        (Kind::Sfixed64, Value::I64(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (Kind::Uint32, Value::U32(v)) => put_varint(out, *v as u64),
        (Kind::Fixed32, Value::U32(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (Kind::Uint64, Value::U64(v)) => put_varint(out, *v),
        (Kind::Fixed64, Value::U64(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (Kind::Float, Value::F32(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (Kind::Double, Value::F64(v)) => out.extend_from_slice(&v.to_le_bytes()),
        (Kind::Bool, Value::Bool(v)) => put_varint(out, *v as u64),
        (Kind::Enum(_), Value::EnumNumber(v)) => put_varint(out, *v as i64 as u64),
        (Kind::String, Value::String(v)) => {
            put_varint(out, v.len() as u64);
            out.extend_from_slice(v.as_bytes());
        }
        (Kind::Bytes, Value::Bytes(v)) => {
            put_varint(out, v.len() as u64);
            out.extend_from_slice(v);
        }
        (Kind::Message(_), Value::Message(m)) => {
            let body = encode(m);
            put_varint(out, body.len() as u64);
            out.extend_from_slice(&body);
        }
        _ => unreachable!("value {value:?} does not match kind {kind:?}"),
    }
}

fn put_field(out: &mut Vec<u8>, field: &Field, value: &Value) {
    let number = field.number();
    let kind = field.kind();
    if field.is_map() {
        let Kind::Message(entry) = &kind else {
            unreachable!("map entry")
        };
        let (kf, vf) = (entry.map_entry_key_field(), entry.map_entry_value_field());
        let mut entries: Vec<_> = value.as_map().expect("map").iter().collect();
        entries.sort_by(|a, b| cmp_keys(a.0, b.0));
        for (k, v) in entries {
            let mut body = Vec::new();
            put_tag(&mut body, 1, wire_type(&kf.kind()));
            put_scalar(&mut body, &kf.kind(), &key_value(k));
            put_tag(&mut body, 2, wire_type(&vf.kind()));
            put_scalar(&mut body, &vf.kind(), v);
            put_tag(out, number, 2);
            put_varint(out, body.len() as u64);
            out.extend_from_slice(&body);
        }
    } else if field.is_list() {
        let items = value.as_list().expect("list");
        if items.is_empty() {
            return;
        }
        if field.is_packed() {
            let mut body = Vec::new();
            for item in items {
                put_scalar(&mut body, &kind, item);
            }
            put_tag(out, number, 2);
            put_varint(out, body.len() as u64);
            out.extend_from_slice(&body);
        } else {
            for item in items {
                put_one(out, field, &kind, item);
            }
        }
    } else {
        put_one(out, field, &kind, value);
    }
}

fn put_one(out: &mut Vec<u8>, field: &Field, kind: &Kind, value: &Value) {
    if field.is_group() {
        put_tag(out, field.number(), 3);
        out.extend_from_slice(&encode(value.as_message().expect("group")));
        put_tag(out, field.number(), 4);
    } else {
        put_tag(out, field.number(), wire_type(kind));
        put_scalar(out, kind, value);
    }
}

pub(crate) fn key_value(key: &MapKey) -> Value {
    match key {
        MapKey::Bool(v) => Value::Bool(*v),
        MapKey::I32(v) => Value::I32(*v),
        MapKey::I64(v) => Value::I64(*v),
        MapKey::U32(v) => Value::U32(*v),
        MapKey::U64(v) => Value::U64(*v),
        MapKey::String(v) => Value::String(v.clone()),
    }
}

/// `order.GenericKeyOrder`: false before true, numbers ascending, strings
/// by bytes.
pub(crate) fn cmp_keys(a: &MapKey, b: &MapKey) -> std::cmp::Ordering {
    match (a, b) {
        (MapKey::Bool(x), MapKey::Bool(y)) => x.cmp(y),
        (MapKey::I32(x), MapKey::I32(y)) => x.cmp(y),
        (MapKey::I64(x), MapKey::I64(y)) => x.cmp(y),
        (MapKey::U32(x), MapKey::U32(y)) => x.cmp(y),
        (MapKey::U64(x), MapKey::U64(y)) => x.cmp(y),
        (MapKey::String(x), MapKey::String(y)) => x.as_bytes().cmp(y.as_bytes()),
        _ => std::cmp::Ordering::Equal,
    }
}

/// `proto.MarshalOptions{Deterministic: true}` for a generated Go type:
/// extensions by number, then fields by number, then unknown fields.
pub(crate) fn encode(msg: &DynamicMessage) -> Vec<u8> {
    let mut out = Vec::new();
    let mut extensions: Vec<_> = msg.extensions().collect();
    extensions.sort_by_key(|(ext, _)| ext.number());
    for (ext, value) in extensions {
        put_field(&mut out, &Field::Extension(ext), value);
    }
    // Fields by number, except that members of a real oneof come last, oneof
    // by oneof in declaration order (`order.LegacyFieldOrder`, kept "for
    // compatibility with historic wire output").
    let desc = msg.descriptor();
    let in_oneof = |fd: &FieldDescriptor| fd.containing_oneof().is_some_and(|o| !o.is_synthetic());
    let plain = desc.fields().filter(|fd| !in_oneof(fd));
    let oneofs = desc.oneofs().filter(|o| !o.is_synthetic()).flat_map(|o| {
        let mut fields: Vec<_> = o.fields().collect();
        fields.sort_by_key(|f| f.number());
        fields
    });
    for fd in plain.chain(oneofs) {
        if go_has(msg, &fd) {
            let value = msg.get_field(&fd);
            put_field(&mut out, &Field::Plain(fd), &value);
        }
    }
    for unknown in msg.unknown_fields() {
        unknown.encode(&mut out);
    }
    out
}

// ---------------------------------------------------------------- decoding

fn corrupt() -> JsonError {
    JsonError::marshal("proto: cannot parse invalid wire-format data")
}

struct Reader<'a> {
    buf: &'a [u8],
    at: usize,
}

impl Reader<'_> {
    /// `protowire.ConsumeVarint`.
    fn varint(&mut self) -> Result<u64, JsonError> {
        let mut v = 0u64;
        for i in 0..10 {
            let b = *self.buf.get(self.at + i).ok_or_else(corrupt)? as u64;
            if i == 9 && b > 1 {
                return Err(corrupt());
            }
            v |= (b & 0x7f) << (7 * i);
            if b < 0x80 {
                self.at += i + 1;
                return Ok(v);
            }
        }
        unreachable!("the tenth byte returns")
    }

    fn fixed<const N: usize>(&mut self) -> Result<[u8; N], JsonError> {
        let bytes = self.buf.get(self.at..self.at + N).ok_or_else(corrupt)?;
        self.at += N;
        Ok(bytes.try_into().expect("N bytes"))
    }

    fn bytes(&mut self) -> Result<&[u8], JsonError> {
        let len = self.varint()? as usize;
        let end = self.at.checked_add(len).ok_or_else(corrupt)?;
        let bytes = self.buf.get(self.at..end).ok_or_else(corrupt)?;
        self.at = end;
        Ok(bytes)
    }

    fn tag(&mut self) -> Result<(u32, u8), JsonError> {
        let tag = self.varint()?;
        let number = tag >> 3;
        if !(1..=MAX_FIELD_NUMBER).contains(&number) {
            return Err(corrupt());
        }
        Ok((number as u32, (tag & 7) as u8))
    }

    /// `protowire.ConsumeFieldValue`.
    fn skip(&mut self, number: u32, wire_type: u8, depth: u32) -> Result<(), JsonError> {
        match wire_type {
            0 => {
                self.varint()?;
            }
            1 => {
                self.fixed::<8>()?;
            }
            2 => {
                self.bytes()?;
            }
            5 => {
                self.fixed::<4>()?;
            }
            3 => {
                if depth == 0 {
                    return Err(corrupt());
                }
                loop {
                    let (n, wt) = self.tag()?;
                    if wt == 4 {
                        if n != number {
                            return Err(corrupt());
                        }
                        return Ok(());
                    }
                    self.skip(n, wt, depth - 1)?;
                }
            }
            _ => return Err(corrupt()),
        }
        Ok(())
    }
}

/// Decodes `bytes` as a message of type `desc` the way a Go generated type
/// does with `AllowPartial`: unknown fields and known fields with another
/// wire type are dropped, a repeated scalar is read packed or not, the last
/// value of a singular field wins and messages merge.
pub(crate) fn decode(
    desc: &MessageDescriptor,
    bytes: &[u8],
    depth: u32,
) -> Result<DynamicMessage, JsonError> {
    let mut msg = DynamicMessage::new(desc.clone());
    let mut r = Reader { buf: bytes, at: 0 };
    merge(&mut msg, &mut r, None, depth)?;
    Ok(msg)
}

fn merge(
    msg: &mut DynamicMessage,
    r: &mut Reader,
    group: Option<u32>,
    depth: u32,
) -> Result<(), JsonError> {
    if depth == 0 {
        return Err(JsonError::recursion());
    }
    let desc = msg.descriptor();
    while r.at < r.buf.len() {
        let (number, wt) = r.tag()?;
        if wt == 4 {
            return if group == Some(number) {
                Ok(())
            } else {
                Err(corrupt())
            };
        }
        let field = match desc.get_field(number) {
            Some(fd) => Some(Field::Plain(fd)),
            None => desc.get_extension(number).map(Field::Extension),
        };
        let consumed = match field {
            Some(field) => merge_field(msg, &field, wt, r, depth)?,
            None => false,
        };
        if !consumed {
            r.skip(number, wt, depth)?;
        }
    }
    if group.is_some() {
        return Err(corrupt());
    }
    Ok(())
}

fn scalar_from_varint(kind: &Kind, v: u64) -> Value {
    match kind {
        Kind::Int32 => Value::I32(v as i32),
        Kind::Sint32 => {
            let v = v as u32;
            Value::I32(((v >> 1) as i32) ^ -((v & 1) as i32))
        }
        Kind::Int64 => Value::I64(v as i64),
        Kind::Sint64 => Value::I64(((v >> 1) as i64) ^ -((v & 1) as i64)),
        Kind::Uint32 => Value::U32(v as u32),
        Kind::Uint64 => Value::U64(v),
        Kind::Bool => Value::Bool(v != 0),
        Kind::Enum(_) => Value::EnumNumber(v as i32),
        _ => unreachable!("not a varint kind"),
    }
}

/// One scalar of `kind` sent with wire type `wt`; `None` when the wire type
/// is not the kind's.
fn read_scalar(
    r: &mut Reader,
    kind: &Kind,
    wt: u8,
    proto3: bool,
) -> Result<Option<Value>, JsonError> {
    if wt != wire_type(kind) || matches!(kind, Kind::Message(_)) {
        return Ok(None);
    }
    Ok(Some(match kind {
        Kind::Double => Value::F64(f64::from_le_bytes(r.fixed()?)),
        Kind::Float => Value::F32(f32::from_le_bytes(r.fixed()?)),
        Kind::Fixed64 => Value::U64(u64::from_le_bytes(r.fixed()?)),
        Kind::Sfixed64 => Value::I64(i64::from_le_bytes(r.fixed()?)),
        Kind::Fixed32 => Value::U32(u32::from_le_bytes(r.fixed()?)),
        Kind::Sfixed32 => Value::I32(i32::from_le_bytes(r.fixed()?)),
        Kind::String => {
            let bytes = r.bytes()?;
            match std::str::from_utf8(bytes) {
                Ok(s) => Value::String(s.to_owned()),
                // proto2 does not validate, but protojson then fails to
                // write the string: the result is the same error.
                Err(_) => {
                    return Err(JsonError::marshal(if proto3 {
                        "proto: string field contains invalid UTF-8"
                    } else {
                        "proto: field contains invalid UTF-8"
                    }));
                }
            }
        }
        Kind::Bytes => Value::Bytes(r.bytes()?.to_vec().into()),
        _ => scalar_from_varint(kind, r.varint()?),
    }))
}

fn is_packable(kind: &Kind) -> bool {
    !matches!(kind, Kind::String | Kind::Bytes | Kind::Message(_))
}

fn value_mut<'m>(msg: &'m mut DynamicMessage, field: &Field) -> &'m mut Value {
    match field {
        Field::Plain(fd) => msg.get_field_mut(fd),
        Field::Extension(ext) => msg.get_extension_mut(ext),
    }
}

/// Merges one field; `false` when the wire type makes it unknown.
fn merge_field(
    msg: &mut DynamicMessage,
    field: &Field,
    wt: u8,
    r: &mut Reader,
    depth: u32,
) -> Result<bool, JsonError> {
    let kind = field.kind();
    let proto3 = field.is_proto3();
    if field.is_map() {
        if wt != 2 {
            return Ok(false);
        }
        let Kind::Message(entry) = &kind else {
            unreachable!("map entry")
        };
        let (kf, vf) = (entry.map_entry_key_field(), entry.map_entry_value_field());
        let body = r.bytes()?;
        let mut er = Reader { buf: body, at: 0 };
        let mut key = kf.default_value();
        let mut val: Option<Value> = None;
        while er.at < er.buf.len() {
            let (n, ewt) = er.tag()?;
            let done = match n {
                1 => match read_scalar(&mut er, &kf.kind(), ewt, proto3)? {
                    Some(k) => {
                        key = k;
                        true
                    }
                    None => false,
                },
                2 => merge_map_value(&vf, &mut val, ewt, &mut er, depth, proto3)?,
                _ => false,
            };
            if !done {
                er.skip(n, ewt, depth)?;
            }
        }
        let val = val.unwrap_or_else(|| vf.default_value());
        let key = key.into_map_key().expect("map key kind");
        value_mut(msg, field)
            .as_map_mut()
            .expect("map")
            .insert(key, val);
        return Ok(true);
    }
    if let Kind::Message(sub) = &kind {
        let want = if field.is_group() { 3 } else { 2 };
        if wt != want {
            return Ok(false);
        }
        let merge_into = |target: &mut DynamicMessage, r: &mut Reader| {
            if field.is_group() {
                merge(target, r, Some(field.number()), depth - 1)
            } else {
                let body = r.bytes()?;
                merge(target, &mut Reader { buf: body, at: 0 }, None, depth - 1)
            }
        };
        if field.is_list() {
            let mut item = DynamicMessage::new(sub.clone());
            merge_into(&mut item, r)?;
            value_mut(msg, field)
                .as_list_mut()
                .expect("list")
                .push(Value::Message(item));
        } else {
            let has = match field {
                Field::Plain(fd) => msg.has_field(fd),
                Field::Extension(ext) => msg.has_extension(ext),
            };
            if !has {
                set(msg, field, Value::Message(DynamicMessage::new(sub.clone())));
            }
            let target = value_mut(msg, field).as_message_mut().expect("message");
            merge_into(target, r)?;
        }
        return Ok(true);
    }
    if field.is_list() {
        if wt == 2 && is_packable(&kind) {
            let body = r.bytes()?;
            let mut pr = Reader { buf: body, at: 0 };
            let mut items = Vec::new();
            while pr.at < pr.buf.len() {
                items.push(read_scalar(&mut pr, &kind, wire_type(&kind), proto3)?.expect("kind"));
            }
            value_mut(msg, field)
                .as_list_mut()
                .expect("list")
                .extend(items);
            return Ok(true);
        }
        return Ok(match read_scalar(r, &kind, wt, proto3)? {
            Some(v) => {
                value_mut(msg, field).as_list_mut().expect("list").push(v);
                true
            }
            None => false,
        });
    }
    Ok(match read_scalar(r, &kind, wt, proto3)? {
        Some(v) => {
            set(msg, field, v);
            true
        }
        None => false,
    })
}

fn merge_map_value(
    vf: &FieldDescriptor,
    val: &mut Option<Value>,
    wt: u8,
    r: &mut Reader,
    depth: u32,
    proto3: bool,
) -> Result<bool, JsonError> {
    if let Kind::Message(sub) = vf.kind() {
        if wt != 2 {
            return Ok(false);
        }
        let body = r.bytes()?;
        let target = val.get_or_insert_with(|| Value::Message(DynamicMessage::new(sub.clone())));
        merge(
            target.as_message_mut().expect("message"),
            &mut Reader { buf: body, at: 0 },
            None,
            depth - 1,
        )?;
        return Ok(true);
    }
    Ok(match read_scalar(r, &vf.kind(), wt, proto3)? {
        Some(v) => {
            *val = Some(v);
            true
        }
        None => false,
    })
}

fn set(msg: &mut DynamicMessage, field: &Field, value: Value) {
    match field {
        Field::Plain(fd) => msg.set_field(fd, value),
        Field::Extension(ext) => msg.set_extension(ext, value),
    }
}
