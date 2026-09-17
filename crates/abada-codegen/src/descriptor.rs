//! The slice of `descriptor.proto` and `google/api/http.proto` the generator
//! reads. Declared by hand, with the upstream field numbers, because
//! `prost-types` drops extensions and the `google.api.http` option IS one.

use prost::Message;

#[derive(Clone, PartialEq, Message)]
pub struct FileDescriptorSet {
    #[prost(message, repeated, tag = "1")]
    pub file: Vec<FileDescriptorProto>,
}

#[derive(Clone, PartialEq, Message)]
pub struct FileDescriptorProto {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub package: Option<String>,
    #[prost(message, repeated, tag = "6")]
    pub service: Vec<ServiceDescriptorProto>,
}

#[derive(Clone, PartialEq, Message)]
pub struct ServiceDescriptorProto {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(message, repeated, tag = "2")]
    pub method: Vec<MethodDescriptorProto>,
}

#[derive(Clone, PartialEq, Message)]
pub struct MethodDescriptorProto {
    #[prost(string, optional, tag = "1")]
    pub name: Option<String>,
    #[prost(string, optional, tag = "2")]
    pub input_type: Option<String>,
    #[prost(string, optional, tag = "3")]
    pub output_type: Option<String>,
    #[prost(message, optional, tag = "4")]
    pub options: Option<MethodOptions>,
    #[prost(bool, optional, tag = "5")]
    pub client_streaming: Option<bool>,
    #[prost(bool, optional, tag = "6")]
    pub server_streaming: Option<bool>,
}

#[derive(Clone, PartialEq, Message)]
pub struct MethodOptions {
    /// `google.api.http`, extension 72295728 of `MethodOptions`.
    #[prost(message, optional, tag = "72295728")]
    pub http: Option<HttpRule>,
}

#[derive(Clone, PartialEq, Message)]
pub struct HttpRule {
    #[prost(string, tag = "1")]
    pub selector: String,
    #[prost(oneof = "Pattern", tags = "2, 3, 4, 5, 6, 8")]
    pub pattern: Option<Pattern>,
    #[prost(string, tag = "7")]
    pub body: String,
    #[prost(string, tag = "12")]
    pub response_body: String,
    #[prost(message, repeated, tag = "11")]
    pub additional_bindings: Vec<HttpRule>,
}

#[derive(Clone, PartialEq, prost::Oneof)]
pub enum Pattern {
    #[prost(string, tag = "2")]
    Get(String),
    #[prost(string, tag = "3")]
    Put(String),
    #[prost(string, tag = "4")]
    Post(String),
    #[prost(string, tag = "5")]
    Delete(String),
    #[prost(string, tag = "6")]
    Patch(String),
    #[prost(message, tag = "8")]
    Custom(CustomHttpPattern),
}

#[derive(Clone, PartialEq, Message)]
pub struct CustomHttpPattern {
    #[prost(string, tag = "1")]
    pub kind: String,
    #[prost(string, tag = "2")]
    pub path: String,
}
