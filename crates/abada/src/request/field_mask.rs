//! `runtime.FieldMaskFromRequestBody`: the paths a `PATCH` body sets, when the
//! request message has exactly one `FieldMask` and the client sent none.

use std::collections::VecDeque;

use prost_reflect::{Kind, MessageDescriptor};

use super::json::{FirstValue, Json, Node, first_value, parse};
use super::strconv::{parse_float, quote};

fn is_dynamic(desc: &MessageDescriptor) -> bool {
    matches!(
        desc.full_name(),
        "google.protobuf.Struct" | "google.protobuf.Value"
    )
}

/// `buildPathsBlindly`: every leaf of a JSON object, by key, without looking
/// at a descriptor.
fn paths_blindly(name: &str, node: &Node) -> Vec<String> {
    let Json::Object(_) = &node.json else {
        return vec![name.to_string()];
    };
    let mut paths = Vec::new();
    let mut queue: VecDeque<(String, &Node)> = VecDeque::from([(name.to_string(), node)]);
    while let Some((path, node)) = queue.pop_front() {
        let Json::Object(members) = &node.json else {
            continue;
        };
        for (k, v) in members {
            let child = format!("{path}.{k}");
            if let Json::Object(_) = v.json {
                queue.push_back((child, v));
            } else {
                paths.push(child);
            }
        }
    }
    paths
}

/// `json.Decoder.Decode(&interface{})` refuses a number `float64` cannot hold.
fn check_numbers(node: &Node) -> Result<(), String> {
    match &node.json {
        Json::Number(lit) if parse_float(lit.as_bytes(), 64).is_err() => Err(format!(
            "json: cannot unmarshal number {lit} into Go value of type float64"
        )),
        Json::Array(items) => items.iter().try_for_each(check_numbers),
        Json::Object(members) => members.iter().try_for_each(|(_, v)| check_numbers(v)),
        _ => Ok(()),
    }
}

struct Item<'a> {
    path: String,
    node: Option<&'a Node>,
    msg: Option<MessageDescriptor>,
}

/// The paths, sorted the way Go's `sort.Strings` sorts (by bytes).
pub(crate) fn from_request_body(
    body: &[u8],
    msg: &MessageDescriptor,
) -> Result<Vec<String>, String> {
    let raw = match first_value(body) {
        FirstValue::Eof => return Ok(Vec::new()),
        FirstValue::Error(e) => return Err(e),
        FirstValue::Value(raw) => raw,
    };
    let root = parse(raw);
    check_numbers(&root)?;
    let mut paths: Vec<String> = Vec::new();
    let mut queue: VecDeque<Item<'_>> = VecDeque::from([Item {
        path: String::new(),
        node: Some(&root),
        msg: Some(msg.clone()),
    }]);
    while let Some(item) = queue.pop_front() {
        match item.node.map(|n| &n.json) {
            Some(Json::Object(members)) if !members.is_empty() => {
                for (k, v) in members {
                    let Some(desc) = &item.msg else {
                        return Err("JSON structure did not match request type".into());
                    };
                    let Some(fd) = desc
                        .get_field_by_name(k)
                        .or_else(|| desc.get_field_by_json_name(k))
                    else {
                        return Err(format!(
                            "could not find field {} in {}",
                            quote(k.as_bytes()),
                            quote(desc.full_name().as_bytes())
                        ));
                    };
                    let field_msg = match fd.kind() {
                        Kind::Message(m) => Some(m),
                        _ => None,
                    };
                    if let Some(m) = &field_msg
                        && is_dynamic(m)
                    {
                        for p in paths_blindly(fd.name(), v) {
                            let path = if item.path.is_empty() {
                                p
                            } else {
                                format!("{}.{p}", item.path)
                            };
                            queue.push_back(Item {
                                path,
                                node: None,
                                msg: None,
                            });
                        }
                        continue;
                    }
                    if let Some(m) = &field_msg
                        && m.full_name() == "google.protobuf.Any"
                        && !fd.is_list()
                    {
                        let has_type = matches!(&v.json, Json::Object(members) if members.iter().any(|(k, _)| k == "@type"));
                        if !has_type {
                            return Err(format!(
                                "could not find field @type in {} in message {}",
                                quote(k.as_bytes()),
                                quote(desc.full_name().as_bytes())
                            ));
                        }
                        // grpc-gateway keeps the key as written and drops the
                        // parent path here.
                        queue.push_back(Item {
                            path: k.clone(),
                            node: None,
                            msg: None,
                        });
                        continue;
                    }
                    let path = if item.path.is_empty() {
                        fd.name().to_string()
                    } else {
                        format!("{}.{}", item.path, fd.name())
                    };
                    if fd.is_list() || fd.is_map() {
                        paths.push(path);
                    } else {
                        queue.push_back(Item {
                            path,
                            node: Some(v),
                            msg: field_msg,
                        });
                    }
                }
            }
            // An empty object falls through to the leaf case even at the
            // root, where its path is "".
            Some(Json::Object(_)) => paths.push(item.path),
            _ if !item.path.is_empty() => paths.push(item.path),
            _ => {}
        }
    }
    paths.sort();
    Ok(paths)
}
