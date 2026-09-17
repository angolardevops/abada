//! Every HTTP binding a descriptor set declares, in declaration order.

use std::fmt;

use abada::path::{Pattern, PatternError};
use prost::Message;

use crate::descriptor::{self, FileDescriptorSet, HttpRule};

/// One HTTP route to one RPC. A rule with `additional_bindings` gives several.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    /// Fully-qualified service, e.g. `delonix.node.v1.Compute`.
    pub service: String,
    pub method: String,
    pub http_method: String,
    pub template: String,
    /// `""` (no body), `"*"` or a field path.
    pub body: String,
    pub response_body: String,
    pub client_streaming: bool,
    pub server_streaming: bool,
    /// Index among the bindings of the same RPC; 0 is the primary rule.
    pub index: usize,
}

#[derive(Debug)]
pub enum ExtractError {
    Decode(prost::DecodeError),
    /// A rule without a pattern, which grpc-gateway also refuses.
    NoPattern {
        rpc: String,
    },
    /// grpc-gateway forbids nesting past the first level.
    NestedAdditionalBindings {
        rpc: String,
    },
    InvalidTemplate {
        rpc: String,
        error: PatternError,
    },
}

impl fmt::Display for ExtractError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Decode(e) => write!(f, "decoding the descriptor set: {e}"),
            Self::NoPattern { rpc } => write!(f, "{rpc}: google.api.http has no pattern"),
            Self::NestedAdditionalBindings { rpc } => {
                write!(f, "{rpc}: additional_bindings cannot be nested")
            }
            Self::InvalidTemplate { rpc, error } => write!(f, "{rpc}: {error}"),
        }
    }
}

impl std::error::Error for ExtractError {}

/// Reads an encoded `FileDescriptorSet` (as `protoc -o` or `buf build` write it).
pub fn bindings(descriptor_set: &[u8]) -> Result<Vec<Binding>, ExtractError> {
    let set = FileDescriptorSet::decode(descriptor_set).map_err(ExtractError::Decode)?;
    let mut out = Vec::new();
    for file in &set.file {
        for service in &file.service {
            let service_name = match file.package.as_deref() {
                Some(p) if !p.is_empty() => format!("{p}.{}", service.name()),
                _ => service.name().to_string(),
            };
            for method in &service.method {
                let Some(rule) = method.options.as_ref().and_then(|o| o.http.as_ref()) else {
                    continue;
                };
                let rpc = format!("{service_name}.{}", method.name());
                let mut index = 0;
                let mut push = |rule: &HttpRule, out: &mut Vec<Binding>| {
                    let (http_method, template) = match &rule.pattern {
                        Some(descriptor::Pattern::Get(t)) => ("GET", t.clone()),
                        Some(descriptor::Pattern::Put(t)) => ("PUT", t.clone()),
                        Some(descriptor::Pattern::Post(t)) => ("POST", t.clone()),
                        Some(descriptor::Pattern::Delete(t)) => ("DELETE", t.clone()),
                        Some(descriptor::Pattern::Patch(t)) => ("PATCH", t.clone()),
                        Some(descriptor::Pattern::Custom(c)) => (c.kind.as_str(), c.path.clone()),
                        None => return Err(ExtractError::NoPattern { rpc: rpc.clone() }),
                    };
                    if let Err(error) = Pattern::new(&template) {
                        return Err(ExtractError::InvalidTemplate {
                            rpc: rpc.clone(),
                            error,
                        });
                    }
                    out.push(Binding {
                        service: service_name.clone(),
                        method: method.name().to_string(),
                        http_method: http_method.to_string(),
                        template,
                        body: rule.body.clone(),
                        response_body: rule.response_body.clone(),
                        client_streaming: method.client_streaming(),
                        server_streaming: method.server_streaming(),
                        index,
                    });
                    index += 1;
                    Ok(())
                };
                push(rule, &mut out)?;
                for extra in &rule.additional_bindings {
                    if !extra.additional_bindings.is_empty() {
                        return Err(ExtractError::NestedAdditionalBindings { rpc });
                    }
                    push(extra, &mut out)?;
                }
            }
        }
    }
    Ok(out)
}
