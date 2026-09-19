use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use sha2::{Digest as _, Sha256};

use super::{
    AuthFlow, CredentialSource, ModelBackend, ModelDiscoverySource, Provider, ProviderDescriptor,
};
use crate::config::{ConfigSnapshot, ProviderKind, ProviderSettings};
use crate::store::GlobalStore;

#[derive(Clone, Copy, Debug)]
pub struct ProviderDefinition {
    pub id: &'static str,
    pub kind: ProviderKind,
    pub display_name: &'static str,
    pub credential_source: CredentialSource,
    pub credential_environment_variable: Option<&'static str>,
    pub catalog_authority: ModelDiscoverySource,
    pub models_dev_alias: Option<&'static str>,
    pub fallback_backend: Option<ModelBackend>,
    pub supported_backends: &'static [ModelBackend],
    pub auth_flows: &'static [AuthFlow],
}

const RESPONSES: &[ModelBackend] = &[ModelBackend::OpenAiResponses];
const CHAT_COMPLETIONS: &[ModelBackend] = &[ModelBackend::OpenAiCompatible];
const MESSAGES: &[ModelBackend] = &[ModelBackend::AnthropicMessages];
const GEMINI: &[ModelBackend] = &[ModelBackend::Gemini];
const ZEN_BACKENDS: &[ModelBackend] = &[ModelBackend::OpenAiResponses, ModelBackend::Gemini];
const COPILOT_BACKENDS: &[ModelBackend] = &[
    ModelBackend::OpenAiResponses,
    ModelBackend::OpenAiCompatible,
    ModelBackend::AnthropicMessages,
];
const CHATGPT_AUTH: &[AuthFlow] = &[AuthFlow::BrowserPkce, AuthFlow::DeviceCode];
const COPILOT_AUTH: &[AuthFlow] = &[AuthFlow::DeviceCode];

static BUILT_INS: [ProviderDefinition; 9] = [
    ProviderDefinition {
        id: "mock",
        kind: ProviderKind::Mock,
        display_name: "Mock",
        credential_source: CredentialSource::Unauthenticated,
        credential_environment_variable: None,
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: None,
        supported_backends: &[],
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "chatgpt",
        kind: ProviderKind::Chatgpt,
        display_name: "ChatGPT subscription",
        credential_source: CredentialSource::Subscription,
        credential_environment_variable: None,
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: Some("openai"),
        fallback_backend: Some(ModelBackend::OpenAiResponses),
        supported_backends: RESPONSES,
        auth_flows: CHATGPT_AUTH,
    },
    ProviderDefinition {
        id: "github-copilot",
        kind: ProviderKind::GithubCopilot,
        display_name: "GitHub Copilot",
        credential_source: CredentialSource::Subscription,
        credential_environment_variable: None,
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: None,
        supported_backends: COPILOT_BACKENDS,
        auth_flows: COPILOT_AUTH,
    },
    ProviderDefinition {
        id: "openai",
        kind: ProviderKind::Openai,
        display_name: "OpenAI",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("OPENAI_API_KEY"),
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: Some(ModelBackend::OpenAiResponses),
        supported_backends: RESPONSES,
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "opencode",
        kind: ProviderKind::Opencode,
        display_name: "OpenCode Zen",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("OPENCODE_API_KEY"),
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: Some(ModelBackend::OpenAiResponses),
        supported_backends: ZEN_BACKENDS,
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "anthropic",
        kind: ProviderKind::Anthropic,
        display_name: "Anthropic",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("ANTHROPIC_API_KEY"),
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: Some(ModelBackend::AnthropicMessages),
        supported_backends: MESSAGES,
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "openrouter",
        kind: ProviderKind::Openrouter,
        display_name: "OpenRouter",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("OPENROUTER_API_KEY"),
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: None,
        fallback_backend: Some(ModelBackend::OpenAiCompatible),
        supported_backends: CHAT_COMPLETIONS,
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "google",
        kind: ProviderKind::Google,
        display_name: "Google Gemini",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("GOOGLE_API_KEY"),
        catalog_authority: ModelDiscoverySource::ProviderApi,
        models_dev_alias: Some("google"),
        fallback_backend: Some(ModelBackend::Gemini),
        supported_backends: GEMINI,
        auth_flows: &[],
    },
    ProviderDefinition {
        id: "google-vertex",
        kind: ProviderKind::GoogleVertex,
        display_name: "Google Vertex AI",
        credential_source: CredentialSource::Environment,
        credential_environment_variable: Some("GOOGLE_APPLICATION_CREDENTIALS"),
        catalog_authority: ModelDiscoverySource::ModelsDev,
        models_dev_alias: Some("google-vertex"),
        fallback_backend: Some(ModelBackend::Gemini),
        supported_backends: GEMINI,
        auth_flows: &[],
    },
];

#[must_use]
pub fn builtin_provider_definitions() -> &'static [ProviderDefinition] {
    &BUILT_INS
}

#[derive(Clone)]
pub struct ProviderEntry {
    pub settings: ProviderSettings,
    pub descriptor: ProviderDescriptor,
    pub adapter: Arc<dyn Provider>,
    pub connection_fingerprint: String,
    pub discovery_fingerprint: String,
    pub models_dev_alias: Option<&'static str>,
}

impl std::fmt::Debug for ProviderEntry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderEntry")
            .field("settings", &self.settings)
            .field("descriptor", &self.descriptor)
            .field("connection_fingerprint", &self.connection_fingerprint)
            .field("discovery_fingerprint", &self.discovery_fingerprint)
            .finish_non_exhaustive()
    }
}

#[derive(Clone, Debug, Default)]
pub struct ProviderSet {
    entries: BTreeMap<String, ProviderEntry>,
}

impl ProviderSet {
    #[must_use]
    pub fn get(&self, id: &str) -> Option<&ProviderEntry> {
        self.entries.get(id)
    }

    pub fn iter(&self) -> impl Iterator<Item = (&String, &ProviderEntry)> {
        self.entries.iter()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

pub(crate) fn build_provider_set(
    config: &ConfigSnapshot,
    credential_dir: &Path,
    cache_store: Option<&GlobalStore>,
    previous: Option<&ProviderSet>,
    overrides: &BTreeMap<String, Arc<dyn Provider>>,
) -> ProviderSet {
    let mut entries = BTreeMap::new();
    for (id, settings) in config.providers() {
        let connection = connection_fingerprint(settings);
        let discovery = discovery_fingerprint(settings);
        let adapter = overrides.get(id).cloned().or_else(|| {
            previous
                .and_then(|set| set.get(id))
                .filter(|entry| entry.connection_fingerprint == connection)
                .map(|entry| entry.adapter.clone())
        });
        let Some(adapter) =
            adapter.or_else(|| build_adapter(id, settings, credential_dir, cache_store))
        else {
            continue;
        };
        let descriptor = adapter.descriptor().clone();
        let models_dev_alias = BUILT_INS
            .iter()
            .find(|definition| definition.id == id)
            .and_then(|definition| definition.models_dev_alias);
        entries.insert(
            id.clone(),
            ProviderEntry {
                settings: settings.clone(),
                descriptor,
                adapter,
                connection_fingerprint: connection,
                discovery_fingerprint: discovery,
                models_dev_alias,
            },
        );
    }
    ProviderSet { entries }
}

fn build_adapter(
    id: &str,
    settings: &ProviderSettings,
    credential_dir: &Path,
    cache_store: Option<&GlobalStore>,
) -> Option<Arc<dyn Provider>> {
    let display_name = settings.display_name.as_deref().unwrap_or(id);
    match settings.kind {
        ProviderKind::Mock => Some(Arc::new(super::MockProvider)),
        ProviderKind::Chatgpt => Some(Arc::new(crate::ChatGptProvider::new(credential_dir))),
        ProviderKind::GithubCopilot => {
            Some(Arc::new(crate::GitHubCopilotProvider::new(credential_dir)))
        }
        ProviderKind::Openai => {
            let mut provider = crate::OpenAiProvider::new();
            if let Some(base_url) = settings.base_url.as_deref() {
                provider = provider.with_endpoint(base_url);
            }
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            Some(Arc::new(provider.with_credential_dir(credential_dir)))
        }
        ProviderKind::Opencode => {
            let mut provider = crate::OpenCodeProvider::new();
            if let Some(base_url) = settings.base_url.as_deref() {
                provider = provider.with_endpoint(base_url);
            }
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            Some(Arc::new(provider.with_credential_dir(credential_dir)))
        }
        ProviderKind::Anthropic => {
            let mut provider = crate::AnthropicProvider::new();
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            Some(Arc::new(provider.with_credential_dir(credential_dir)))
        }
        ProviderKind::Openrouter => {
            let mut provider = crate::OpenAiCompatibleProvider::openrouter();
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            Some(Arc::new(provider.with_credential_dir(credential_dir)))
        }
        ProviderKind::Google => {
            let mut provider = crate::GeminiProvider::google();
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            provider = provider.with_credential_dir(credential_dir);
            if let Some(store) = cache_store {
                provider = provider.with_cache_store(store.clone());
            }
            Some(Arc::new(provider))
        }
        ProviderKind::GoogleVertex => {
            let mut provider = crate::GeminiProvider::vertex(
                settings.auth,
                settings.project.clone(),
                settings.location.clone(),
            );
            if let Some(variable) = settings.api_key_env.as_deref() {
                provider = provider.with_environment_variable(variable);
            }
            provider = provider.with_credential_dir(credential_dir);
            if let Some(store) = cache_store {
                provider = provider.with_cache_store(store.clone());
            }
            Some(Arc::new(provider))
        }
        ProviderKind::OpenAiCompatible => {
            let base_url = settings.base_url.as_deref()?;
            let key = settings.api_key_env.as_deref()?;
            match settings
                .protocol
                .unwrap_or(crate::ProviderProtocol::OpenAiCompatible)
            {
                crate::ProviderProtocol::Responses => Some(Arc::new(
                    crate::OpenAiProvider::for_custom_responses(
                        id,
                        display_name,
                        base_url,
                        key,
                        settings.models_endpoint.clone(),
                    )
                    .with_credential_dir(credential_dir),
                )),
                crate::ProviderProtocol::OpenAiCompatible => Some(Arc::new(
                    crate::OpenAiCompatibleProvider::custom(
                        id,
                        display_name,
                        base_url,
                        key,
                        settings.models_endpoint.clone(),
                    )
                    .with_credential_dir(credential_dir),
                )),
                crate::ProviderProtocol::Messages => None,
            }
        }
    }
}

fn connection_fingerprint(settings: &ProviderSettings) -> String {
    fingerprint(&format!(
        "{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}\n{:?}",
        settings.kind,
        settings.base_url,
        settings.api_key_env,
        settings.protocol,
        settings.auth,
        settings.project,
        settings.location,
    ))
}

fn discovery_fingerprint(settings: &ProviderSettings) -> String {
    fingerprint(&format!(
        "{}\n{:?}\n{:?}\n{:?}",
        connection_fingerprint(settings),
        settings.models_endpoint,
        settings.models,
        settings.capability_overrides
    ))
}

fn fingerprint(value: &str) -> String {
    format!("{:x}", Sha256::digest(value.as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chatgpt_uses_its_authenticated_codex_catalog() {
        let definition = builtin_provider_definitions()
            .iter()
            .find(|definition| definition.id == "chatgpt")
            .unwrap();

        assert_eq!(
            definition.catalog_authority,
            ModelDiscoverySource::ProviderApi
        );
        assert_eq!(definition.models_dev_alias, Some("openai"));
        assert_eq!(
            crate::ChatGptProvider::new(Path::new("."))
                .descriptor()
                .model_discovery,
            ModelDiscoverySource::ProviderApi
        );
    }

    #[test]
    fn unchanged_connections_reuse_adapters_and_endpoint_changes_replace_them() {
        let first = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[providers.openai]\nbase_url = 'https://one.example/v1'\n",
        )
        .unwrap();
        let overrides = BTreeMap::new();
        let initial = build_provider_set(&first, Path::new("."), None, None, &overrides);
        let unchanged = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[providers.openai]\nbase_url = 'https://one.example/v1'\nmodels = ['extra']\n",
        )
        .unwrap();
        let reused =
            build_provider_set(&unchanged, Path::new("."), None, Some(&initial), &overrides);
        assert!(Arc::ptr_eq(
            &initial.get("openai").unwrap().adapter,
            &reused.get("openai").unwrap().adapter
        ));
        assert_ne!(
            initial.get("openai").unwrap().discovery_fingerprint,
            reused.get("openai").unwrap().discovery_fingerprint
        );

        let changed = ConfigSnapshot::parse(
            Path::new("config.toml"),
            "version = 1\n[providers.openai]\nbase_url = 'https://two.example/v1'\n",
        )
        .unwrap();
        let replaced =
            build_provider_set(&changed, Path::new("."), None, Some(&reused), &overrides);
        assert!(!Arc::ptr_eq(
            &reused.get("openai").unwrap().adapter,
            &replaced.get("openai").unwrap().adapter
        ));
    }
}
