//! One HTTP rule bound to its RPC: what the generator decides once per
//! binding (path parameters and their converters, the body target, the query
//! filter, whether `PATCH` computes a field mask), and the generated
//! handler's request step.

use std::fmt;

use prost_reflect::{
    DynamicMessage, EnumDescriptor, FieldDescriptor, Kind, MethodDescriptor, ReflectMessage,
    Syntax, Value,
};

use super::RequestError;
use super::field_mask;
use super::fields::{
    Ctx, has_runtime_converter, is_message, populate_field_value_from_path, runtime_convert,
    runtime_enum, wkt_message,
};
use super::form::{Form, parse_form};
use crate::json::Marshaler;
use crate::path::{PathParams, PathTemplate};

/// Generator options that change the request step, with
/// `protoc-gen-grpc-gateway`'s defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BindingOptions {
    /// `allow_patch_feature` (default `true`).
    pub allow_patch_feature: bool,
    /// `repeated_path_param_separator` (default `csv`, a comma).
    pub repeated_path_param_separator: u8,
}

impl Default for BindingOptions {
    fn default() -> Self {
        Self {
            allow_patch_feature: true,
            repeated_path_param_separator: b',',
        }
    }
}

/// Where the body goes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BodySelector {
    /// No `body`: the generated handler discards it.
    None,
    /// `body: "*"`.
    Whole,
    /// `body: "<field>"`, a top-level field of the request message.
    Field(FieldDescriptor),
}

/// A rule the generator refuses, or accepts but produces code for that does
/// not compile or panics on every request. abada refuses all of them when the
/// binding is built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BindingError {
    /// `no field %q found in %s`.
    NoField { path: String, message: String },
    /// `not an aggregate type: %s in %s`.
    NotAggregate { field: String, path: String },
    /// A `proto3 optional` field in a path variable.
    OptionalInPath { field: String, path: String },
    /// A message (or map) path variable that is not a well-known type.
    MessageInPath { path: String },
    /// A path variable of a type without a runtime converter
    /// (`FieldMask`, `Struct`, `Value`, a repeated well-known type).
    UnsupportedPathType { path: String },
    /// `body: "a.b"`: the generated Go code dereferences a nil message.
    NestedBodyField { path: String },
    /// A field mask computed into a body field that is not a singular message,
    /// or into a repeated mask: the generated Go code does not compile.
    UnsupportedPatchBody { path: String },
    /// proto2 request messages: not measured, so not accepted.
    Proto2 { message: String },
}

impl fmt::Display for BindingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoField { path, message } => write!(f, "no field {path:?} found in {message}"),
            Self::NotAggregate { field, path } => {
                write!(f, "not an aggregate type: {field} in {path}")
            }
            Self::OptionalInPath { field, path } => {
                write!(
                    f,
                    "optional field not allowed in field path: {field} in {path}"
                )
            }
            Self::MessageInPath { path } => write!(
                f,
                "{path} is a protobuf message type. Protobuf message types cannot be used as path parameters, use a scalar value type (such as string) instead"
            ),
            Self::UnsupportedPathType { path } => {
                write!(f, "unsupported field type of parameter {path}")
            }
            Self::NestedBodyField { path } => write!(
                f,
                "body {path:?}: a nested body field is not supported (grpc-gateway's generated code dereferences a nil message)"
            ),
            Self::UnsupportedPatchBody { path } => write!(
                f,
                "body {path:?}: a field mask can only be computed for a singular message body"
            ),
            Self::Proto2 { message } => write!(f, "{message}: proto2 requests are not supported"),
        }
    }
}

impl std::error::Error for BindingError {}

#[derive(Debug, Clone)]
enum PathConv {
    /// A top-level field: `runtime.<Kind>` or `runtime.<Kind>Slice`.
    Top { repeated: bool },
    /// A nested field: `runtime.PopulateFieldFromPath`, then, for an enum,
    /// `runtime.Enum`/`EnumSlice` again.
    Nested {
        reparse_enum: Option<EnumDescriptor>,
    },
}

#[derive(Debug, Clone)]
struct PathField {
    /// The dotted field path, as the template names it and `PathParams` keys it.
    name: String,
    fields: Vec<FieldDescriptor>,
    conv: PathConv,
}

/// The request step of one generated handler.
#[derive(Debug, Clone)]
pub struct RequestBinding {
    method: MethodDescriptor,
    http_method: String,
    path_fields: Vec<PathField>,
    body: BodySelector,
    response_body: Vec<FieldDescriptor>,
    /// `None` when the generated handler never parses the query.
    query_filter: Option<Vec<Vec<String>>>,
    patch_mask: Option<FieldDescriptor>,
    separator: u8,
}

/// What a generated handler reads from the HTTP request.
#[derive(Debug, Clone, Copy)]
pub struct HttpRequest<'a> {
    /// The method routing used (after `X-HTTP-Method-Override`).
    pub method: &'a str,
    /// The raw query, without `?`.
    pub raw_query: &'a [u8],
    /// The first `Content-Type` value.
    pub content_type: Option<&'a [u8]>,
    pub body: &'a [u8],
    /// Set when `ServeMux` already called `ParseForm`: the body is consumed
    /// and these are the form values.
    pub form: Option<&'a Form>,
}

fn resolve(
    root: &prost_reflect::MessageDescriptor,
    path: &str,
    is_path_param: bool,
) -> Result<Vec<FieldDescriptor>, BindingError> {
    let mut msg = root.clone();
    let mut out: Vec<FieldDescriptor> = Vec::new();
    for (i, c) in path.split('.').enumerate() {
        if i > 0 {
            let prev = &out[i - 1];
            match prev.kind() {
                Kind::Message(m) => msg = m,
                _ => {
                    return Err(BindingError::NotAggregate {
                        field: prev.name().to_string(),
                        path: path.to_string(),
                    });
                }
            }
        }
        let Some(fd) = msg.get_field_by_name(c) else {
            return Err(BindingError::NoField {
                path: path.to_string(),
                message: root.name().to_string(),
            });
        };
        if is_path_param && fd.containing_oneof().is_some_and(|o| o.is_synthetic()) {
            return Err(BindingError::OptionalInPath {
                field: fd.name().to_string(),
                path: path.to_string(),
            });
        }
        out.push(fd);
    }
    Ok(out)
}

impl RequestBinding {
    /// Binds `template`, `body` and `response_body` (as written in the rule)
    /// to `method`.
    pub fn new(
        method: MethodDescriptor,
        http_method: &str,
        template: &PathTemplate,
        body: &str,
        response_body: &str,
        options: &BindingOptions,
    ) -> Result<Self, BindingError> {
        let input = method.input();
        if input.parent_file().syntax() == Syntax::Proto2 {
            return Err(BindingError::Proto2 {
                message: input.full_name().to_string(),
            });
        }
        let mut path_fields = Vec::new();
        for name in template.fields() {
            let fields = resolve(&input, name, true)?;
            let target = fields.last().expect("a field path has a field");
            if let Kind::Message(m) = target.kind() {
                if !has_runtime_converter(m.full_name()) {
                    return Err(BindingError::MessageInPath {
                        path: name.to_string(),
                    });
                }
            }
            let conv = if fields.len() == 1 {
                // `runtime.Timestamp` assigned to a repeated field: the
                // generated code does not compile.
                if target.is_list() && is_message(target) {
                    return Err(BindingError::UnsupportedPathType {
                        path: name.to_string(),
                    });
                }
                PathConv::Top {
                    repeated: target.is_list(),
                }
            } else {
                PathConv::Nested {
                    reparse_enum: match target.kind() {
                        Kind::Enum(e) => Some(e),
                        _ => None,
                    },
                }
            };
            path_fields.push(PathField {
                name: name.to_string(),
                fields,
                conv,
            });
        }

        let body_selector = match body {
            "" => BodySelector::None,
            "*" => BodySelector::Whole,
            path => {
                let fields = resolve(&input, path, false)?;
                if fields.len() > 1 {
                    return Err(BindingError::NestedBodyField {
                        path: path.to_string(),
                    });
                }
                BodySelector::Field(fields.into_iter().next().expect("one field"))
            }
        };

        let response_body = match response_body {
            "" | "*" => Vec::new(),
            path => resolve(&method.output(), path, false)?,
        };

        // `HasQueryParam`: a top-level field that is neither the body field
        // nor a path parameter — compared as dotted strings, as the generator
        // does.
        let query_filter = match &body_selector {
            BodySelector::Whole => None,
            _ => {
                let body_path = match &body_selector {
                    BodySelector::Field(fd) => Some(fd.name().to_string()),
                    _ => None,
                };
                let remaining = input.fields().any(|f| {
                    body_path.as_deref() != Some(f.name())
                        && !path_fields.iter().any(|p| p.name == f.name())
                });
                remaining.then(|| {
                    body_path
                        .iter()
                        .map(|b| b.split('.').map(str::to_string).collect())
                        .chain(
                            path_fields
                                .iter()
                                .map(|p| p.name.split('.').map(str::to_string).collect()),
                        )
                        .collect()
                })
            }
        };

        let patch_mask = match &body_selector {
            BodySelector::Field(body_fd)
                if options.allow_patch_feature && http_method == "PATCH" =>
            {
                let masks: Vec<FieldDescriptor> = input
                    .fields()
                    .filter(|f| {
                        matches!(f.kind(), Kind::Message(ref m) if m.full_name() == "google.protobuf.FieldMask")
                    })
                    .collect();
                match masks.as_slice() {
                    [mask] => {
                        if mask.is_list()
                            || !is_message(body_fd)
                            || body_fd.is_list()
                            || body_fd.is_map()
                        {
                            return Err(BindingError::UnsupportedPatchBody {
                                path: body.to_string(),
                            });
                        }
                        Some(mask.clone())
                    }
                    _ => None,
                }
            }
            _ => None,
        };

        Ok(Self {
            method,
            http_method: http_method.to_string(),
            path_fields,
            body: body_selector,
            response_body,
            query_filter,
            patch_mask,
            separator: options.repeated_path_param_separator,
        })
    }

    pub fn method(&self) -> &MethodDescriptor {
        &self.method
    }

    pub fn http_method(&self) -> &str {
        &self.http_method
    }

    pub fn body(&self) -> &BodySelector {
        &self.body
    }

    /// The response field the writer encodes (`response_body`), outermost
    /// first; empty for the whole response.
    pub fn response_body(&self) -> &[FieldDescriptor] {
        &self.response_body
    }

    /// Whether the generated handler parses the query at all.
    pub fn reads_query(&self) -> bool {
        self.query_filter.is_some()
    }

    /// The field `PATCH` fills from the body's keys when the client sent no
    /// mask.
    pub fn patch_field_mask(&self) -> Option<&FieldDescriptor> {
        self.patch_mask.as_ref()
    }

    /// Builds the request message: the body through `marshaler` (the
    /// `runtime.ServeMux`'s inbound marshaler, [`Marshaler::default`] unless
    /// the gateway registered another registry), then the path variables,
    /// then the query — the generated handler's order, which matters because
    /// decoding a body resets the message.
    pub fn decode(
        &self,
        request: &HttpRequest<'_>,
        params: &PathParams,
        marshaler: &Marshaler,
    ) -> Result<DynamicMessage, RequestError> {
        let mut ctx = Ctx {
            marshaler,
            invalid_utf8: false,
        };
        let mut msg = DynamicMessage::new(self.method.input());
        let body = if request.form.is_some() {
            &[][..]
        } else {
            request.body
        };

        match &self.body {
            BodySelector::None => {}
            BodySelector::Whole => marshaler
                .decode_into(&mut msg, body)
                .map_err(RequestError::codec)?,
            BodySelector::Field(fd) => {
                marshaler
                    .decode_field(&mut msg, fd, body)
                    .map_err(RequestError::codec)?;
                if let Some(mask_fd) = &self.patch_mask {
                    let empty = match msg.get_field(mask_fd).as_ref() {
                        Value::Message(m) if msg.has_field(mask_fd) => m
                            .get_field_by_number(1)
                            .is_none_or(|p| p.as_list().is_none_or(<[Value]>::is_empty)),
                        _ => true,
                    };
                    if empty {
                        let Kind::Message(body_msg) = fd.kind() else {
                            unreachable!("checked when the binding was built");
                        };
                        let paths = field_mask::from_request_body(body, &body_msg)
                            .map_err(RequestError::invalid)?;
                        let Kind::Message(mask_desc) = mask_fd.kind() else {
                            unreachable!("a FieldMask");
                        };
                        let paths = paths.into_iter().map(Value::String).collect();
                        msg.set_field(
                            mask_fd,
                            Value::Message(wkt_message(&mask_desc, &[(1, Value::List(paths))])),
                        );
                    }
                }
            }
        }

        for p in &self.path_fields {
            let Some(val) = params.get(&p.name) else {
                return Err(RequestError::invalid(format!(
                    "missing parameter {}",
                    p.name
                )));
            };
            self.populate_path(&mut ctx, &mut msg, p, val)?;
        }

        if let Some(filter) = &self.query_filter {
            let parsed;
            let form = match request.form {
                Some(form) => form,
                None => {
                    // The body was read by the body step or discarded.
                    let (form, err) =
                        parse_form(request.method, request.content_type, request.raw_query, b"");
                    if let Some(e) = err {
                        return Err(RequestError::invalid(e));
                    }
                    parsed = form;
                    &parsed
                }
            };
            populate_query(&mut ctx, &mut msg, form, filter)?;
        }

        if ctx.invalid_utf8 {
            return Err(RequestError::invalid_utf8());
        }
        Ok(msg)
    }

    fn populate_path(
        &self,
        ctx: &mut Ctx<'_>,
        msg: &mut DynamicMessage,
        p: &PathField,
        val: &[u8],
    ) -> Result<(), RequestError> {
        let mismatch = |e: &dyn fmt::Display| {
            RequestError::invalid(format!("type mismatch, parameter: {}, error: {e}", p.name))
        };
        match &p.conv {
            PathConv::Top { repeated } => {
                let fd = &p.fields[0];
                let kind = fd.kind();
                let value = if *repeated {
                    let mut items = Vec::new();
                    for part in val.split(|&c| c == self.separator) {
                        items.push(runtime_convert(ctx, &kind, part).map_err(|e| mismatch(&e))?);
                    }
                    Value::List(items)
                } else {
                    runtime_convert(ctx, &kind, val).map_err(|e| mismatch(&e))?
                };
                if let Some(oneof) = fd.containing_oneof() {
                    if !oneof.is_synthetic() {
                        if let Some(other) = oneof.fields().find(|f| f != fd && msg.has_field(f)) {
                            return Err(RequestError::invalid(format!(
                                "expect type: *{}_{}, but: {}\n",
                                msg.descriptor().name(),
                                fd.name(),
                                other.name()
                            )));
                        }
                    }
                }
                msg.set_field(fd, value);
            }
            PathConv::Nested { reparse_enum } => {
                let path: Vec<&[u8]> = p.name.split('.').map(str::as_bytes).collect();
                populate_field_value_from_path(ctx, msg, &path, &[val.to_vec()])
                    .map_err(|e| mismatch(&e.message).with_origin(e.origin))?;
                if let Some(e) = reparse_enum {
                    let not_enum = |err: &str| {
                        RequestError::invalid(format!(
                            "could not parse path as enum value, parameter: {}, error: {err}",
                            p.name
                        ))
                    };
                    let target = p.fields.last().expect("a field");
                    let value = if target.is_list() {
                        let mut items = Vec::new();
                        for part in val.split(|&c| c == self.separator) {
                            items.push(Value::EnumNumber(
                                runtime_enum(e, part).map_err(|err| not_enum(&err))?,
                            ));
                        }
                        Value::List(items)
                    } else {
                        Value::EnumNumber(runtime_enum(e, val).map_err(|err| not_enum(&err))?)
                    };
                    let mut m = &mut *msg;
                    for fd in &p.fields[..p.fields.len() - 1] {
                        let Value::Message(child) = m.get_field_mut(fd) else {
                            unreachable!("populated above");
                        };
                        m = child;
                    }
                    m.set_field(target, value);
                }
            }
        }
        Ok(())
    }
}

impl fmt::Display for super::fields::FieldError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

/// The part of `regexp.MustCompile(`^(.*)\[(.*)\]$`)` that matters: `.` does
/// not match a newline, and the first group is greedy.
fn split_map_key(key: &[u8]) -> Option<(&[u8], &[u8])> {
    if key.contains(&b'\n') || key.last() != Some(&b']') {
        return None;
    }
    let open = key.iter().rposition(|&c| c == b'[')?;
    Some((&key[..open], &key[open + 1..key.len() - 1]))
}

/// `runtime.normalizeFieldPath`: proto names for known fields, or the path
/// as given as soon as one component is unknown or not a singular message.
fn normalize_field_path(desc: &prost_reflect::MessageDescriptor, path: &[&[u8]]) -> Vec<Vec<u8>> {
    let original = || path.iter().map(|c| c.to_vec()).collect();
    let mut msg = desc.clone();
    let mut out = Vec::with_capacity(path.len());
    for (i, name) in path.iter().enumerate() {
        let Some(fd) = std::str::from_utf8(name).ok().and_then(|n| {
            msg.get_field_by_name(n)
                .or_else(|| msg.get_field_by_json_name(n))
        }) else {
            return original();
        };
        out.push(fd.name().as_bytes().to_vec());
        if i == path.len() - 1 {
            break;
        }
        match fd.kind() {
            Kind::Message(m) if !fd.is_list() && !fd.is_map() => msg = m,
            _ => return original(),
        }
    }
    out
}

/// `DefaultQueryParser.Parse`.
fn populate_query(
    ctx: &mut Ctx<'_>,
    msg: &mut DynamicMessage,
    form: &Form,
    filter: &[Vec<String>],
) -> Result<(), RequestError> {
    let desc = msg.descriptor();
    for (key, values) in form.iter() {
        let mut values: Vec<Vec<u8>> = values.to_vec();
        let mut key = key;
        if let Some((k, map_key)) = split_map_key(key) {
            key = k;
            values.insert(0, map_key.to_vec());
        }
        let path: Vec<&[u8]> = key.split(|&c| c == b'.').collect();
        let normalized = normalize_field_path(&desc, &path);
        let filtered = filter.iter().any(|seq| {
            seq.len() <= normalized.len()
                && seq
                    .iter()
                    .zip(&normalized)
                    .all(|(a, b)| a.as_bytes() == b.as_slice())
        });
        if filtered {
            continue;
        }
        let normalized: Vec<&[u8]> = normalized.iter().map(Vec::as_slice).collect();
        populate_field_value_from_path(ctx, msg, &normalized, &values)
            .map_err(|e| e.into_request_error())?;
    }
    Ok(())
}
