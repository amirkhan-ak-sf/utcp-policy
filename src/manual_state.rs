//! Compiled in-memory state for the served Manual.
//!
//! `ManualState::initialize` does the one-shot work: load the source
//! (OpenAPI / static / hybrid), compile the path-template router, and
//! pre-serialize the JSON body that the discovery endpoint will hand
//! out byte-for-byte.
//!
//! At v1 there is **no live OpenAPI fetch**: a connected-mode-safe
//! outbound HTTP fetch from policy startup requires a registered
//! `Service` (see oauth-2-jwt-bearer for the pattern), and we don't
//! want a hidden runtime dependency on the operator declaring one for
//! the OpenAPI URL. Instead `manualSource=openapi` requires the
//! operator to either (a) point at a local-mode HTTP source that is
//! itself reachable as the API-instance upstream, or (b) paste the
//! Manual JSON via `manualSource=static`. Periodic refresh and
//! `Service`-backed OpenAPI fetch are tracked in ROADMAP.

use anyhow::{anyhow, Context};
use serde_json::Value;

use crate::config::{ManualSource, PolicyConfig};
use crate::manual::{
    model::Manual,
    openapi::{self, ConvertOptions},
    overrides, render,
};
use crate::validate::ToolRouter;

pub struct ManualState {
    pub manual: Manual,
    pub manual_bytes: Vec<u8>,
    pub router: ToolRouter,
}

impl ManualState {
    pub fn from_inline_openapi(spec: &Value, cfg: &PolicyConfig) -> anyhow::Result<Self> {
        let manual = openapi::convert(
            spec,
            &ConvertOptions {
                utcp_version: &cfg.utcp_version,
                tool_name_prefix: &cfg.tool_name_prefix,
            },
        )
        .map_err(|e| anyhow!("OpenAPI -> UTCP conversion failed: {e}"))?;
        Self::finalize(manual)
    }

    pub fn from_static(value: Value, cfg: &PolicyConfig) -> anyhow::Result<Self> {
        let mut manual = Manual::from_value(value).context("staticManual is not a valid Manual")?;
        // Honour utcp_version override and a tool-name prefix on
        // explicit-static input too, so multi-instance deployments stay
        // namespaced consistently.
        if !cfg.utcp_version.is_empty() {
            manual.utcp_version = cfg.utcp_version.clone();
        }
        if !cfg.tool_name_prefix.is_empty() {
            for t in &mut manual.tools {
                t.name = format!("{}{}", cfg.tool_name_prefix, t.name);
            }
        }
        Self::finalize(manual)
    }

    pub fn from_hybrid(spec: &Value, patch: &Value, cfg: &PolicyConfig) -> anyhow::Result<Self> {
        let mut manual = openapi::convert(
            spec,
            &ConvertOptions {
                utcp_version: &cfg.utcp_version,
                tool_name_prefix: &cfg.tool_name_prefix,
            },
        )
        .map_err(|e| anyhow!("OpenAPI -> UTCP conversion failed: {e}"))?;
        overrides::apply_overrides(&mut manual, patch);
        Self::finalize(manual)
    }

    fn finalize(manual: Manual) -> anyhow::Result<Self> {
        let router = ToolRouter::build(&manual)
            .map_err(|e| anyhow!("router compilation failed: {e}"))?;
        let manual_bytes = render::to_json_bytes(&manual)
            .map_err(|e| anyhow!("manual rendering failed: {e}"))?;
        Ok(Self {
            manual,
            manual_bytes,
            router,
        })
    }

    /// `manualSource` decides which constructor we call, but in v1 we
    /// can only build state when the source material is already
    /// in-process. Bridges policy load -> ManualState construction.
    pub fn for_source(cfg: &PolicyConfig) -> anyhow::Result<Self> {
        match cfg.manual_source {
            ManualSource::Static => {
                let v = cfg
                    .static_manual_json
                    .as_ref()
                    .ok_or_else(|| anyhow!("staticManual is missing"))?;
                Self::from_static(v.clone(), cfg)
            }
            ManualSource::OpenApi | ManualSource::Hybrid => Err(anyhow!(
                "manualSource={:?} requires a runtime OpenAPI fetch, which is deferred to a future \
                 release. As a workaround, set manualSource=static and paste the Manual JSON into \
                 staticManual, or run an OpenAPI->UTCP build step at deploy time.",
                cfg.manual_source
            )),
        }
    }
}
