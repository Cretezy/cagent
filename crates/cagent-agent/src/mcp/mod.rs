//! External Model Context Protocol configuration, management, and runtime support.

mod config;
mod oauth;
mod package;
mod runtime;
mod secrets;
mod types;

pub use config::{McpConfigService, parse_mcp_json};
pub(crate) use config::{McpGlobalConfig, global_config_from_document};
pub use oauth::{
    McpOAuthAttempt, McpOAuthCompletion, McpOAuthFlow, McpOAuthPrompt, begin_oauth, clear_oauth,
    oauth_status,
};
pub use package::{
    McpPackage, McpPackageParameter, McpPackageParameterType, McpPackageSecret,
    McpPackageSecretType, McpPackageSetup, builtin_packages, fetch_url_package,
};
pub(crate) use runtime::{McpRegistryReadiness, McpRegistrySnapshot};
pub use runtime::{McpSupervisor, provider_safe_tool_name};
pub use secrets::{McpSecretStatus, McpSecretStore};
pub use types::*;

pub(crate) fn validate_http_url(url: &str, allow_insecure: bool) -> Result<(), String> {
    let parsed = url::Url::parse(url).map_err(|error| format!("invalid MCP URL: {error}"))?;
    if parsed.scheme() == "https" {
        return Ok(());
    }
    if parsed.scheme() != "http" {
        return Err("Streamable HTTP MCP URLs must use http or https".into());
    }
    let loopback = parsed.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<std::net::IpAddr>()
                .is_ok_and(|ip| ip.is_loopback())
    });
    if loopback || allow_insecure {
        Ok(())
    } else {
        Err(
            "plain HTTP is allowed only for localhost/loopback unless allow_insecure is true"
                .into(),
        )
    }
}
