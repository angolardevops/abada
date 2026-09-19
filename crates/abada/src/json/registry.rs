//! The types protojson can resolve: what `protoregistry.GlobalTypes` is for
//! a Go binary.

use prost_reflect::{DescriptorError, DescriptorPool, MessageDescriptor};
use prost_types::{
    DescriptorProto, FieldDescriptorProto, FileDescriptorProto,
    field_descriptor_proto::{Label, Type},
};

/// The message types an `Any` can name, built on a `DescriptorPool`.
///
/// A Go gateway resolves `Any` through the types its binary links: the
/// user's generated packages, the well-known types, and whatever else it
/// imports (grpc-go links `google.rpc.Status`). abada's registry is the pool
/// it is given, plus the well-known types and `google.rpc.Status` when the
/// pool lacks them. A type the Go binary happens to link and the pool does
/// not contain (`google.protobuf.FileDescriptorProto`, say) is unresolvable
/// here.
#[derive(Clone, Debug)]
pub struct TypeRegistry {
    pool: DescriptorPool,
    /// The pool as given, before completion: the user's messages come from it.
    given: DescriptorPool,
    /// Whether any message of `pool` has a required field; when none has,
    /// `proto.CheckInitialized` has nothing to find.
    has_required: bool,
}

impl Default for TypeRegistry {
    /// The well-known types and `google.rpc.Status`.
    fn default() -> Self {
        Self::new(DescriptorPool::global()).expect("the well-known types are consistent")
    }
}

const STATUS_FILE: &str = "google/rpc/status.proto";

impl TypeRegistry {
    /// A registry over `pool`, completed with the well-known types and
    /// `google.rpc.Status` when it does not define them.
    pub fn new(pool: DescriptorPool) -> Result<Self, DescriptorError> {
        let given = pool.clone();
        let mut pool = pool;
        let wkt = DescriptorPool::global();
        let missing: Vec<FileDescriptorProto> = wkt
            .files()
            .filter(|f| pool.get_file_by_name(f.name()).is_none())
            .map(|f| f.file_descriptor_proto().clone())
            .collect();
        if !missing.is_empty() {
            pool.add_file_descriptor_protos(missing)?;
        }
        if pool.get_message_by_name("google.rpc.Status").is_none()
            && pool.get_file_by_name(STATUS_FILE).is_none()
        {
            pool.add_file_descriptor_proto(status_file())?;
        }
        let has_required = pool
            .all_messages()
            .any(|m| m.fields().any(|f| f.is_required()));
        Ok(Self {
            pool,
            given,
            has_required,
        })
    }

    /// Whether messages of `pool` can be missing a required field.
    pub(crate) fn may_miss_required(&self, pool: &DescriptorPool) -> bool {
        self.has_required || (*pool != self.pool && *pool != self.given)
    }

    /// A registry over an encoded `FileDescriptorSet`.
    pub fn decode(file_descriptor_set: &[u8]) -> Result<Self, DescriptorError> {
        Self::new(DescriptorPool::decode(file_descriptor_set)?)
    }

    /// The pool, well-known types and `google.rpc.Status` included.
    pub fn pool(&self) -> &DescriptorPool {
        &self.pool
    }

    /// `FindMessageByURL`: the part after the last `/` names the message;
    /// the host is not looked at.
    pub fn find_message_by_url(&self, url: &str) -> Option<MessageDescriptor> {
        let name = url.rsplit_once('/').map_or(url, |(_, name)| name);
        self.pool.get_message_by_name(name)
    }
}

/// `google/rpc/status.proto` from googleapis, as far as protojson sees it.
fn status_file() -> FileDescriptorProto {
    let field = |name: &str, number: i32, label: Label, ty: Type, type_name: Option<&str>| {
        FieldDescriptorProto {
            name: Some(name.into()),
            number: Some(number),
            label: Some(label as i32),
            r#type: Some(ty as i32),
            type_name: type_name.map(Into::into),
            json_name: Some(name.into()),
            ..Default::default()
        }
    };
    FileDescriptorProto {
        name: Some(STATUS_FILE.into()),
        package: Some("google.rpc".into()),
        dependency: vec!["google/protobuf/any.proto".into()],
        message_type: vec![DescriptorProto {
            name: Some("Status".into()),
            field: vec![
                field("code", 1, Label::Optional, Type::Int32, None),
                field("message", 2, Label::Optional, Type::String, None),
                field(
                    "details",
                    3,
                    Label::Repeated,
                    Type::Message,
                    Some(".google.protobuf.Any"),
                ),
            ],
            ..Default::default()
        }],
        syntax: Some("proto3".into()),
        ..Default::default()
    }
}
