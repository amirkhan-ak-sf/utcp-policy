//! Inbound request validation.
//!
//! Two phases:
//!
//!   1. Routing — given `(method, path)`, find the tool whose
//!      `tool_call_template.url` template matches. See `router`.
//!   2. Schema validation — coerce path / query / header / body values
//!      into a synthetic JSON object and check it against the tool's
//!      compiled `inputs` schema. See `schema`.

pub mod router;
pub mod schema;

pub use router::{ResolvedRoute, RouterError, ToolRouter};
pub use schema::{Violation, validate_inputs};
