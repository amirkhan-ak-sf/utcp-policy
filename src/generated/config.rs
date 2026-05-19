use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "allowInsecureOpenapiUrl")]
    pub allow_insecure_openapi_url: Option<bool>,
    #[serde(alias = "cacheControlHeader")]
    pub cache_control_header: Option<String>,
    #[serde(alias = "discoveryPath")]
    pub discovery_path: Option<String>,
    #[serde(alias = "enforcementMode")]
    pub enforcement_mode: Option<String>,
    #[serde(alias = "manualSource")]
    pub manual_source: String,
    #[serde(alias = "maxBodyBytes")]
    pub max_body_bytes: Option<i64>,
    #[serde(alias = "openapiUrl")]
    pub openapi_url: Option<String>,
    #[serde(alias = "principalHeader")]
    pub principal_header: Option<String>,
    #[serde(alias = "refreshIntervalSeconds")]
    pub refresh_interval_seconds: Option<i64>,
    #[serde(alias = "requirePrincipal")]
    pub require_principal: Option<bool>,
    #[serde(alias = "staticManual")]
    pub static_manual: Option<String>,
    #[serde(alias = "toolHeaderName")]
    pub tool_header_name: Option<String>,
    #[serde(alias = "toolNamePrefix")]
    pub tool_name_prefix: Option<String>,
    #[serde(alias = "utcpVersion")]
    pub utcp_version: Option<String>,
    #[serde(alias = "validateInputs")]
    pub validate_inputs: Option<bool>,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    abi.setup()?;
    Ok(())
}
