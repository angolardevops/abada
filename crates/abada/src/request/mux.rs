//! What `runtime.ServeMux.ServeHTTP` does between reading the request line and
//! calling a handler, besides routing: `X-HTTP-Method-Override` and the
//! `POST` → `GET` path-length fallback, both only for a `POST` whose first
//! `Content-Type` is exactly `application/x-www-form-urlencoded`, and both
//! calling `ParseForm`, which consumes the body.
//!
//! Marshaler selection is not here: with only the default marshaler
//! registered, `MarshalerForRequest` answers the same marshaler whatever
//! `Content-Type` and `Accept` say.

use super::RequestError;
use super::form::{Form, parse_form};
use crate::path::{RequestPath, RouteOutcome, Router};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct MuxOptions {
    /// `runtime.WithDisablePathLengthFallback`.
    pub disable_path_length_fallback: bool,
}

/// The parts of a request `ServeMux` looks at.
#[derive(Debug, Clone, Copy)]
pub struct Incoming<'a> {
    pub method: &'a str,
    /// The origin-form request target.
    pub target: &'a str,
    /// The first `Content-Type` value.
    pub content_type: Option<&'a [u8]>,
    /// The first `X-HTTP-Method-Override` value.
    pub method_override: Option<&'a [u8]>,
    pub body: &'a [u8],
}

/// Where the request goes.
#[derive(Debug)]
pub struct Dispatch<'a, T> {
    /// The method routing used: the request's, or the override's.
    pub method: String,
    pub outcome: DispatchOutcome<'a, T>,
    /// Set when `ServeMux` parsed the form. The handler must not read the
    /// body again: it is empty to it.
    pub form: Option<Form>,
}

#[derive(Debug)]
pub enum DispatchOutcome<'a, T> {
    Route(RouteOutcome<'a, T>),
    /// `ParseForm` failed: grpc-gateway answers `InvalidArgument`, after the
    /// error bodies of any malformed escapes met on the way.
    FormError {
        escapes: Vec<Vec<u8>>,
        error: RequestError,
    },
}

/// The raw query of an origin-form target, as `url.ParseRequestURI` splits it.
pub fn raw_query(target: &str) -> &[u8] {
    target
        .split_once('?')
        .map_or(&[][..], |(_, q)| q.as_bytes())
}

/// `strings.ToUpper`: simple case mapping, rune by rune.
fn go_to_upper(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .chars()
        .map(|c| {
            let mut up = c.to_uppercase();
            match (up.next(), up.next()) {
                (Some(u), None) => u,
                _ => c,
            }
        })
        .collect()
}

pub fn dispatch<'a, T>(
    router: &'a Router<T>,
    path: &RequestPath,
    incoming: &Incoming<'_>,
    options: &MuxOptions,
) -> Dispatch<'a, T> {
    let path_length_fallback = |method: &str| {
        !options.disable_path_length_fallback
            && method == "POST"
            && incoming.content_type == Some(b"application/x-www-form-urlencoded".as_slice())
    };
    let query = raw_query(incoming.target);
    let mut method = incoming.method.to_string();
    let mut form: Option<Form> = None;

    if let Some(ov) = incoming.method_override
        && !ov.is_empty()
        && path_length_fallback(&method)
    {
        let (parsed, err) = parse_form(&method, incoming.content_type, query, incoming.body);
        if let Some(e) = err {
            return Dispatch {
                method,
                outcome: DispatchOutcome::FormError {
                    escapes: Vec::new(),
                    error: RequestError::invalid(e),
                },
                form: None,
            };
        }
        form = Some(parsed);
        method = go_to_upper(ov);
    }

    let (outcome, fell_back) =
        router.route_with_fallback(&method, path, path_length_fallback(&method));
    if fell_back && form.is_none() {
        let (parsed, err) = parse_form(&method, incoming.content_type, query, incoming.body);
        if let Some(e) = err {
            let escapes = match outcome {
                RouteOutcome::BadRequest { escapes, .. } => escapes,
                _ => Vec::new(),
            };
            return Dispatch {
                method,
                outcome: DispatchOutcome::FormError {
                    escapes,
                    error: RequestError::invalid(e),
                },
                form: None,
            };
        }
        form = Some(parsed);
    }
    Dispatch {
        method,
        outcome: DispatchOutcome::Route(outcome),
        form,
    }
}
