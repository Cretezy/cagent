#![allow(clippy::items_after_test_module)] // Parsing helpers remain adjacent to the bundled-package tests that exercise them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    McpConfiguredValue, McpLocation, McpParameterValue, McpSecretStatus, McpServerDefinition,
    McpTransportConfig,
};
use crate::RuntimeError;

const GITHUB_PACKAGE: &str = include_str!("../../../../plugins/mcp/github/package.toml");
const GITLAB_PACKAGE: &str = include_str!("../../../../plugins/mcp/gitlab/package.toml");

/// One bundled or downloaded MCP package shown by every frontend.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpPackage {
    pub schema_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub description: String,
    #[serde(default)]
    pub parameters: IndexMap<String, McpPackageParameter>,
    #[serde(default)]
    pub secrets: IndexMap<String, McpPackageSecret>,
    pub mcp: McpServerDefinition,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpPackageParameter {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: McpPackageParameterType,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<McpParameterValue>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpPackageSecret {
    pub label: String,
    #[serde(rename = "type")]
    pub kind: McpPackageSecretType,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// Frontend-neutral setup state for one installed package-backed MCP.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct McpPackageSetup {
    pub package: McpPackage,
    pub server_name: String,
    pub location: McpLocation,
    pub parameters: BTreeMap<String, McpParameterValue>,
    pub secret_statuses: BTreeMap<String, McpSecretStatus>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPackageSecretType {
    String,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum McpPackageParameterType {
    String,
    StringList,
    Integer,
    Boolean,
}

impl McpPackage {
    fn validate(&self) -> Result<(), RuntimeError> {
        if self.schema_version != 1 {
            return Err(RuntimeError::InvalidOption(format!(
                "unsupported MCP package schema version: {}",
                self.schema_version
            )));
        }
        if self.id.is_empty() || self.name.is_empty() || self.version.is_empty() {
            return Err(RuntimeError::InvalidOption(
                "MCP package id, name, and version must not be empty".into(),
            ));
        }
        self.mcp.validate().map_err(RuntimeError::InvalidOption)?;
        for (id, parameter) in &self.parameters {
            if !valid_id(id) {
                return Err(RuntimeError::InvalidOption(
                    "MCP package parameter IDs must contain only letters, numbers, underscores, and hyphens"
                        .into(),
                ));
            }
            if let Some(default) = &parameter.default {
                validate_parameter_type(id, parameter, default)?;
            }
        }
        if self.secrets.keys().any(|id| !valid_id(id)) {
            return Err(RuntimeError::InvalidOption(
                "MCP package secret IDs must contain only letters, numbers, underscores, and hyphens"
                    .into(),
            ));
        }
        Ok(())
    }

    pub fn resolve(
        &self,
        source: &str,
        server_name: &str,
        digest: Option<String>,
        values: BTreeMap<String, McpParameterValue>,
    ) -> Result<McpServerDefinition, RuntimeError> {
        self.validate()?;
        if let Some(unknown) = values.keys().find(|id| !self.parameters.contains_key(*id)) {
            return Err(RuntimeError::InvalidOption(format!(
                "unknown MCP package parameter: {unknown}"
            )));
        }
        let mut resolved = BTreeMap::new();
        for (id, parameter) in &self.parameters {
            let value = values
                .get(id)
                .cloned()
                .or_else(|| parameter.default.clone());
            let Some(value) = value else {
                if parameter.required {
                    return Err(RuntimeError::InvalidOption(format!(
                        "missing MCP package parameter: {}",
                        id
                    )));
                }
                continue;
            };
            validate_parameter_type(id, parameter, &value)?;
            resolved.insert(id.clone(), value);
        }
        let mut definition = self.mcp.clone();
        apply_parameters(&mut definition, &resolved)?;
        apply_secrets(&mut definition, server_name, &self.secrets)?;
        definition.package = Some(source.into());
        definition.package_digest = digest;
        definition.parameters = resolved;
        Ok(definition)
    }
}

fn validate_parameter_type(
    id: &str,
    parameter: &McpPackageParameter,
    value: &McpParameterValue,
) -> Result<(), RuntimeError> {
    let valid = matches!(
        (parameter.kind, value),
        (
            McpPackageParameterType::String,
            McpParameterValue::String(_)
        ) | (
            McpPackageParameterType::StringList,
            McpParameterValue::Strings(_)
        ) | (
            McpPackageParameterType::Integer,
            McpParameterValue::Integer(_)
        ) | (
            McpPackageParameterType::Boolean,
            McpParameterValue::Boolean(_)
        )
    );
    if valid {
        Ok(())
    } else {
        Err(RuntimeError::InvalidOption(format!(
            "invalid value type for MCP package parameter {}",
            id
        )))
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
}

fn apply_parameters(
    definition: &mut McpServerDefinition,
    parameters: &BTreeMap<String, McpParameterValue>,
) -> Result<(), RuntimeError> {
    let replace = |source: &str| -> Result<String, RuntimeError> {
        let mut output = source.to_owned();
        while let Some(start) = output.find("${parameter:") {
            let rest = &output[start + 12..];
            let end = rest.find('}').ok_or_else(|| {
                RuntimeError::InvalidOption("unterminated MCP package parameter".into())
            })?;
            let id = &rest[..end];
            let value = parameters.get(id).ok_or_else(|| {
                RuntimeError::InvalidOption(format!("missing MCP package parameter: {id}"))
            })?;
            output.replace_range(start..=(start + 12 + end), &value.to_string());
        }
        Ok(output)
    };
    match &mut definition.transport {
        McpTransportConfig::Stdio {
            command,
            args,
            cwd,
            env,
            ..
        } => {
            *command = replace(command)?;
            for value in args {
                *value = replace(value)?;
            }
            if let Some(value) = cwd {
                *value = replace(value)?;
            }
            for value in env.values_mut() {
                replace_configured(value, &replace)?;
            }
        }
        McpTransportConfig::StreamableHttp { url, headers, .. } => {
            *url = replace(url)?;
            for value in headers.values_mut() {
                replace_configured(value, &replace)?;
            }
        }
    }
    if let Some(image) = &mut definition.image {
        *image = replace(image)?;
    }
    if let Some(oauth) = &mut definition.oauth {
        for value in [
            oauth.bearer_token.as_mut(),
            oauth.client_id.as_mut(),
            oauth.client_secret.as_mut(),
        ]
        .into_iter()
        .flatten()
        {
            replace_configured(value, &replace)?;
        }
    }
    Ok(())
}

fn apply_secrets(
    definition: &mut McpServerDefinition,
    server_name: &str,
    declared: &IndexMap<String, McpPackageSecret>,
) -> Result<(), RuntimeError> {
    let replace = |source: &str| -> Result<String, RuntimeError> {
        let mut output = source.to_owned();
        let mut offset = 0;
        while let Some(relative_start) = output[offset..].find("${secret:") {
            let start = offset + relative_start;
            let rest = &output[start + 9..];
            let end = rest.find('}').ok_or_else(|| {
                RuntimeError::InvalidOption("unterminated MCP package secret reference".into())
            })?;
            let id = &rest[..end];
            if !valid_id(id) || !declared.contains_key(id) {
                return Err(RuntimeError::InvalidOption(format!(
                    "MCP package references undeclared local secret: {id}"
                )));
            }
            let replacement = format!("${{secret:mcp/{server_name}/{id}}}");
            output.replace_range(start..=(start + 9 + end), &replacement);
            offset = start + replacement.len();
        }
        Ok(output)
    };
    match &mut definition.transport {
        McpTransportConfig::Stdio { env, .. } => {
            for value in env.values_mut() {
                replace_configured(value, &replace)?;
            }
        }
        McpTransportConfig::StreamableHttp { headers, .. } => {
            for value in headers.values_mut() {
                replace_configured(value, &replace)?;
            }
        }
    }
    if let Some(oauth) = &mut definition.oauth {
        for value in [oauth.bearer_token.as_mut(), oauth.client_secret.as_mut()]
            .into_iter()
            .flatten()
        {
            replace_configured(value, &replace)?;
        }
    }
    let serialized = toml::to_string(definition)
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
    if serialized.contains("${secret:") && serialized.contains("${secret:mcp/") {
        // Qualified references at this point were produced above. Any local
        // reference left behind was placed in an execution field where package
        // secrets are intentionally forbidden (command, args, URL, or image).
        let without_scoped = declared.keys().fold(serialized, |value, id| {
            value.replace(&format!("${{secret:mcp/{server_name}/{id}}}"), "")
        });
        if without_scoped.contains("${secret:") {
            return Err(RuntimeError::InvalidOption(
                "MCP package secrets may be used only in environment values, HTTP headers, OAuth bearer tokens, or OAuth client secrets"
                    .into(),
            ));
        }
    } else if serialized.contains("${secret:") {
        return Err(RuntimeError::InvalidOption(
            "MCP package contains an undeclared or unscoped secret reference".into(),
        ));
    }
    Ok(())
}

fn replace_configured(
    configured: &mut McpConfiguredValue,
    replace: &impl Fn(&str) -> Result<String, RuntimeError>,
) -> Result<(), RuntimeError> {
    match configured {
        McpConfiguredValue::String(value) | McpConfiguredValue::Described { value, .. } => {
            *value = replace(value)?;
        }
    }
    Ok(())
}

#[must_use]
pub fn builtin_packages() -> Vec<McpPackage> {
    [GITHUB_PACKAGE, GITLAB_PACKAGE]
        .into_iter()
        .map(|source| {
            toml::from_str::<McpPackage>(source)
                .expect("built-in MCP package definitions must be valid")
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_contains_hosted_github_and_oci_gitlab() {
        let packages = builtin_packages();
        assert_eq!(packages.len(), 2);
        let github = packages
            .iter()
            .find(|package| package.id == "github")
            .unwrap();
        assert!(matches!(
            &github.mcp.transport,
            McpTransportConfig::StreamableHttp { url, .. }
                if url == "https://api.githubcopilot.com/mcp/"
        ));
        assert_eq!(
            github
                .mcp
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.client_id.as_ref())
                .map(McpConfiguredValue::value),
            Some("Ov23liqbwZBfNr0IFkh7")
        );
        assert_eq!(github.secrets["pat"].label, "GitHub personal access token");
        assert!(!github.secrets.contains_key("oauth_client_secret"));
        assert!(matches!(
            github.parameters["toolsets"].default,
            Some(McpParameterValue::Strings(_))
        ));
        let gitlab = packages
            .iter()
            .find(|package| package.id == "gitlab")
            .unwrap();
        assert!(
            gitlab.secrets["pat"]
                .description
                .as_deref()
                .is_some_and(|description| description
                    .contains("https://gitlab.com/-/user_settings/personal_access_tokens"))
        );
        assert_eq!(
            gitlab
                .parameters
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["api_url", "permission_mode"]
        );
        let defaults = github
            .parameters
            .iter()
            .filter_map(|(id, parameter)| {
                parameter.default.clone().map(|value| (id.clone(), value))
            })
            .collect();
        let resolved = github
            .resolve("builtin:github", "work-github", None, defaults)
            .unwrap();
        assert_eq!(
            resolved
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.bearer_token.as_ref())
                .map(McpConfiguredValue::value),
            Some("${secret:mcp/work-github/pat}")
        );
        assert_eq!(
            resolved
                .oauth
                .as_ref()
                .and_then(|oauth| oauth.device_authorization_endpoint.as_deref()),
            Some("https://github.com/login/device/code")
        );
        assert!(matches!(
            &resolved.transport,
            McpTransportConfig::StreamableHttp { headers, .. }
                if headers["X-MCP-Toolsets"].value() == "repos,issues,pull_requests"
        ));
        assert!(github.mcp.image.is_none());

        let gitlab = packages
            .iter()
            .find(|package| package.id == "gitlab")
            .unwrap();
        assert_eq!(gitlab.mcp.runner, super::super::McpRunner::Oci);
        assert!(matches!(
            gitlab.mcp.transport,
            McpTransportConfig::Stdio { .. }
        ));
        assert!(
            gitlab
                .mcp
                .image
                .as_deref()
                .is_some_and(|image| image.contains("gitlab"))
        );
    }

    #[test]
    fn package_rejects_cross_mcp_secret_references() {
        let source = r#"
schema_version = 1
id = "unsafe"
name = "Unsafe"
version = "1"
description = "test"

[secrets.token]
label = "Token"
type = "string"

[mcp]
transport = "stdio"
command = "server"

[mcp.env]
TOKEN = "${secret:mcp/another/token}"
"#;
        let package: McpPackage = toml::from_str(source).unwrap();
        assert!(
            package
                .resolve("builtin:unsafe", "unsafe", None, BTreeMap::new())
                .is_err()
        );
    }
}

pub(crate) fn resolve_stored_package(
    config_path: &Path,
    source: &str,
    server_name: &str,
    digest: Option<String>,
    parameters: BTreeMap<String, McpParameterValue>,
) -> Result<McpServerDefinition, RuntimeError> {
    let package = if let Some(id) = source.strip_prefix("builtin:") {
        builtin_packages()
            .into_iter()
            .find(|package| package.id == id)
            .ok_or_else(|| RuntimeError::InvalidOption(format!("unknown built-in MCP: {id}")))?
    } else {
        let digest_value = digest.as_deref().ok_or_else(|| {
            RuntimeError::InvalidOption("URL MCP packages require package_digest".into())
        })?;
        let path = package_cache_path(config_path, source);
        let bytes = std::fs::read(&path).map_err(|error| RuntimeError::Config {
            path: path.clone(),
            message: format!("cached MCP package is unavailable: {error}"),
        })?;
        let actual = format!("sha256:{:x}", Sha256::digest(&bytes));
        if actual != digest_value {
            return Err(RuntimeError::Config {
                path,
                message: "cached MCP package digest does not match configuration".into(),
            });
        }
        toml::from_str(std::str::from_utf8(&bytes).map_err(|error| {
            RuntimeError::InvalidOption(format!("MCP package is not UTF-8: {error}"))
        })?)
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
    };
    package.resolve(source, server_name, digest, parameters)
}

#[must_use]
pub(crate) fn package_cache_path(config_path: &Path, source: &str) -> PathBuf {
    let key = format!("{:x}", Sha256::digest(source.as_bytes()));
    config_path
        .parent()
        .unwrap_or_else(|| Path::new("."))
        .join("mcp-packages")
        .join(format!("{key}.toml"))
}

/// Downloads and digest-locks a custom HTTPS package manifest.
pub async fn fetch_url_package(
    config_path: &Path,
    source: &str,
) -> Result<(McpPackage, String), RuntimeError> {
    let url = url::Url::parse(source).map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid MCP package URL: {error}"))
    })?;
    if url.scheme() != "https" {
        return Err(RuntimeError::InvalidOption(
            "custom MCP package URLs must use HTTPS".into(),
        ));
    }
    let response = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
        .get(url)
        .send()
        .await
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?
        .error_for_status()
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| RuntimeError::InvalidOption(error.to_string()))?;
    if bytes.len() > 1024 * 1024 {
        return Err(RuntimeError::InvalidOption(
            "MCP package manifest exceeds 1 MiB".into(),
        ));
    }
    let package: McpPackage = toml::from_str(std::str::from_utf8(&bytes).map_err(|error| {
        RuntimeError::InvalidOption(format!("MCP package is not UTF-8: {error}"))
    })?)
    .map_err(|error| RuntimeError::InvalidOption(format!("invalid MCP package: {error}")))?;
    package.validate()?;
    let digest = format!("sha256:{:x}", Sha256::digest(&bytes));
    let path = package_cache_path(config_path, source);
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    tokio::fs::write(path, &bytes).await?;
    Ok((package, digest))
}
