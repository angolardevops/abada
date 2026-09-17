//! HTTP path templates and routing, behaviour-compatible with grpc-gateway v2.

mod pattern;
mod router;
mod template;

pub use pattern::{MatchError, PathParams, Pattern, PatternError, UnescapingMode};
#[doc(hidden)]
pub use router::path_kept_bytes;
pub use router::{InvalidTarget, RequestPath, RouteOutcome, Router};
#[doc(hidden)]
pub use template::Compiled;
pub use template::{ParseError, PathTemplate, Segment};
