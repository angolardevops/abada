//! protojson's `unmarshal` (`encoding/protojson/decode.go` and the decoding
//! half of `well_known_types.go`), ported function for function onto
//! `DynamicMessage`.

use prost::Message as _;
use prost_reflect::{
    DynamicMessage, Kind, MapKey, MessageDescriptor, OneofDescriptor, ReflectMessage, Value,
};

use super::token::{Decoder as Tokens, Kind as Tok, Token};
use super::{Field, JsonError, Marshaler, Wkt, go, is_known_value, is_null_value, wire, wkt};

/// `UnmarshalOptions.unmarshal`: reset, decode one value, require the end.
pub(crate) fn unmarshal(
    marshaler: &Marshaler,
    msg: &mut DynamicMessage,
    json: &[u8],
) -> Result<(), JsonError> {
    msg.clear();
    let mut d = Decoder {
        tokens: Tokens::new(json),
        marshaler,
        discard_unknown: marshaler.unmarshal.discard_unknown,
    };
    let limit = marshaler.unmarshal.recursion_limit as i64;
    d.message(msg, false, limit)?;
    let tok = d.tokens.read()?;
    if tok.kind != Tok::Eof {
        return Err(unexpected(&tok));
    }
    check_initialized(msg)
}

fn unexpected(tok: &Token) -> JsonError {
    JsonError::syntax(format!(
        "syntax error (offset {}): unexpected token {}",
        tok.pos,
        tok.raw_str()
    ))
}

fn invalid(tok: &Token, what: impl std::fmt::Display) -> JsonError {
    JsonError::invalid(format!("(offset {}): {what}", tok.pos))
}

pub(crate) struct Decoder<'a, 'm> {
    pub(crate) tokens: Tokens<'a>,
    marshaler: &'m Marshaler,
    discard_unknown: bool,
}

impl<'a> Decoder<'a, '_> {
    /// `unmarshalMessage`. `limit` is the remaining recursion budget, which Go
    /// carries in a copy of the options for each nested call.
    fn message(
        &mut self,
        m: &mut DynamicMessage,
        skip_type_url: bool,
        limit: i64,
    ) -> Result<(), JsonError> {
        let limit = limit - 1;
        if limit < 0 {
            return Err(JsonError::recursion());
        }
        let desc = m.descriptor();
        if let Some(w) = wkt(&desc) {
            return self.well_known(w, m, limit);
        }
        let tok = self.tokens.read()?;
        if tok.kind != Tok::ObjectOpen {
            return Err(unexpected(&tok));
        }
        let mut seen_numbers: Vec<u32> = Vec::new();
        let mut seen_oneofs: Vec<OneofDescriptor> = Vec::new();
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose => return Ok(()),
                Tok::Name => {}
                _ => return Err(unexpected(&tok)),
            }
            let name = tok.text.as_ref();
            if skip_type_url && name == "@type" {
                let _ = self.tokens.read();
                continue;
            }
            let field = if name.len() >= 2 && name.starts_with('[') && name.ends_with(']') {
                self.extension(&desc, &tok, &name[1..name.len() - 1])?
            } else {
                desc.get_field_by_json_name(name)
                    .or_else(|| desc.get_field_by_name(name))
                    .map(Field::Plain)
            };
            let Some(field) = field else {
                if self.discard_unknown {
                    self.skip_value(limit)?;
                    continue;
                }
                return Err(invalid(
                    &tok,
                    format_args!("unknown field {}", tok.raw_str()),
                ));
            };
            let number = field.number();
            if seen_numbers.contains(&number) {
                return Err(invalid(
                    &tok,
                    format_args!("duplicate field {}", tok.raw_str()),
                ));
            }
            seen_numbers.push(number);

            let kind = field.kind();
            if self.tokens.peek().is_ok_and(|t| t.kind == Tok::Null)
                && !is_known_value(&kind)
                && !is_null_value(&kind)
            {
                self.tokens.read()?;
                continue;
            }
            if field.is_list() {
                let mut items = Vec::new();
                self.list(&mut items, &kind, limit)?;
                value_mut(m, &field)
                    .as_list_mut()
                    .expect("list")
                    .extend(items);
            } else if field.is_map() {
                let Kind::Message(entry) = &kind else {
                    unreachable!("map entry")
                };
                let map = value_mut(m, &field).as_map_mut().expect("map");
                let mut local = std::mem::take(map);
                let result = self.map(&mut local, entry, limit);
                *value_mut(m, &field).as_map_mut().expect("map") = local;
                result?;
            } else {
                if let Field::Plain(fd) = &field {
                    if let Some(oneof) = fd.containing_oneof() {
                        if seen_oneofs.contains(&oneof) {
                            return Err(invalid(
                                &tok,
                                format_args!(
                                    "error parsing {}, oneof {} is already set",
                                    tok.raw_str(),
                                    oneof.full_name()
                                ),
                            ));
                        }
                        seen_oneofs.push(oneof);
                    }
                }
                let value = match &kind {
                    Kind::Message(sub) => {
                        let mut sub = DynamicMessage::new(sub.clone());
                        self.message(&mut sub, false, limit)?;
                        Some(Value::Message(sub))
                    }
                    _ => self.scalar(&kind, &field_json_name(&field))?,
                };
                if let Some(value) = value {
                    set(m, &field, value);
                }
            }
        }
    }

    /// The `[full.name]` branch: an extension of this message, or nothing.
    fn extension(
        &self,
        desc: &MessageDescriptor,
        tok: &Token,
        name: &str,
    ) -> Result<Option<Field>, JsonError> {
        let pool = desc.parent_pool();
        if let Some(ext) = pool.get_extension_by_name(name) {
            let in_range = desc.extension_ranges().any(|r| r.contains(&ext.number()));
            if !in_range || ext.containing_message().full_name() != desc.full_name() {
                return Err(invalid(
                    tok,
                    format_args!(
                        "message {} cannot be extended by {}",
                        desc.full_name(),
                        ext.full_name()
                    ),
                ));
            }
            return Ok(Some(Field::Extension(ext)));
        }
        // The Go registry holds messages and enums under the same names; one
        // found there is "the wrong type", an error, not an unknown field.
        if pool.get_message_by_name(name).is_some() || pool.get_enum_by_name(name).is_some() {
            return Err(JsonError::unresolvable(format!(
                "unable to resolve {}: found wrong type",
                tok.raw_str()
            )));
        }
        Ok(None)
    }

    /// `unmarshalScalar`: `None` for an unknown enum name that is discarded.
    pub(crate) fn scalar(
        &mut self,
        kind: &Kind,
        json_name: &str,
    ) -> Result<Option<Value>, JsonError> {
        let tok = self.tokens.read()?;
        let value = match kind {
            Kind::Bool => (tok.kind == Tok::Bool).then_some(Value::Bool(tok.boolean)),
            Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
                int(&tok, 32).map(|v| Value::I32(v as i32))
            }
            Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => int(&tok, 64).map(Value::I64),
            Kind::Uint32 | Kind::Fixed32 => uint(&tok, 32).map(|v| Value::U32(v as u32)),
            Kind::Uint64 | Kind::Fixed64 => uint(&tok, 64).map(Value::U64),
            Kind::Float => float(&tok, 32).map(|v| Value::F32(v as f32)),
            Kind::Double => float(&tok, 64).map(Value::F64),
            Kind::String => {
                (tok.kind == Tok::String).then(|| Value::String(tok.text.clone().into_owned()))
            }
            Kind::Bytes => bytes(&tok).map(|b| Value::Bytes(b.into())),
            Kind::Enum(e) => match tok.kind {
                Tok::String => match e.get_value_by_name(&tok.text) {
                    Some(v) => Some(Value::EnumNumber(v.number())),
                    None if self.discard_unknown => return Ok(None),
                    None => None,
                },
                Tok::Number => tok.int(32).map(|n| Value::EnumNumber(n as i32)),
                Tok::Null if is_null_value(kind) => Some(Value::EnumNumber(0)),
                _ => None,
            },
            Kind::Message(_) => unreachable!("messages are not scalars"),
        };
        match value {
            Some(v) => Ok(Some(v)),
            None => Err(invalid(
                &tok,
                format_args!(
                    "invalid value for {} field {}: {}",
                    kind_name(kind),
                    json_name,
                    tok.raw_str()
                ),
            )),
        }
    }

    /// `unmarshalList`.
    fn list(&mut self, items: &mut Vec<Value>, kind: &Kind, limit: i64) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::ArrayOpen {
            return Err(unexpected(&tok));
        }
        loop {
            if self.tokens.peek()?.kind == Tok::ArrayClose {
                self.tokens.read()?;
                return Ok(());
            }
            match kind {
                Kind::Message(sub) => {
                    let mut item = DynamicMessage::new(sub.clone());
                    self.message(&mut item, false, limit)?;
                    items.push(Value::Message(item));
                }
                _ => {
                    if let Some(v) = self.scalar(kind, "")? {
                        items.push(v);
                    }
                }
            }
        }
    }

    /// `unmarshalMap`.
    fn map(
        &mut self,
        map: &mut std::collections::HashMap<MapKey, Value>,
        entry: &MessageDescriptor,
        limit: i64,
    ) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::ObjectOpen {
            return Err(unexpected(&tok));
        }
        let key_kind = entry.map_entry_key_field().kind();
        let value_kind = entry.map_entry_value_field().kind();
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose => return Ok(()),
                Tok::Name => {}
                _ => return Err(unexpected(&tok)),
            }
            let key = map_key(&tok, &key_kind)?;
            if map.contains_key(&key) {
                return Err(invalid(
                    &tok,
                    format_args!("duplicate map key {}", tok.raw_str()),
                ));
            }
            let value = match &value_kind {
                Kind::Message(sub) => {
                    let mut item = DynamicMessage::new(sub.clone());
                    self.message(&mut item, false, limit)?;
                    Some(Value::Message(item))
                }
                _ => self.scalar(&value_kind, "")?,
            };
            if let Some(value) = value {
                map.insert(key, value);
            }
        }
    }

    /// `skipJSONValue`.
    pub(crate) fn skip_value(&mut self, limit: i64) -> Result<(), JsonError> {
        let mut open = 0i64;
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose | Tok::ArrayClose => open -= 1,
                Tok::ObjectOpen | Tok::ArrayOpen => {
                    open += 1;
                    if open > limit {
                        return Err(JsonError::recursion());
                    }
                }
                Tok::Eof => return Err(JsonError::eof()),
                _ => {}
            }
            if open == 0 {
                return Ok(());
            }
        }
    }

    fn well_known(&mut self, w: Wkt, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        match w {
            Wkt::Any => self.any(m, limit),
            Wkt::Timestamp => self.timestamp(m),
            Wkt::Duration => self.duration(m),
            Wkt::Wrapper => self.wrapper(m),
            Wkt::Struct => self.structure(m, limit),
            Wkt::ListValue => self.list_value(m, limit),
            Wkt::Value => self.known_value(m, limit),
            Wkt::FieldMask => self.field_mask(m),
            Wkt::Empty => self.empty(limit),
        }
    }

    /// `unmarshalAny`.
    fn any(&mut self, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        let start = self.tokens.peek()?;
        if start.kind != Tok::ObjectOpen {
            return Err(unexpected(&start));
        }
        let mut scout = Decoder {
            tokens: self.tokens.clone(),
            marshaler: self.marshaler,
            discard_unknown: false,
        };
        let type_tok = match scout.find_type_url(limit)? {
            TypeUrl::Empty => {
                self.tokens.read()?;
                self.tokens.read()?;
                return Ok(());
            }
            TypeUrl::Missing => {
                if self.discard_unknown {
                    return self.skip_value(limit);
                }
                return Err(invalid(&start, "missing \"@type\" field"));
            }
            TypeUrl::Found(tok) => tok,
        };
        let url = type_tok.text.as_ref();
        let Some(desc) = self.marshaler.registry().find_message_by_url(url) else {
            return Err(JsonError::unresolvable(format!(
                "(offset {}): unable to resolve {}: not found",
                type_tok.pos,
                type_tok.raw_str()
            )));
        };
        let mut embedded = DynamicMessage::new(desc.clone());
        match wkt(&desc) {
            Some(w) => self.any_value(w, &mut embedded, limit)?,
            None => self.message(&mut embedded, true, limit)?,
        }
        let value = wire::encode(&embedded);
        let fields = m.descriptor();
        m.set_field(
            &fields.get_field(1).expect("Any.type_url"),
            Value::String(url.to_owned()),
        );
        m.set_field(
            &fields.get_field(2).expect("Any.value"),
            Value::Bytes(value.into()),
        );
        Ok(())
    }

    /// `findTypeURL`, run on a copy of the decoder.
    fn find_type_url(&mut self, limit: i64) -> Result<TypeUrl<'a>, JsonError> {
        let mut found: Option<Token<'a>> = None;
        let mut fields = 0;
        self.tokens.read()?;
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose => {
                    return Ok(match found {
                        Some(t) => TypeUrl::Found(t),
                        None if fields > 0 => TypeUrl::Missing,
                        None => TypeUrl::Empty,
                    });
                }
                Tok::Name => {
                    fields += 1;
                    if tok.text != "@type" {
                        self.skip_value(limit)?;
                        continue;
                    }
                    if found.is_some() {
                        return Err(invalid(&tok, "duplicate \"@type\" field"));
                    }
                    let value = self.tokens.read()?;
                    if value.kind != Tok::String {
                        return Err(invalid(
                            &value,
                            format_args!("@type field value is not a string: {}", value.raw_str()),
                        ));
                    }
                    if value.text.is_empty() {
                        return Err(invalid(&value, "@type field contains empty value"));
                    }
                    found = Some(value);
                }
                _ => {}
            }
        }
    }

    /// `unmarshalAnyValue`: the `value` field of an `Any` holding a
    /// well-known type.
    fn any_value(&mut self, w: Wkt, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        self.tokens.read()?;
        let mut found = false;
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose => {
                    if !found && w != Wkt::Empty {
                        return Err(invalid(&tok, "missing \"value\" field"));
                    }
                    return Ok(());
                }
                Tok::Name => match tok.text.as_ref() {
                    "@type" => {
                        let _ = self.tokens.read();
                    }
                    "value" => {
                        if found {
                            return Err(invalid(&tok, "duplicate \"value\" field"));
                        }
                        self.well_known(w, m, limit)?;
                        found = true;
                    }
                    _ => {
                        if self.discard_unknown {
                            self.skip_value(limit)?;
                            continue;
                        }
                        return Err(invalid(
                            &tok,
                            format_args!("unknown field {}", tok.raw_str()),
                        ));
                    }
                },
                _ => {}
            }
        }
    }

    /// `unmarshalWrapperType`.
    fn wrapper(&mut self, m: &mut DynamicMessage) -> Result<(), JsonError> {
        let fd = m.descriptor().get_field(1).expect("wrapper value");
        if let Some(v) = self.scalar(&fd.kind(), fd.json_name())? {
            m.set_field(&fd, v);
        }
        Ok(())
    }

    /// `unmarshalEmpty`.
    fn empty(&mut self, limit: i64) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::ObjectOpen {
            return Err(unexpected(&tok));
        }
        loop {
            let tok = self.tokens.read()?;
            match tok.kind {
                Tok::ObjectClose => return Ok(()),
                Tok::Name => {
                    if self.discard_unknown {
                        self.skip_value(limit)?;
                        continue;
                    }
                    return Err(invalid(
                        &tok,
                        format_args!("unknown field {}", tok.raw_str()),
                    ));
                }
                _ => return Err(unexpected(&tok)),
            }
        }
    }

    /// `unmarshalStruct`.
    fn structure(&mut self, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        let fd = m.descriptor().get_field(1).expect("Struct.fields");
        let Kind::Message(entry) = fd.kind() else {
            unreachable!("map entry")
        };
        let mut local = std::mem::take(m.get_field_mut(&fd).as_map_mut().expect("map"));
        let result = self.map(&mut local, &entry, limit);
        *m.get_field_mut(&fd).as_map_mut().expect("map") = local;
        result
    }

    /// `unmarshalListValue`.
    fn list_value(&mut self, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        let fd = m.descriptor().get_field(1).expect("ListValue.values");
        let mut items = Vec::new();
        let result = self.list(&mut items, &fd.kind(), limit);
        m.get_field_mut(&fd)
            .as_list_mut()
            .expect("list")
            .extend(items);
        result
    }

    /// `unmarshalKnownValue`.
    fn known_value(&mut self, m: &mut DynamicMessage, limit: i64) -> Result<(), JsonError> {
        let tok = self.tokens.peek()?;
        let desc = m.descriptor();
        let field = |n| desc.get_field(n).expect("Value field");
        let (fd, value) = match tok.kind {
            Tok::Null => {
                self.tokens.read()?;
                (field(1), Value::EnumNumber(0))
            }
            Tok::Bool => {
                let tok = self.tokens.read()?;
                (field(4), Value::Bool(tok.boolean))
            }
            Tok::Number => {
                let tok = self.tokens.read()?;
                let Some(v) = float(&tok, 64) else {
                    return Err(invalid(
                        &tok,
                        format_args!("invalid google.protobuf.Value: {}", tok.raw_str()),
                    ));
                };
                (field(2), Value::F64(v))
            }
            Tok::String => {
                let tok = self.tokens.read()?;
                (field(3), Value::String(tok.text.into_owned()))
            }
            Tok::ObjectOpen => {
                let fd = field(5);
                let Kind::Message(sd) = fd.kind() else {
                    unreachable!("Struct")
                };
                let mut sub = DynamicMessage::new(sd);
                self.structure(&mut sub, limit)?;
                (fd, Value::Message(sub))
            }
            Tok::ArrayOpen => {
                let fd = field(6);
                let Kind::Message(ld) = fd.kind() else {
                    unreachable!("ListValue")
                };
                let mut sub = DynamicMessage::new(ld);
                self.list_value(&mut sub, limit)?;
                (fd, Value::Message(sub))
            }
            _ => {
                return Err(invalid(
                    &tok,
                    format_args!("invalid google.protobuf.Value: {}", tok.raw_str()),
                ));
            }
        };
        m.set_field(&fd, value);
        Ok(())
    }

    /// `unmarshalDuration`.
    fn duration(&mut self, m: &mut DynamicMessage) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::String {
            return Err(unexpected(&tok));
        }
        let Some((secs, nanos)) = parse_duration(tok.text.as_bytes()) else {
            return Err(invalid(
                &tok,
                format_args!("invalid google.protobuf.Duration value {}", tok.raw_str()),
            ));
        };
        if !(-MAX_DURATION_SECONDS..=MAX_DURATION_SECONDS).contains(&secs) {
            return Err(invalid(
                &tok,
                format_args!(
                    "google.protobuf.Duration value out of range: {}",
                    tok.raw_str()
                ),
            ));
        }
        let desc = m.descriptor();
        m.set_field(&desc.get_field(1).expect("seconds"), Value::I64(secs));
        m.set_field(&desc.get_field(2).expect("nanos"), Value::I32(nanos));
        Ok(())
    }

    /// `unmarshalTimestamp`.
    fn timestamp(&mut self, m: &mut DynamicMessage) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::String {
            return Err(unexpected(&tok));
        }
        let s = tok.text.as_bytes();
        let bad = || {
            invalid(
                &tok,
                format_args!("invalid google.protobuf.Timestamp value {}", tok.raw_str()),
            )
        };
        let (secs, nanos) = go::parse_rfc3339(s).ok_or_else(bad)?;
        if !(MIN_TIMESTAMP_SECONDS..=MAX_TIMESTAMP_SECONDS).contains(&secs) {
            return Err(invalid(
                &tok,
                format_args!(
                    "google.protobuf.Timestamp value out of range: {}",
                    tok.raw_str()
                ),
            ));
        }
        let i = s.iter().rposition(|&c| c == b'.');
        let j = s.iter().rposition(|&c| matches!(c, b'Z' | b'-' | b'+'));
        if let (Some(i), Some(j)) = (i, j) {
            if j >= i && j - i > ".999999999".len() {
                return Err(bad());
            }
        }
        let desc = m.descriptor();
        m.set_field(&desc.get_field(1).expect("seconds"), Value::I64(secs));
        m.set_field(&desc.get_field(2).expect("nanos"), Value::I32(nanos));
        Ok(())
    }

    /// `unmarshalFieldMask`.
    fn field_mask(&mut self, m: &mut DynamicMessage) -> Result<(), JsonError> {
        let tok = self.tokens.read()?;
        if tok.kind != Tok::String {
            return Err(unexpected(&tok));
        }
        let s = go_trim_space(&tok.text);
        if s.is_empty() {
            return Ok(());
        }
        let mut paths = Vec::new();
        for s0 in s.split(',') {
            let snake = json_snake_case(s0);
            if s0.contains('_') || !is_valid_full_name(&snake) {
                return Err(invalid(
                    &tok,
                    format_args!("google.protobuf.FieldMask.paths contains invalid path: {s0:?}"),
                ));
            }
            paths.push(Value::String(snake));
        }
        let fd = m.descriptor().get_field(1).expect("paths");
        m.get_field_mut(&fd)
            .as_list_mut()
            .expect("list")
            .extend(paths);
        Ok(())
    }
}

enum TypeUrl<'a> {
    Empty,
    Missing,
    Found(Token<'a>),
}

pub(crate) const MAX_DURATION_SECONDS: i64 = 315_576_000_000;
pub(crate) const MIN_TIMESTAMP_SECONDS: i64 = -62_135_596_800;
pub(crate) const MAX_TIMESTAMP_SECONDS: i64 = 253_402_300_799;

fn field_json_name(field: &Field) -> String {
    match field {
        Field::Plain(f) => f.json_name().to_owned(),
        Field::Extension(e) => e.json_name().to_owned(),
    }
}

fn kind_name(kind: &Kind) -> &'static str {
    match kind {
        Kind::Double => "double",
        Kind::Float => "float",
        Kind::Int32 => "int32",
        Kind::Int64 => "int64",
        Kind::Uint32 => "uint32",
        Kind::Uint64 => "uint64",
        Kind::Sint32 => "sint32",
        Kind::Sint64 => "sint64",
        Kind::Fixed32 => "fixed32",
        Kind::Fixed64 => "fixed64",
        Kind::Sfixed32 => "sfixed32",
        Kind::Sfixed64 => "sfixed64",
        Kind::Bool => "bool",
        Kind::String => "string",
        Kind::Bytes => "bytes",
        Kind::Message(_) => "message",
        Kind::Enum(_) => "enum",
    }
}

fn value_mut<'m>(msg: &'m mut DynamicMessage, field: &Field) -> &'m mut Value {
    match field {
        Field::Plain(fd) => msg.get_field_mut(fd),
        Field::Extension(ext) => msg.get_extension_mut(ext),
    }
}

fn set(msg: &mut DynamicMessage, field: &Field, value: Value) {
    match field {
        Field::Plain(fd) => msg.set_field(fd, value),
        Field::Extension(ext) => msg.set_extension(ext, value),
    }
}

/// A JSON string that holds a number, re-read by a fresh tokenizer:
/// surrounding space is refused, and only the first token counts.
fn number_in_string<'t>(tok: &'t Token) -> Option<Token<'t>> {
    let s = tok.text.as_bytes();
    if go_trim_space(&tok.text).len() != s.len() {
        return None;
    }
    Tokens::new(s).read().ok()
}

fn int(tok: &Token, bits: u32) -> Option<i64> {
    match tok.kind {
        Tok::Number => tok.int(bits),
        Tok::String => number_in_string(tok)?.int(bits),
        _ => None,
    }
}

fn uint(tok: &Token, bits: u32) -> Option<u64> {
    match tok.kind {
        Tok::Number => tok.uint(bits),
        Tok::String => number_in_string(tok)?.uint(bits),
        _ => None,
    }
}

fn float(tok: &Token, bits: u32) -> Option<f64> {
    match tok.kind {
        Tok::Number => tok.float(bits),
        Tok::String => match tok.text.as_ref() {
            // `math.NaN()`, whose bits reach the binary form.
            "NaN" => Some(f64::from_bits(0x7ff8_0000_0000_0001)),
            "Infinity" => Some(f64::INFINITY),
            "-Infinity" => Some(f64::NEG_INFINITY),
            _ => number_in_string(tok)?.float(bits),
        },
        _ => None,
    }
}

/// `unmarshalBytes`: standard or URL alphabet, padded or not.
fn bytes(tok: &Token) -> Option<Vec<u8>> {
    if tok.kind != Tok::String {
        return None;
    }
    let s = tok.text.as_bytes();
    go::Base64 {
        url: s.iter().any(|&c| c == b'-' || c == b'_'),
        padding: s.len() % 4 == 0,
    }
    .decode(s)
}

/// `unmarshalMapKey`.
fn map_key(tok: &Token, kind: &Kind) -> Result<MapKey, JsonError> {
    let name = tok.text.as_bytes();
    let key = match kind {
        Kind::String => Some(MapKey::String(tok.text.clone().into_owned())),
        Kind::Bool => match name {
            b"true" => Some(MapKey::Bool(true)),
            b"false" => Some(MapKey::Bool(false)),
            _ => None,
        },
        Kind::Int32 | Kind::Sint32 | Kind::Sfixed32 => {
            go::parse_int(name, 10, 32).map(|v| MapKey::I32(v as i32))
        }
        Kind::Int64 | Kind::Sint64 | Kind::Sfixed64 => go::parse_int(name, 10, 64).map(MapKey::I64),
        Kind::Uint32 | Kind::Fixed32 => go::parse_uint(name, 10, 32).map(|v| MapKey::U32(v as u32)),
        Kind::Uint64 | Kind::Fixed64 => go::parse_uint(name, 10, 64).map(MapKey::U64),
        _ => unreachable!("invalid map key kind"),
    };
    key.ok_or_else(|| {
        invalid(
            tok,
            format_args!(
                "invalid value for {} key: {}",
                kind_name(kind),
                tok.raw_str()
            ),
        )
    })
}

/// `strings.TrimSpace`, for the ASCII space characters the protojson
/// inputs can hold, and the Unicode ones Go also trims.
pub(crate) fn go_trim_space(s: &str) -> &str {
    s.trim_matches(|c: char| {
        matches!(
            c,
            '\t' | '\n' | '\u{b}' | '\u{c}' | '\r' | ' ' | '\u{85}' | '\u{a0}'
        ) || (c as u32 > 0xff && c.is_whitespace())
    })
}

/// `strs.JSONSnakeCase`.
pub(crate) fn json_snake_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for c in s.bytes() {
        if c.is_ascii_uppercase() {
            out.push('_');
            out.push(c.to_ascii_lowercase() as char);
        } else {
            out.push(c as char);
        }
    }
    out
}

/// `strs.JSONCamelCase`.
pub(crate) fn json_camel_case(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut was_underscore = false;
    for c in s.bytes() {
        if c != b'_' {
            if was_underscore && c.is_ascii_lowercase() {
                out.push(c.to_ascii_uppercase() as char);
            } else {
                out.push(c as char);
            }
        }
        was_underscore = c == b'_';
    }
    out
}

/// `protoreflect.FullName.IsValid`.
pub(crate) fn is_valid_full_name(s: &str) -> bool {
    let s = s.as_bytes();
    let letter = |c: u8| c == b'_' || c.is_ascii_alphabetic();
    let mut i = 0;
    loop {
        if i >= s.len() || !letter(s[i]) {
            return false;
        }
        i += 1;
        while i < s.len() && (letter(s[i]) || s[i].is_ascii_digit()) {
            i += 1;
        }
        if i == s.len() {
            return true;
        }
        if s[i] != b'.' {
            return false;
        }
        i += 1;
    }
}

/// `parseDuration`.
fn parse_duration(input: &[u8]) -> Option<(i64, i32)> {
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
    let mut intp: &[u8] = b"";
    match b[0] {
        b'0' => b = &b[1..],
        b'1'..=b'9' => {
            let n = 1 + b[1..].iter().take_while(|c| c.is_ascii_digit()).count();
            intp = &b[..n];
            b = &b[n..];
        }
        b'.' => {}
        _ => return None,
    }
    let mut frac = [b'0'; 9];
    let mut has_frac = false;
    if !b.is_empty() {
        if b[0] != b'.' {
            return None;
        }
        b = &b[1..];
        let mut n = 0;
        while !b.is_empty() && n < 9 && b[0].is_ascii_digit() {
            frac[n] = b[0];
            n += 1;
            b = &b[1..];
        }
        if !b.is_empty() {
            return None;
        }
        has_frac = true;
    }
    let mut secs = 0i64;
    if !intp.is_empty() {
        secs = go::parse_int(intp, 10, 64)?;
    }
    let mut nanos = 0i64;
    if has_frac {
        let start = frac.iter().position(|&c| c != b'0').unwrap_or(9);
        if start < 9 {
            nanos = go::parse_int(&frac[start..], 10, 32)?;
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

/// `proto.CheckInitialized`: every required field of every message present
/// is set. Unknown fields and `Any` payloads are not looked into.
pub(crate) fn check_initialized(msg: &DynamicMessage) -> Result<(), JsonError> {
    let desc = msg.descriptor();
    for fd in desc.fields() {
        if fd.is_required() && !msg.has_field(&fd) {
            return Err(JsonError::invalid(format!(
                "proto: required field {} not set",
                fd.full_name()
            )));
        }
        if !matches!(fd.kind(), Kind::Message(_)) || !msg.has_field(&fd) {
            continue;
        }
        match &*msg.get_field(&fd) {
            Value::Message(m) => check_initialized(m)?,
            Value::List(items) => {
                for item in items {
                    if let Value::Message(m) = item {
                        check_initialized(m)?;
                    }
                }
            }
            Value::Map(map) => {
                for item in map.values() {
                    if let Value::Message(m) = item {
                        check_initialized(m)?;
                    }
                }
            }
            _ => {}
        }
    }
    for (_, value) in msg.extensions() {
        match value {
            Value::Message(m) => check_initialized(m)?,
            Value::List(items) => {
                for item in items {
                    if let Value::Message(m) = item {
                        check_initialized(m)?;
                    }
                }
            }
            _ => {}
        }
    }
    Ok(())
}
