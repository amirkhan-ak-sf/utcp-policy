use serde::Deserialize;
#[derive(Deserialize, Clone, Debug)]
pub struct Tools0Config {
    #[serde(alias = "bodyField")]
    pub body_field: Option<String>,
    #[serde(alias = "contentType")]
    pub content_type: Option<String>,
    #[serde(alias = "description")]
    pub description: Option<String>,
    #[serde(alias = "inputs")]
    pub inputs: Option<String>,
    #[serde(alias = "method")]
    pub method: Option<String>,
    #[serde(alias = "name")]
    pub name: String,
    #[serde(alias = "path")]
    pub path: String,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Upstreams0Config {
    #[serde(alias = "host", deserialize_with = "pdk::serde::deserialize_service")]
    pub host: pdk::hl::Service,
    #[serde(alias = "tools")]
    pub tools: Option<Vec<Tools0Config>>,
}
#[derive(Deserialize, Clone, Debug)]
pub struct Config {
    #[serde(alias = "apiInstanceProxyPath")]
    pub api_instance_proxy_path: Option<String>,
    #[serde(alias = "cacheControlHeader")]
    pub cache_control_header: Option<String>,
    #[serde(alias = "discoveryPath")]
    pub discovery_path: Option<String>,
    #[serde(alias = "enforcementMode")]
    pub enforcement_mode: Option<String>,
    #[serde(alias = "manualDescription")]
    pub manual_description: Option<String>,
    #[serde(alias = "manualInfoVersion")]
    pub manual_info_version: Option<String>,
    #[serde(alias = "manualTitle")]
    pub manual_title: Option<String>,
    #[serde(alias = "maxBodyBytes")]
    pub max_body_bytes: Option<i64>,
    #[serde(alias = "outboundTimeoutSeconds")]
    pub outbound_timeout_seconds: Option<i64>,
    #[serde(alias = "principalHeader")]
    pub principal_header: Option<String>,
    #[serde(alias = "requirePrincipal")]
    pub require_principal: Option<bool>,
    #[serde(alias = "toolHeaderName")]
    pub tool_header_name: Option<String>,
    #[serde(alias = "toolNamePrefix")]
    pub tool_name_prefix: Option<String>,
    #[serde(alias = "upstreams")]
    pub upstreams: Vec<Upstreams0Config>,
    #[serde(alias = "utcpVersion")]
    pub utcp_version: Option<String>,
    #[serde(alias = "validateInputs")]
    pub validate_inputs: Option<bool>,
}
#[pdk::hl::entrypoint_flex]
fn init(abi: &dyn pdk::flex_abi::api::FlexAbi) -> Result<(), anyhow::Error> {
    let config: Config = serde_json::from_slice(abi.get_configuration())
        .map_err(|err| {
            anyhow::anyhow!(
                "Failed to parse configuration '{}'. Cause: {}",
                String::from_utf8_lossy(abi.get_configuration()), err
            )
        })?;
    for current in config.upstreams {
        abi.service_create(current.host)?;
    }
    abi.setup()?;
    Ok(())
}
