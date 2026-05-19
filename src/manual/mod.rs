//! UTCP Manual model, OpenAPI conversion, and deterministic
//! serialization.
//!
//! `Manual::from_openapi` ports UTCP's `OpenApiConverter` mapping rules
//! (see UTCP HTTP protocol spec) to Rust so the policy stays a single
//! self-contained WASM module. `Manual::to_json_bytes` produces a
//! byte-stable JSON document — tools sorted by name, JSON keys in
//! declaration order — so downstream agents that hash the Manual see a
//! stable hash across pod restarts.

pub mod model;
pub mod openapi;
pub mod overrides;
pub mod render;

pub use model::{CallTemplate, Manual, Tool};
