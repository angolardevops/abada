//! JSON transcoding, as grpc-gateway v2.27.3's default marshaler does it.
//!
//! A `runtime.ServeMux` marshals with `runtime.JSONPb{MarshalOptions:
//! {EmitUnpopulated: true}, UnmarshalOptions: {DiscardUnknown: true}}`
//! (`runtime/marshaler_registry.go`): protojson over the message, lowerCamelCase
//! names, enums by name, 64-bit integers as strings. [`Marshaler::default`]
//! is that marshaler, built on `prost-reflect`'s `DynamicMessage` and driven
//! by the descriptor. Its behaviour is not re-derived from the proto3 JSON
//! specification: it is protojson's and `encoding/json`'s code, ported and
//! measured against grpc-gateway by `conformance/vectors/json-*.json`.
//!
//! Three entry points follow what the generated gateway does with a body:
//!
//! - `body: "*"` — [`Marshaler::decode_into`]: `Decode(&protoReq)`. An empty
//!   body leaves the message as it is; any other body **resets** the message
//!   first (`protojson.Unmarshal` calls `proto.Reset`). The generated handler
//!   decodes the body *before* it copies path and query values in, so
//!   whoever populates a request must decode the body first, or lose the
//!   path values.
//! - `body: "<field>"` — [`Marshaler::decode_field`]: `Decode(&protoReq.Field)`.
//!   grpc-gateway does **not** use protojson for a field that is not a
//!   message: `encoding/json` decodes into the generated Go type (an `int64`
//!   must be a JSON number, an enum must be a number, bytes are strictly
//!   padded base64). Message fields are reset; lists are replaced unless the
//!   body is `null`; maps are merged; a presence field is set even by `null`
//!   or an empty body.
//! - `response_body: "<field>"` — [`Marshaler::encode_field`]: the field's Go
//!   value through `JSONPb.marshalNonProtoField`, again `encoding/json` for
//!   anything but messages.
//!
//! One written deviation, with the vector that shows it (`depth_101`):
//! messages nest at most [`UnmarshalOptions::recursion_limit`] levels (100 by
//! default, prost's own limit for the binary form the body is sent in), where
//! Go allows 10 000. A deeper body is rejected instead of risking the stack.
//!
//! Where Go itself depends on the platform — `int32(f)` of an out-of-range
//! float, when a `body: "<field>"` enum is given as `3e9` — abada follows Go
//! on amd64, where the vectors were measured.

mod decode;
mod encode;
mod go;
mod registry;
mod stream;
mod token;
mod wire;

use std::fmt;

use prost_reflect::{
    DynamicMessage, ExtensionDescriptor, FieldDescriptor, Kind, MessageDescriptor, Syntax, Value,
};

pub use registry::TypeRegistry;

/// What went wrong, coarsely; the message is for people.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum JsonErrorKind {
    /// Not JSON, or not the JSON grammar protojson reads.
    Syntax,
    /// The input ended inside a value.
    UnexpectedEof,
    /// Valid JSON that does not fit the message: a wrong type, an integer out
    /// of range, a duplicate field, a missing required field.
    InvalidValue,
    /// An `Any` type URL or an extension name that the registry cannot resolve.
    Unresolvable,
    /// Messages nested deeper than the recursion limit.
    RecursionLimit,
    /// A message that cannot be written as JSON (an out-of-range
    /// `Timestamp`, a `Value` with no kind, an irreversible `FieldMask` path).
    Marshal,
    /// A shape grpc-gateway's generated code does not support either.
    Unsupported,
}

/// A JSON decoding or encoding failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JsonError {
    kind: JsonErrorKind,
    message: String,
}

impl JsonError {
    fn new(kind: JsonErrorKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
        }
    }

    pub(crate) fn syntax(message: impl Into<String>) -> Self {
        Self::new(JsonErrorKind::Syntax, message)
    }

    pub(crate) fn eof() -> Self {
        Self::new(JsonErrorKind::UnexpectedEof, "unexpected EOF")
    }

    pub(crate) fn invalid(message: impl Into<String>) -> Self {
        Self::new(JsonErrorKind::InvalidValue, message)
    }

    pub(crate) fn unresolvable(message: impl Into<String>) -> Self {
        Self::new(JsonErrorKind::Unresolvable, message)
    }

    pub(crate) fn recursion() -> Self {
        Self::new(
            JsonErrorKind::RecursionLimit,
            "exceeded max recursion depth",
        )
    }

    pub(crate) fn marshal(message: impl Into<String>) -> Self {
        Self::new(JsonErrorKind::Marshal, message)
    }

    pub(crate) fn unsupported(message: impl Into<String>) -> Self {
        Self::new(JsonErrorKind::Unsupported, message)
    }

    /// The class of the failure.
    pub fn kind(&self) -> JsonErrorKind {
        self.kind
    }
}

impl fmt::Display for JsonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JsonError {}

/// `protojson.MarshalOptions`, less what grpc-gateway never sets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarshalOptions {
    /// Write unset fields: `null` for messages and fields with presence,
    /// `[]`/`{}` for lists and maps, zero for the rest. Not unset oneof
    /// members, proto3 `optional` fields or extensions.
    pub emit_unpopulated: bool,
    /// Like `emit_unpopulated` without the `null`s.
    pub emit_default_values: bool,
    /// Field names as in the `.proto` instead of lowerCamelCase.
    pub use_proto_names: bool,
    /// Enum values as numbers instead of names.
    pub use_enum_numbers: bool,
}

/// `protojson.UnmarshalOptions`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnmarshalOptions {
    /// Skip unknown fields and unknown enum names instead of failing.
    pub discard_unknown: bool,
    /// Deepest message nesting accepted. See the module documentation.
    pub recursion_limit: u32,
}

/// The default message nesting limit.
pub const DEFAULT_RECURSION_LIMIT: u32 = 100;

/// grpc-gateway's `runtime.JSONPb` over a [`TypeRegistry`].
#[derive(Debug, Clone)]
pub struct Marshaler {
    /// Outbound options.
    pub marshal: MarshalOptions,
    /// Inbound options.
    pub unmarshal: UnmarshalOptions,
    registry: TypeRegistry,
}

impl Default for Marshaler {
    /// The default marshaler of a `runtime.ServeMux`, over the default
    /// registry (well-known types and `google.rpc.Status`).
    fn default() -> Self {
        Self::new(TypeRegistry::default())
    }
}

impl Marshaler {
    /// The default marshaler of a `runtime.ServeMux`: `EmitUnpopulated` on,
    /// `DiscardUnknown` on, everything else off.
    pub fn new(registry: TypeRegistry) -> Self {
        Self {
            marshal: MarshalOptions {
                emit_unpopulated: true,
                emit_default_values: false,
                use_proto_names: false,
                use_enum_numbers: false,
            },
            unmarshal: UnmarshalOptions {
                discard_unknown: true,
                recursion_limit: DEFAULT_RECURSION_LIMIT,
            },
            registry,
        }
    }

    /// The registry `Any` values are resolved with.
    pub fn registry(&self) -> &TypeRegistry {
        &self.registry
    }

    /// `JSONPb.ContentType`.
    pub fn content_type(&self) -> &'static str {
        "application/json"
    }

    /// A new message of type `desc` from a request body, as for `body: "*"`.
    pub fn decode(
        &self,
        desc: &MessageDescriptor,
        body: &[u8],
    ) -> Result<DynamicMessage, JsonError> {
        let mut msg = DynamicMessage::new(desc.clone());
        self.decode_into(&mut msg, body)?;
        Ok(msg)
    }

    /// `marshaler.NewDecoder(body).Decode(&protoReq)` with the generated
    /// handler's `io.EOF` check: the first JSON value of `body` replaces the
    /// message; an empty or blank body leaves it unchanged. Bytes after the
    /// first value are ignored, as `encoding/json`'s `Decoder` does. On error
    /// the message may be partly written.
    pub fn decode_into(&self, msg: &mut DynamicMessage, body: &[u8]) -> Result<(), JsonError> {
        let start = body
            .iter()
            .position(|c| !matches!(c, b' ' | b'\t' | b'\r' | b'\n'));
        match start {
            None => Ok(()),
            Some(i) if body[i] == b'{' => decode::unmarshal(self, msg, &body[i..], false),
            Some(_) => {
                let value = stream::first_value(body)?.expect("not blank");
                decode::unmarshal(self, msg, value, true)
            }
        }
    }

    /// `marshaler.NewDecoder(body).Decode(&protoReq.<Field>)` for a
    /// `body: "<field>"` binding. `field` must belong to `msg`'s type and
    /// not be a member of a (non-synthetic) oneof. On error the message may
    /// be partly written.
    pub fn decode_field(
        &self,
        msg: &mut DynamicMessage,
        field: &FieldDescriptor,
        body: &[u8],
    ) -> Result<(), JsonError> {
        stream::decode_field(self, msg, field, body)
    }

    /// `protojson.UnmarshalOptions.Unmarshal`: `json` must be exactly one
    /// JSON value; the message is reset first.
    pub fn unmarshal_into(&self, msg: &mut DynamicMessage, json: &[u8]) -> Result<(), JsonError> {
        decode::unmarshal(self, msg, json, true)
    }

    /// `JSONPb.Marshal` of a message: protojson, compact.
    pub fn encode(&self, msg: &DynamicMessage) -> Result<Vec<u8>, JsonError> {
        let mut out = Vec::with_capacity(128);
        encode::marshal(self, msg, &mut out)?;
        Ok(out)
    }

    /// `JSONPb.Marshal` of one field's value, for `response_body: "<field>"`.
    /// `field` must belong to `msg`'s type and not be a member of a
    /// (non-synthetic) oneof.
    pub fn encode_field(
        &self,
        msg: &DynamicMessage,
        field: &FieldDescriptor,
    ) -> Result<Vec<u8>, JsonError> {
        stream::encode_field(self, msg, field)
    }
}

/// A field or an extension, where protojson treats both alike.
#[derive(Clone, Debug)]
pub(crate) enum Field {
    Plain(FieldDescriptor),
    Extension(ExtensionDescriptor),
}

impl Field {
    pub(crate) fn number(&self) -> u32 {
        match self {
            Field::Plain(f) => f.number(),
            Field::Extension(e) => e.number(),
        }
    }

    pub(crate) fn kind(&self) -> Kind {
        match self {
            Field::Plain(f) => f.kind(),
            Field::Extension(e) => e.kind(),
        }
    }

    pub(crate) fn is_list(&self) -> bool {
        match self {
            Field::Plain(f) => f.is_list(),
            Field::Extension(e) => e.is_list(),
        }
    }

    pub(crate) fn is_map(&self) -> bool {
        match self {
            Field::Plain(f) => f.is_map(),
            Field::Extension(e) => e.is_map(),
        }
    }

    pub(crate) fn is_group(&self) -> bool {
        match self {
            Field::Plain(f) => f.is_group(),
            Field::Extension(e) => e.is_group(),
        }
    }

    pub(crate) fn is_packed(&self) -> bool {
        match self {
            Field::Plain(f) => f.is_packed(),
            Field::Extension(e) => e.is_packed(),
        }
    }

    pub(crate) fn is_proto3(&self) -> bool {
        let file = match self {
            Field::Plain(f) => f.parent_file(),
            Field::Extension(e) => e.parent_file(),
        };
        file.syntax() == Syntax::Proto3
    }
}

/// `protoreflect.Message.Has`: presence for fields that have it, non-empty
/// for lists and maps, non-zero otherwise, where `-0.0` is not zero.
pub(crate) fn go_has(msg: &DynamicMessage, fd: &FieldDescriptor) -> bool {
    if !msg.has_field(fd) {
        // prost-reflect counts `-0.0` in a proto3 field as unset.
        if fd.supports_presence() || fd.is_list() || fd.is_map() {
            return false;
        }
        return match &*msg.get_field(fd) {
            Value::F32(v) => v.to_bits() != 0,
            Value::F64(v) => v.to_bits() != 0,
            _ => false,
        };
    }
    match &*msg.get_field(fd) {
        Value::List(l) => !l.is_empty(),
        Value::Map(m) => !m.is_empty(),
        _ => true,
    }
}

/// The well-known types protojson writes in their own form.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wkt {
    Any,
    Timestamp,
    Duration,
    Wrapper,
    Struct,
    ListValue,
    Value,
    FieldMask,
    Empty,
}

pub(crate) fn wkt(desc: &MessageDescriptor) -> Option<Wkt> {
    let name = desc.full_name().strip_prefix("google.protobuf.")?;
    Some(match name {
        "Any" => Wkt::Any,
        "Timestamp" => Wkt::Timestamp,
        "Duration" => Wkt::Duration,
        "BoolValue" | "Int32Value" | "Int64Value" | "UInt32Value" | "UInt64Value"
        | "FloatValue" | "DoubleValue" | "StringValue" | "BytesValue" => Wkt::Wrapper,
        "Struct" => Wkt::Struct,
        "ListValue" => Wkt::ListValue,
        "Value" => Wkt::Value,
        "FieldMask" => Wkt::FieldMask,
        "Empty" => Wkt::Empty,
        _ => return None,
    })
}

pub(crate) fn is_null_value(kind: &Kind) -> bool {
    matches!(kind, Kind::Enum(e) if e.full_name() == "google.protobuf.NullValue")
}

pub(crate) fn is_known_value(kind: &Kind) -> bool {
    matches!(kind, Kind::Message(m) if m.full_name() == "google.protobuf.Value")
}

/// Access for abada's own conformance tests; not a stable API.
#[doc(hidden)]
pub mod testing {
    use prost_reflect::{DynamicMessage, MessageDescriptor};

    use super::JsonError;

    /// Go's deterministic binary form of a message (`proto.MarshalOptions{
    /// Deterministic: true}` for generated types).
    pub fn deterministic_binary(msg: &DynamicMessage) -> Vec<u8> {
        super::wire::encode(msg)
    }

    /// The binary form read the way Go's generated types read it.
    pub fn decode_binary(
        desc: &MessageDescriptor,
        bytes: &[u8],
    ) -> Result<DynamicMessage, JsonError> {
        super::wire::decode(desc, bytes, 10_000)
    }
}
