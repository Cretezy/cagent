use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use crate::{
    ModelBackend, ModelCapabilities, ModelCatalog, ModelDescriptor, ProviderError, ReasoningControl,
};

const DEFAULT_BASE_URL: &str = "https://models.opencode.ai";
const FETCH_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Debug)]
pub(crate) struct ModelsDevModel {
    pub(crate) display_name: String,
    pub(crate) capabilities: ModelCapabilities,
    pub(crate) backend: Option<ModelBackend>,
    pub(crate) raw_metadata: Value,
    eligible_plans: Option<Vec<String>>,
    effort_plans: BTreeMap<String, Vec<String>>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModelsDevCatalog {
    providers: BTreeMap<String, ModelsDevProvider>,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ModelsDevProvider {
    pub(crate) display_name: String,
    pub(crate) npm: Option<String>,
    pub(crate) env: Vec<String>,
    models: BTreeMap<String, ModelsDevModel>,
}

impl ModelsDevCatalog {
    #[cfg(test)]
    pub(crate) fn parse(payload: &Value) -> Result<Self, ProviderError> {
        Self::parse_owned(payload.clone())
    }

    pub(crate) fn parse_owned(payload: Value) -> Result<Self, ProviderError> {
        let Value::Object(providers) = payload else {
            return Err(ProviderError::protocol(
                "invalid_models_dev_response",
                "Models.dev response must be an object keyed by provider ID",
            ));
        };

        let mut catalog = Self::default();
        for (provider_id, provider) in providers {
            let Value::Object(mut provider) = provider else {
                continue;
            };
            let display_name = provider
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or(&provider_id)
                .to_owned();
            let npm = provider
                .get("npm")
                .and_then(Value::as_str)
                .map(ToOwned::to_owned);
            let env = provider
                .get("env")
                .and_then(Value::as_array)
                .map(|values| {
                    values
                        .iter()
                        .filter_map(Value::as_str)
                        .map(ToOwned::to_owned)
                        .collect()
                })
                .unwrap_or_default();
            let Some(Value::Object(models)) = provider.remove("models") else {
                continue;
            };
            let provider_backend = backend_for_npm(npm.as_deref());
            let models = models
                .into_iter()
                .filter_map(|(model_id, model)| {
                    let mut model = parse_model(model)?;
                    model.backend = model.backend.or(provider_backend);
                    Some((model_id, model))
                })
                .collect();
            catalog.providers.insert(
                provider_id,
                ModelsDevProvider {
                    display_name,
                    npm,
                    env,
                    models,
                },
            );
        }
        Ok(catalog)
    }

    pub(crate) fn provider(&self, provider_id: &str) -> Option<&ModelsDevProvider> {
        self.providers.get(provider_id)
    }

    pub(crate) fn model(&self, provider_id: &str, model_id: &str) -> Option<&ModelsDevModel> {
        self.providers
            .get(provider_id)
            .and_then(|provider| provider.models.get(model_id))
    }

    pub(crate) fn upstream_model(
        &self,
        provider_id: &str,
        model_id: &str,
    ) -> Option<&ModelsDevModel> {
        if provider_id != "openrouter" {
            return None;
        }
        let (upstream_provider, upstream_model) = openrouter_upstream(model_id)?;
        self.model(upstream_provider, upstream_model)
    }

    pub(crate) fn upstream_provider_name(&self, provider_id: &str, model_id: &str) -> Option<&str> {
        if provider_id != "openrouter" {
            return None;
        }
        let (upstream_provider, upstream_model) = openrouter_upstream(model_id)?;
        let provider = self.providers.get(upstream_provider)?;
        provider
            .models
            .contains_key(upstream_model)
            .then_some(provider.display_name.as_str())
    }

    pub(crate) fn model_catalog_as(
        &self,
        source_provider_id: &str,
        target_provider_id: &str,
        subscription_plan: Option<&str>,
    ) -> Option<ModelCatalog> {
        let provider = self.providers.get(source_provider_id)?;
        Some(ModelCatalog {
            provider: target_provider_id.into(),
            models: provider
                .models
                .iter()
                .filter(|(_, model)| {
                    plan_allows(model.eligible_plans.as_deref(), subscription_plan)
                })
                .map(|(id, model)| ModelDescriptor {
                    id: id.clone(),
                    display_name: model.display_name.clone(),
                    capabilities: capabilities_for_plan(model, subscription_plan),
                    backend: model.backend,
                    raw_metadata: json!({
                        "models_dev": model.raw_metadata,
                    }),
                })
                .collect(),
            version: None,
        })
    }
}

fn openrouter_upstream(model_id: &str) -> Option<(&str, &str)> {
    let (provider, model) = model_id.split_once('/')?;
    Some((if provider == "z-ai" { "zai" } else { provider }, model))
}

fn plan_allows(eligible_plans: Option<&[String]>, subscription_plan: Option<&str>) -> bool {
    eligible_plans.is_none_or(|plans| {
        subscription_plan.is_some_and(|plan| plans.iter().any(|eligible| eligible == plan))
    })
}

pub(crate) fn capabilities_for_plan(
    model: &ModelsDevModel,
    subscription_plan: Option<&str>,
) -> ModelCapabilities {
    let mut capabilities = model.capabilities.clone();
    if let Some(efforts) = &mut capabilities.reasoning_efforts {
        efforts.retain(|effort| {
            model.effort_plans.get(effort).is_none_or(|plans| {
                subscription_plan.is_some_and(|plan| plans.iter().any(|eligible| eligible == plan))
            })
        });
    }
    capabilities
}

fn backend_for_npm(npm: Option<&str>) -> Option<ModelBackend> {
    match npm {
        Some("@ai-sdk/openai") => Some(ModelBackend::OpenAiResponses),
        Some("@ai-sdk/openai-compatible") => Some(ModelBackend::OpenAiCompatible),
        Some("@ai-sdk/anthropic") => Some(ModelBackend::AnthropicMessages),
        Some("@ai-sdk/google") | Some("@ai-sdk/google-vertex") => Some(ModelBackend::Gemini),
        _ => None,
    }
}

fn parse_model(model: Value) -> Option<ModelsDevModel> {
    let Value::Object(model) = model else {
        return None;
    };
    let display_name = model.get("name").and_then(Value::as_str)?.to_owned();
    let context_window = model
        .get("limit")
        .and_then(|limit| limit.get("context"))
        .and_then(Value::as_u64)?;
    let supports_tools = model.get("tool_call").and_then(Value::as_bool)?;
    let supports_structured_output = model.get("structured_output").and_then(Value::as_bool);
    let supports_text_input = text_modality(&model, "input");
    let supports_image_input = modality(&model, "input", "image");
    let supports_text_output = text_modality(&model, "output");
    let supports_fast_mode = Value::Object(model.clone())
        .pointer("/experimental/modes/fast/provider/body/service_tier")
        .and_then(Value::as_str)
        .is_some_and(|tier| tier == "priority");
    let reasoning_options = model.get("reasoning_options").and_then(Value::as_array);
    let (reasoning_control, reasoning_efforts) =
        reasoning_options.map_or((None, None), |options| {
            if let Some(values) = options
                .iter()
                .find(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
                .and_then(|option| option.get("values"))
                .and_then(Value::as_array)
            {
                return (
                    Some(ReasoningControl::Effort),
                    Some(
                        values
                            .iter()
                            .filter_map(Value::as_str)
                            .map(ToOwned::to_owned)
                            .collect(),
                    ),
                );
            }
            if options
                .iter()
                .any(|option| option.get("type").and_then(Value::as_str) == Some("toggle"))
            {
                return (
                    Some(ReasoningControl::Toggle),
                    Some(vec!["off".into(), "on".into()]),
                );
            }
            (None, None)
        });
    let backend = model
        .get("provider")
        .and_then(|provider| provider.get("npm"))
        .and_then(Value::as_str)
        .and_then(|npm| backend_for_npm(Some(npm)));

    Some(ModelsDevModel {
        display_name,
        capabilities: ModelCapabilities {
            context_window: Some(context_window),
            supports_streaming: None,
            supports_tools: Some(supports_tools),
            supports_structured_output,
            supports_text_input,
            supports_image_input,
            supports_text_output,
            supports_fast_mode: Some(supports_fast_mode),
            reasoning_control,
            reasoning_efforts,
        },
        backend,
        raw_metadata: Value::Object(model),
        eligible_plans: None,
        effort_plans: BTreeMap::new(),
    })
}

fn text_modality(model: &serde_json::Map<String, Value>, direction: &str) -> Option<bool> {
    modality(model, direction, "text")
}

fn modality(
    model: &serde_json::Map<String, Value>,
    direction: &str,
    expected: &str,
) -> Option<bool> {
    let modalities = model.get("modalities")?.as_object()?;
    let values = modalities.get(direction)?.as_array()?;
    Some(values.iter().any(|value| value.as_str() == Some(expected)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_openai_metadata_and_backend_family() {
        let catalog = ModelsDevCatalog::parse(&serde_json::json!({
            "openai": {
                "id": "openai",
                "name": "OpenAI",
                "npm": "@ai-sdk/openai",
                "env": ["OPENAI_API_KEY"],
                "models": {
                    "gpt-5.6-luna": {
                        "id": "gpt-5.6-luna",
                        "name": "GPT 5.6 Luna",
                        "tool_call": true,
                        "structured_output": true,
                        "reasoning_options": [{
                            "type": "effort",
                            "values": ["none", "low", "high"]
                        }],
                        "modalities": {
                            "input": ["text", "image"],
                            "output": ["text"]
                        },
                        "limit": {"context": 1_050_000, "output": 128_000}
                    }
                }
            }
        }))
        .unwrap();
        let provider = catalog.provider("openai").unwrap();
        assert_eq!(provider.env, ["OPENAI_API_KEY"]);
        assert_eq!(
            catalog.model("openai", "gpt-5.6-luna").unwrap().backend,
            Some(ModelBackend::OpenAiResponses)
        );
        assert_eq!(
            catalog
                .model("openai", "gpt-5.6-luna")
                .unwrap()
                .capabilities,
            ModelCapabilities {
                context_window: Some(1_050_000),
                supports_streaming: None,
                supports_fast_mode: Some(false),
                supports_tools: Some(true),
                supports_structured_output: Some(true),
                supports_text_input: Some(true),
                supports_image_input: Some(true),
                supports_text_output: Some(true),
                reasoning_control: Some(ReasoningControl::Effort),
                reasoning_efforts: Some(vec!["none".into(), "low".into(), "high".into()]),
            }
        );
    }

    #[test]
    fn parses_priority_fast_mode_capability() {
        let catalog = ModelsDevCatalog::parse(&serde_json::json!({
            "openai": {"models": {
                "fast": {"name": "Fast", "tool_call": true, "limit": {"context": 128000}, "experimental": {"modes": {"fast": {
                    "provider": {"body": {"service_tier": "priority"}}
                }}}},
                "other": {"name": "Other", "tool_call": true, "limit": {"context": 128000}, "experimental": {"modes": {"fast": {
                    "provider": {"body": {"service_tier": "flex"}}
                }}}}
            }}
        }))
        .unwrap();
        assert_eq!(
            catalog
                .model("openai", "fast")
                .unwrap()
                .capabilities
                .supports_fast_mode,
            Some(true)
        );
        assert_eq!(
            catalog
                .model("openai", "other")
                .unwrap()
                .capabilities
                .supports_fast_mode,
            Some(false)
        );
    }

    #[test]
    fn parses_toggle_reasoning_as_on_off_control() {
        let catalog = ModelsDevCatalog::parse(&serde_json::json!({
            "zai": {
                "models": {
                    "glm-5.1": {
                        "name": "GLM-5.1",
                        "tool_call": true,
                        "reasoning_options": [{"type": "toggle"}],
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 204_800}
                    }
                }
            }
        }))
        .unwrap();

        let capabilities = &catalog.model("zai", "glm-5.1").unwrap().capabilities;
        assert_eq!(
            capabilities.reasoning_control,
            Some(ReasoningControl::Toggle)
        );
        assert_eq!(
            capabilities.reasoning_efforts.as_deref(),
            Some(["off".to_owned(), "on".to_owned()].as_slice())
        );
    }

    #[test]
    fn model_level_provider_metadata_selects_the_request_backend() {
        let catalog = ModelsDevCatalog::parse(&serde_json::json!({
            "opencode": {
                "name": "OpenCode Zen",
                "env": ["OPENCODE_API_KEY"],
                "models": {
                    "responses-model": {
                        "name": "Responses model",
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 64_000},
                        "provider": {"npm": "@ai-sdk/openai"}
                    },
                    "compatible-model": {
                        "name": "Compatible model",
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 64_000},
                        "provider": {"npm": "@ai-sdk/openai-compatible"}
                    },
                    "messages-model": {
                        "name": "Messages model",
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 64_000},
                        "provider": {"npm": "@ai-sdk/anthropic"}
                    }
                }
            }
        }))
        .unwrap();

        assert_eq!(
            catalog
                .model("opencode", "responses-model")
                .unwrap()
                .backend,
            Some(ModelBackend::OpenAiResponses)
        );
        assert_eq!(
            catalog
                .model("opencode", "compatible-model")
                .unwrap()
                .backend,
            Some(ModelBackend::OpenAiCompatible)
        );
        assert_eq!(
            catalog.model("opencode", "messages-model").unwrap().backend,
            Some(ModelBackend::AnthropicMessages)
        );
    }

    #[tokio::test]
    async fn loads_the_disk_cache_before_network_access() {
        let temporary = tempfile::TempDir::new().unwrap();
        let path = temporary.path().join("models.json");
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({
            "openai": {
                    "npm": "@ai-sdk/openai-compatible",
                "models": {
                        "cached-model": {
                            "id": "cached-model",
                            "name": "Cached model",
                            "tool_call": false,
                            "limit": {"context": 64_000}
                }
            }
                }
            }))
            .unwrap(),
        )
        .unwrap();

        let client = ModelsDevClient::new(path);
        let catalog = client.catalog().await.unwrap();
        assert_eq!(
            catalog
                .model("openai", "cached-model")
                .unwrap()
                .capabilities
                .context_window,
            Some(64_000)
        );
    }
}

#[derive(Clone, Debug)]
pub(crate) struct ModelsDevClient {
    client: reqwest::Client,
    url: String,
    cache_path: Option<PathBuf>,
    cache: Arc<std::sync::RwLock<Option<ModelsDevCatalog>>>,
    #[cfg(test)]
    test_catalog: Option<ModelsDevCatalog>,
}

impl ModelsDevClient {
    pub(crate) fn new(cache_path: impl Into<PathBuf>) -> Self {
        let base_url = std::env::var("OPENCODE_MODELS_URL")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_BASE_URL.into());
        let cache_path = cache_path.into();
        let client = reqwest::Client::new();
        Self {
            client,
            url: format!("{}/api.json", base_url.trim_end_matches('/')),
            cache_path: Some(cache_path),
            // Parsing a potentially large local cache belongs to startup
            // hydration, not the synchronous runtime-open path.
            cache: Arc::new(std::sync::RwLock::new(None)),
            #[cfg(test)]
            test_catalog: None,
        }
    }

    #[cfg(test)]
    pub(crate) fn for_catalog(catalog: ModelsDevCatalog) -> Self {
        let mut client = Self {
            client: reqwest::Client::new(),
            url: String::new(),
            cache_path: None,
            cache: Arc::new(std::sync::RwLock::new(None)),
            test_catalog: None,
        };
        client.test_catalog = Some(catalog);
        client
    }

    pub(crate) async fn catalog(&self) -> Result<ModelsDevCatalog, ProviderError> {
        #[cfg(test)]
        if let Some(catalog) = &self.test_catalog {
            return Ok(catalog.clone());
        }

        if self.cached_catalog().is_none() {
            self.hydrate_local().await;
        }
        if let Some(catalog) = self.cached_catalog() {
            return Ok(catalog);
        }
        let catalog = parse_catalog(self.fetch().await?)?;
        *self.cache.write().expect("Models.dev cache lock poisoned") = Some(catalog.clone());
        Ok(catalog)
    }

    #[tracing::instrument(level = "trace", name = "provider.models_dev.refresh", skip_all)]
    pub(crate) async fn refresh(&self) -> Result<(), ProviderError> {
        #[cfg(test)]
        if self.test_catalog.is_some() {
            return Ok(());
        }

        let payload = self.fetch().await?;
        tracing::trace!(bytes = payload.len(), "Models.dev payload received");
        if let Some(path) = &self.cache_path {
            write_cache(path, &payload)?;
        }
        let catalog = parse_catalog(payload)?;
        *self.cache.write().expect("Models.dev cache lock poisoned") = Some(catalog);
        Ok(())
    }

    /// Hydrates local metadata without touching the network. A missing or
    /// corrupt cache intentionally becomes a usable bundled seed catalog.
    pub(crate) async fn hydrate_local(&self) {
        #[cfg(test)]
        if self.test_catalog.is_some() {
            return;
        }

        let Some(path) = self.cache_path.clone() else {
            return;
        };
        let catalog = tokio::task::spawn_blocking(move || {
            load_cache(&path).unwrap_or_else(bundled_seed_catalog)
        })
        .await
        .unwrap_or_else(|_| bundled_seed_catalog());
        *self.cache.write().expect("Models.dev cache lock poisoned") = Some(catalog);
    }

    pub(crate) fn cached_catalog(&self) -> Option<ModelsDevCatalog> {
        #[cfg(test)]
        if let Some(catalog) = &self.test_catalog {
            return Some(catalog.clone());
        }

        self.cache
            .read()
            .expect("Models.dev cache lock poisoned")
            .as_ref()
            .cloned()
    }

    #[tracing::instrument(level = "trace", name = "provider.models_dev.fetch", skip_all)]
    async fn fetch(&self) -> Result<Vec<u8>, ProviderError> {
        let response = tokio::time::timeout(FETCH_TIMEOUT, self.client.get(&self.url).send())
            .await
            .map_err(|_| {
                ProviderError::timeout(
                    "models_dev_timeout",
                    "Models.dev catalog fetch timed out after ten seconds",
                )
            })?
            .map_err(|error| {
                ProviderError::connection("models_dev_connection", error.to_string())
            })?;
        if !response.status().is_success() {
            return Err(ProviderError::connection(
                "models_dev_http_error",
                format!("Models.dev returned HTTP {}", response.status()),
            ));
        }
        response
            .bytes()
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|error| {
                ProviderError::protocol("invalid_models_dev_response", error.to_string())
            })
    }
}

fn bundled_seed_catalog() -> ModelsDevCatalog {
    ModelsDevCatalog::parse_owned(json!({})).expect("empty bundled Models.dev seed is valid")
}

#[tracing::instrument(
    level = "trace",
    name = "provider.models_dev.cache.load",
    skip_all,
    fields(path = %path.display(), bytes = tracing::field::Empty, outcome = tracing::field::Empty)
)]
fn load_cache(path: &Path) -> Option<ModelsDevCatalog> {
    let mut contents = match std::fs::read(path) {
        Ok(contents) => contents,
        Err(_) => {
            tracing::Span::current().record("outcome", "missing");
            return None;
        }
    };
    tracing::Span::current().record("bytes", contents.len());
    let payload = match simd_json::serde::from_slice::<Value>(&mut contents) {
        Ok(payload) => payload,
        Err(_) => {
            tracing::Span::current().record("outcome", "invalid_json");
            return None;
        }
    };
    let catalog = ModelsDevCatalog::parse_owned(payload).ok();
    tracing::Span::current().record(
        "outcome",
        if catalog.is_some() {
            "loaded"
        } else {
            "invalid_catalog"
        },
    );
    catalog
}

fn parse_catalog(mut payload: Vec<u8>) -> Result<ModelsDevCatalog, ProviderError> {
    let payload = simd_json::serde::from_slice::<Value>(&mut payload).map_err(|error| {
        ProviderError::protocol("invalid_models_dev_response", error.to_string())
    })?;
    ModelsDevCatalog::parse_owned(payload)
}

fn write_cache(path: &Path, payload: &[u8]) -> Result<(), ProviderError> {
    let parent = path.parent().ok_or_else(|| {
        ProviderError::configuration("Models.dev cache path has no parent directory")
    })?;
    std::fs::create_dir_all(parent).map_err(|error| {
        ProviderError::connection(
            "models_dev_cache",
            format!("could not create cache directory: {error}"),
        )
    })?;

    let filename = path
        .file_name()
        .ok_or_else(|| ProviderError::configuration("Models.dev cache path has no file name"))?;
    let temporary = path.with_file_name(format!(
        "{}.{}.tmp",
        filename.to_string_lossy(),
        std::process::id()
    ));
    std::fs::write(&temporary, payload).map_err(|error| {
        ProviderError::connection(
            "models_dev_cache",
            format!("could not write cache: {error}"),
        )
    })?;
    std::fs::rename(&temporary, path).map_err(|error| {
        ProviderError::connection(
            "models_dev_cache",
            format!("could not replace cache: {error}"),
        )
    })?;
    Ok(())
}
