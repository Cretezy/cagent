use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::models_dev::{ModelsDevCatalog, ModelsDevClient};
use crate::config::ProviderSettings;
use crate::store::GlobalStore;
use crate::{
    ModelBackend, ModelCapabilities, ModelCatalog, ModelDescriptor, Provider, ProviderError,
    RuntimeError,
};

const CATALOG_TTL: Duration = Duration::from_secs(24 * 60 * 60);
const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelCatalogSource {
    Remote,
    FreshCache,
    StaleCache,
    BundledSeed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResolvedModelCatalog {
    pub catalog: ModelCatalog,
    pub aliases: BTreeMap<String, String>,
    pub source: ModelCatalogSource,
    pub age_millis: Option<u64>,
    pub refresh_error: Option<ProviderError>,
}

impl ResolvedModelCatalog {
    #[must_use]
    pub fn resolve_model_id<'a>(&'a self, model_or_alias: &'a str) -> &'a str {
        self.aliases
            .get(model_or_alias)
            .map_or(model_or_alias, String::as_str)
    }

    #[must_use]
    pub fn model_or_unknown(&self, model_or_alias: &str) -> ModelDescriptor {
        let id = self.resolve_model_id(model_or_alias);
        self.catalog
            .models
            .iter()
            .find(|model| model.id == id)
            .cloned()
            .unwrap_or_else(|| ModelDescriptor {
                id: id.into(),
                display_name: id.into(),
                capabilities: ModelCapabilities::default(),
                backend: None,
                raw_metadata: json!({ "source": "manual_unknown" }),
            })
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PreparedModelCapabilities {
    pub effort: Option<String>,
    pub context_window: u64,
    pub notices: Vec<String>,
}

/// Applies conservative capability degradation before a provider request.
///
/// # Errors
///
/// Returns a hard error when known metadata says streaming or required native tools are absent.
pub fn prepare_model_capabilities(
    model: &ModelDescriptor,
    effort: Option<&str>,
    requires_tools: bool,
    conservative_context_window: u64,
) -> Result<PreparedModelCapabilities, ProviderError> {
    if model.capabilities.supports_streaming == Some(false) {
        return Err(ProviderError::configuration(format!(
            "model {} does not support streaming",
            model.id
        )));
    }
    if requires_tools && model.capabilities.supports_tools == Some(false) {
        return Err(ProviderError::configuration(format!(
            "model {} does not support native tool calling",
            model.id
        )));
    }
    let mut notices = Vec::new();
    let effort = effort.and_then(|effort| {
        let supported = model
            .capabilities
            .reasoning_efforts
            .as_ref()
            .is_none_or(|efforts| efforts.iter().any(|candidate| candidate == effort));
        if supported {
            Some(effort.into())
        } else {
            notices.push(format!(
                "{} does not support reasoning effort {effort}; effort omitted",
                model.id
            ));
            None
        }
    });
    let context_window = model.capabilities.context_window.unwrap_or_else(|| {
        notices.push(format!(
            "{} has no context-window metadata; using conservative default {conservative_context_window}",
            model.id
        ));
        conservative_context_window
    });
    Ok(PreparedModelCapabilities {
        effort,
        context_window,
        notices,
    })
}

trait CatalogClock: Send + Sync {
    fn now_millis(&self) -> u64;
}

#[derive(Debug)]
struct SystemCatalogClock;

impl CatalogClock for SystemCatalogClock {
    fn now_millis(&self) -> u64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis()
            .try_into()
            .unwrap_or(u64::MAX)
    }
}

#[derive(Clone)]
pub(crate) struct CatalogManager {
    store: GlobalStore,
    clock: Arc<dyn CatalogClock>,
    models_dev: Arc<ModelsDevClient>,
}

impl CatalogManager {
    pub(crate) fn new(store: GlobalStore, cache_path: &std::path::Path) -> Self {
        #[cfg(test)]
        let _ = cache_path;
        #[cfg(test)]
        let models_dev = ModelsDevClient::for_catalog(ModelsDevCatalog::default());
        #[cfg(not(test))]
        let models_dev = ModelsDevClient::new(cache_path.to_path_buf());
        Self {
            store,
            clock: Arc::new(SystemCatalogClock),
            models_dev: Arc::new(models_dev),
        }
    }

    #[cfg(test)]
    fn with_clock(store: GlobalStore, clock: Arc<dyn CatalogClock>) -> Self {
        Self {
            store,
            clock,
            models_dev: Arc::new(ModelsDevClient::for_catalog(ModelsDevCatalog::default())),
        }
    }

    #[cfg(test)]
    fn with_models_dev_catalog(
        store: GlobalStore,
        clock: Arc<dyn CatalogClock>,
        catalog: ModelsDevCatalog,
    ) -> Self {
        Self {
            store,
            clock,
            models_dev: Arc::new(ModelsDevClient::for_catalog(catalog)),
        }
    }

    pub(crate) async fn current(
        &self,
        provider: &crate::ProviderDescriptor,
        settings: &ProviderSettings,
        subscription_plan: Option<&str>,
    ) -> Result<ResolvedModelCatalog, RuntimeError> {
        let provider_id = provider.id.as_str();
        let models_dev = self.models_dev.cached_catalog();
        if provider.model_discovery == crate::ModelDiscoverySource::ModelsDev {
            let metadata_provider_id = models_dev_provider_id(provider_id);
            let (catalog, source) = models_dev
                .as_ref()
                .and_then(|models_dev| {
                    models_dev.model_catalog_as(
                        metadata_provider_id,
                        provider_id,
                        subscription_plan,
                    )
                })
                .map_or_else(
                    || (seed_catalog(provider_id), ModelCatalogSource::BundledSeed),
                    |catalog| (catalog, ModelCatalogSource::FreshCache),
                );
            return Ok(resolve(
                catalog,
                settings,
                source,
                None,
                None,
                models_dev.as_ref(),
                provider,
                subscription_plan,
            ));
        }
        let now = self.clock.now_millis();
        if let Some(cached) = self.store.load_model_catalog(provider_id).await? {
            let age = now.saturating_sub(cached.fetched_at_millis);
            // Early Copilot discovery builds could persist an empty success before
            // the provider started rejecting empty API responses. Never let that
            // tombstone suppress a corrected discovery request for a full day.
            let empty_copilot_catalog =
                provider_id == "github-copilot" && cached.catalog.models.is_empty();
            let source = if age < duration_millis(CATALOG_TTL) && !empty_copilot_catalog {
                ModelCatalogSource::FreshCache
            } else {
                ModelCatalogSource::StaleCache
            };
            return Ok(resolve(
                cached.catalog,
                settings,
                source,
                Some(age),
                None,
                models_dev.as_ref(),
                provider,
                subscription_plan,
            ));
        }
        // Provider-owned discovery is the availability authority. Models.dev
        // may enrich IDs returned by that provider, but must never synthesize
        // availability before the provider has answered.
        let catalog = seed_catalog(provider_id);
        Ok(resolve(
            catalog,
            settings,
            ModelCatalogSource::BundledSeed,
            None,
            None,
            self.models_dev.cached_catalog().as_ref(),
            provider,
            subscription_plan,
        ))
    }

    #[tracing::instrument(
        level = "trace",
        name = "provider.catalog.refresh_if_stale",
        skip_all,
        fields(provider = %provider.descriptor().id)
    )]
    pub(crate) async fn refresh_if_stale(
        &self,
        provider: Arc<dyn Provider>,
        settings: &ProviderSettings,
    ) -> Result<ResolvedModelCatalog, RuntimeError> {
        let descriptor = provider.descriptor();
        let subscription_plan = provider.subscription_plan().await;
        let current = self
            .current(descriptor, settings, subscription_plan.as_deref())
            .await?;
        if descriptor.model_discovery == crate::ModelDiscoverySource::ModelsDev {
            return Ok(current);
        }
        if !settings.enabled || matches!(current.source, ModelCatalogSource::FreshCache) {
            return Ok(current);
        }
        self.refresh(provider, settings).await
    }

    #[tracing::instrument(
        level = "trace",
        name = "provider.catalog.refresh",
        skip_all,
        fields(provider = %provider.descriptor().id)
    )]
    pub(crate) async fn refresh(
        &self,
        provider: Arc<dyn Provider>,
        settings: &ProviderSettings,
    ) -> Result<ResolvedModelCatalog, RuntimeError> {
        let descriptor = provider.descriptor();
        let provider_id = descriptor.id.clone();
        let subscription_plan = provider.subscription_plan().await;
        if !settings.enabled {
            return self
                .current(descriptor, settings, subscription_plan.as_deref())
                .await;
        }
        if descriptor.model_discovery == crate::ModelDiscoverySource::ModelsDev {
            match self.models_dev.refresh().await {
                Ok(()) => {
                    return self
                        .current(descriptor, settings, subscription_plan.as_deref())
                        .await;
                }
                Err(error) => {
                    let mut fallback = self
                        .current(descriptor, settings, subscription_plan.as_deref())
                        .await?;
                    fallback.refresh_error = Some(error);
                    return Ok(fallback);
                }
            }
        }
        let Some(discovery) = provider.discover_models() else {
            return self
                .current(descriptor, settings, subscription_plan.as_deref())
                .await;
        };
        let discovery = tokio::time::timeout(DISCOVERY_TIMEOUT, discovery).await;
        let result = match discovery {
            Ok(result) => result,
            Err(_) => Err(ProviderError::timeout(
                "model_discovery_timeout",
                "model discovery timed out after five seconds",
            )),
        };
        match result {
            Ok(mut catalog) => {
                if catalog.models.is_empty() {
                    tracing::warn!(
                        provider = %provider_id,
                        error_code = "empty_model_catalog",
                        "provider model catalog refresh returned no models"
                    );
                    let mut fallback = self
                        .current(descriptor, settings, subscription_plan.as_deref())
                        .await?;
                    fallback.refresh_error = Some(ProviderError::protocol(
                        "empty_model_catalog",
                        format!(
                            "{} returned an empty model catalog",
                            descriptor.display_name
                        ),
                    ));
                    return Ok(fallback);
                }
                self.apply_models_dev_metadata(
                    &mut catalog,
                    &provider_id,
                    subscription_plan.as_deref(),
                )
                .await;
                let fetched_at = self.clock.now_millis();
                self.store
                    .save_model_catalog(catalog.clone(), fetched_at)
                    .await?;
                Ok(resolve(
                    catalog,
                    settings,
                    ModelCatalogSource::Remote,
                    Some(0),
                    None,
                    self.models_dev.cached_catalog().as_ref(),
                    descriptor,
                    subscription_plan.as_deref(),
                ))
            }
            Err(error) => {
                tracing::warn!(
                    provider = %provider_id,
                    error_code = %error.code,
                    error = %error,
                    "provider model catalog refresh failed; using fallback catalog"
                );
                let mut fallback = self
                    .current(descriptor, settings, subscription_plan.as_deref())
                    .await?;
                fallback.refresh_error = Some(error);
                Ok(fallback)
            }
        }
    }

    async fn apply_models_dev_metadata(
        &self,
        catalog: &mut ModelCatalog,
        provider_id: &str,
        subscription_plan: Option<&str>,
    ) {
        let metadata = match self.models_dev.catalog().await {
            Ok(metadata) => metadata,
            Err(error) => {
                tracing::debug!(%error, "Models.dev metadata refresh unavailable");
                return;
            }
        };
        tracing::debug!(provider = provider_id, "using Models.dev model metadata");
        merge_models_dev_metadata(catalog, &metadata, subscription_plan);
    }

    pub(crate) async fn refresh_models_dev(&self) -> Result<(), ProviderError> {
        self.models_dev.refresh().await
    }

    pub(crate) async fn hydrate_models_dev_cache(&self) {
        self.models_dev.hydrate_local().await;
    }
}

#[allow(clippy::too_many_arguments)]
fn resolve(
    mut catalog: ModelCatalog,
    settings: &ProviderSettings,
    source: ModelCatalogSource,
    age_millis: Option<u64>,
    refresh_error: Option<ProviderError>,
    models_dev: Option<&ModelsDevCatalog>,
    provider: &crate::ProviderDescriptor,
    subscription_plan: Option<&str>,
) -> ResolvedModelCatalog {
    let mut known = catalog
        .models
        .iter()
        .map(|model| model.id.clone())
        .collect::<BTreeSet<_>>();
    if provider.model_discovery == crate::ModelDiscoverySource::ModelsDev {
        for model in &settings.models {
            if known.insert(model.clone()) {
                catalog.models.push(ModelDescriptor {
                    id: model.clone(),
                    display_name: model.clone(),
                    capabilities: ModelCapabilities::default(),
                    backend: provider.default_model_backend,
                    raw_metadata: json!({ "source": "manual" }),
                });
            }
        }
    }
    if let Some(models_dev) = models_dev {
        merge_models_dev_metadata(&mut catalog, models_dev, subscription_plan);
    }
    for model in &mut catalog.models {
        if model.backend.is_none() {
            model.backend = provider.default_model_backend;
        }
        if let Some(overrides) = settings.capability_overrides.get(&model.id) {
            merge_capabilities(&mut model.capabilities, overrides);
        }
        // Apply after enrichment/overrides, including catalogs cached by older clients.
        // Ultra requires Codex orchestration that Cagent does not implement.
        if catalog.provider == "chatgpt"
            && let Some(efforts) = &mut model.capabilities.reasoning_efforts
        {
            efforts.retain(|effort| effort != "ultra");
        }
    }
    catalog.models.retain(|model| {
        model.backend.is_none()
            || provider
                .supported_model_backends
                .contains(&model.backend.unwrap_or(ModelBackend::OpenAiResponses))
    });
    catalog.models.sort_by(|left, right| left.id.cmp(&right.id));
    ResolvedModelCatalog {
        catalog,
        aliases: settings.aliases.clone(),
        source,
        age_millis,
        refresh_error,
    }
}

fn merge_models_dev_metadata(
    catalog: &mut ModelCatalog,
    models_dev: &ModelsDevCatalog,
    subscription_plan: Option<&str>,
) {
    let metadata_provider_id = models_dev_provider_id(&catalog.provider);
    let Some(provider) = models_dev.provider(metadata_provider_id) else {
        return;
    };
    tracing::debug!(
        provider = %catalog.provider,
        npm = ?provider.npm,
        credential_environment = ?provider.env,
        "using Models.dev provider metadata"
    );
    for model in &mut catalog.models {
        let Some(metadata) = models_dev.model(metadata_provider_id, &model.id) else {
            continue;
        };
        if model.display_name == model.id {
            model.display_name = metadata.display_name.clone();
        }
        if model.backend.is_none()
            || (catalog.provider == "opencode" && metadata.backend == Some(ModelBackend::Gemini))
        {
            model.backend = metadata.backend;
        }
        fill_missing_capabilities(
            &mut model.capabilities,
            &super::models_dev::capabilities_for_plan(metadata, subscription_plan),
        );
        let upstream_provider_name = models_dev
            .upstream_provider_name(&catalog.provider, &model.id)
            .map(str::to_owned);
        let provider_advertised_reasoning_efforts = model
            .raw_metadata
            .get("provider")
            .filter(|_| model.raw_metadata.get("models_dev").is_some())
            .unwrap_or(&model.raw_metadata)
            .pointer("/reasoning/supported_efforts")
            .and_then(Value::as_array)
            .is_some_and(|efforts| !efforts.is_empty());
        if let Some(upstream) = models_dev.upstream_model(&catalog.provider, &model.id) {
            let upstream = super::models_dev::capabilities_for_plan(upstream, subscription_plan);
            if upstream.reasoning_control.is_some() && !provider_advertised_reasoning_efforts {
                model.capabilities.reasoning_control = upstream.reasoning_control;
                model.capabilities.reasoning_efforts = upstream.reasoning_efforts;
            }
        }
        let provider_metadata = model
            .raw_metadata
            .get("provider")
            .filter(|_| model.raw_metadata.get("models_dev").is_some())
            .unwrap_or(&model.raw_metadata);
        model.raw_metadata = json!({
            "provider": provider_metadata,
            "models_dev": metadata.raw_metadata,
            "upstream_provider_name": upstream_provider_name,
        });
    }
}

fn models_dev_provider_id(provider_id: &str) -> &str {
    crate::builtin_provider_definitions()
        .iter()
        .find(|definition| definition.id == provider_id)
        .and_then(|definition| definition.models_dev_alias)
        .unwrap_or(provider_id)
}

fn merge_capabilities(capabilities: &mut ModelCapabilities, overrides: &ModelCapabilities) {
    if overrides.context_window.is_some() {
        capabilities.context_window = overrides.context_window;
    }
    if overrides.supports_streaming.is_some() {
        capabilities.supports_streaming = overrides.supports_streaming;
    }
    if overrides.supports_fast_mode.is_some() {
        capabilities.supports_fast_mode = overrides.supports_fast_mode;
    }
    if overrides.supports_tools.is_some() {
        capabilities.supports_tools = overrides.supports_tools;
    }
    if overrides.supports_structured_output.is_some() {
        capabilities.supports_structured_output = overrides.supports_structured_output;
    }
    if overrides.supports_text_input.is_some() {
        capabilities.supports_text_input = overrides.supports_text_input;
    }
    if overrides.supports_image_input.is_some() {
        capabilities.supports_image_input = overrides.supports_image_input;
    }
    if overrides.supports_text_output.is_some() {
        capabilities.supports_text_output = overrides.supports_text_output;
    }
    if overrides.reasoning_control.is_some() {
        capabilities.reasoning_control = overrides.reasoning_control;
    }
    if overrides.reasoning_efforts.is_some() {
        capabilities
            .reasoning_efforts
            .clone_from(&overrides.reasoning_efforts);
    }
}

fn fill_missing_capabilities(capabilities: &mut ModelCapabilities, metadata: &ModelCapabilities) {
    if capabilities.context_window.is_none() {
        capabilities.context_window = metadata.context_window;
    }
    if capabilities.supports_streaming.is_none() {
        capabilities.supports_streaming = metadata.supports_streaming;
    }
    if capabilities.supports_fast_mode.is_none() {
        capabilities.supports_fast_mode = metadata.supports_fast_mode;
    }
    if capabilities.supports_tools.is_none() {
        capabilities.supports_tools = metadata.supports_tools;
    }
    if capabilities.supports_structured_output.is_none() {
        capabilities.supports_structured_output = metadata.supports_structured_output;
    }
    if capabilities.supports_text_input.is_none() {
        capabilities.supports_text_input = metadata.supports_text_input;
    }
    if capabilities.supports_image_input.is_none() {
        capabilities.supports_image_input = metadata.supports_image_input;
    }
    if capabilities.supports_text_output.is_none() {
        capabilities.supports_text_output = metadata.supports_text_output;
    }
    if capabilities.reasoning_control.is_none() {
        capabilities.reasoning_control = metadata.reasoning_control;
    }
    if capabilities.reasoning_efforts.is_none() {
        capabilities
            .reasoning_efforts
            .clone_from(&metadata.reasoning_efforts);
    }
}

fn seed_catalog(provider: &str) -> ModelCatalog {
    ModelCatalog {
        provider: provider.into(),
        models: Vec::new(),
        version: None,
    }
}

fn duration_millis(duration: Duration) -> u64 {
    duration.as_millis().try_into().unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use serde_json::Value;

    use crate::{
        AuthState, CredentialSource, ModelRequest, ProviderDescriptor, ProviderFuture,
        ProviderStream,
    };

    use super::*;

    #[derive(Debug)]
    struct FixedClock(u64);

    impl CatalogClock for FixedClock {
        fn now_millis(&self) -> u64 {
            self.0
        }
    }

    #[derive(Debug)]
    struct CatalogProvider {
        result: Mutex<Option<Result<ModelCatalog, ProviderError>>>,
        descriptor: ProviderDescriptor,
    }

    impl CatalogProvider {
        fn returning(result: Result<ModelCatalog, ProviderError>) -> Self {
            Self {
                result: Mutex::new(Some(result)),
                descriptor: provider_descriptor(crate::ModelDiscoverySource::ProviderApi),
            }
        }

        fn from_source(
            source: crate::ModelDiscoverySource,
            result: Result<ModelCatalog, ProviderError>,
        ) -> Self {
            Self {
                result: Mutex::new(Some(result)),
                descriptor: provider_descriptor(source),
            }
        }
    }

    impl Provider for CatalogProvider {
        fn descriptor(&self) -> &ProviderDescriptor {
            &self.descriptor
        }

        fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
            Box::pin(async { Ok(AuthState::Missing) })
        }

        fn discover_models(
            &self,
        ) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
            let result = self.result.lock().unwrap().take().unwrap();
            Some(Box::pin(async move { result }))
        }

        fn stream(
            &self,
            _request: ModelRequest,
            _cancellation: tokio_util::sync::CancellationToken,
        ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
            Box::pin(async { Err(ProviderError::configuration("not used")) })
        }
    }

    fn provider_descriptor(source: crate::ModelDiscoverySource) -> ProviderDescriptor {
        ProviderDescriptor {
            id: "openai".into(),
            display_name: "OpenAI".into(),
            default_model_backend: Some(ModelBackend::OpenAiResponses),
            supported_model_backends: vec![ModelBackend::OpenAiResponses],
            model_discovery: source,
            credential_source: CredentialSource::Environment,
            credential_environment_variable: Some("OPENAI_API_KEY".into()),
            supports_managed_api_key: false,
            auth_flows: Vec::new(),
        }
    }

    fn opencode_descriptor() -> ProviderDescriptor {
        ProviderDescriptor {
            id: "opencode".into(),
            display_name: "OpenCode Zen".into(),
            default_model_backend: Some(ModelBackend::OpenAiResponses),
            supported_model_backends: vec![ModelBackend::OpenAiResponses],
            model_discovery: crate::ModelDiscoverySource::ProviderApi,
            credential_source: CredentialSource::Environment,
            credential_environment_variable: Some("OPENCODE_API_KEY".into()),
            supports_managed_api_key: false,
            auth_flows: Vec::new(),
        }
    }

    fn settings(enabled: bool) -> ProviderSettings {
        ProviderSettings {
            kind: crate::ProviderKind::Openai,
            enabled,
            api_key_env: Some("OPENAI_API_KEY".into()),
            display_name: None,
            base_url: None,
            protocol: None,
            models_endpoint: None,
            usage_limit: None,
            models: vec!["manual-model".into()],
            aliases: BTreeMap::from([("daily".into(), "remote-model".into())]),
            capability_overrides: BTreeMap::from([(
                "remote-model".into(),
                ModelCapabilities {
                    context_window: Some(42),
                    supports_tools: Some(true),
                    ..ModelCapabilities::default()
                },
            )]),
            auth: crate::config::VertexAuth::Adc,
            project: None,
            location: None,
        }
    }

    #[tokio::test]
    async fn successful_refresh_is_cached_and_enriched_without_synthetic_models() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("catalog"))
            .await
            .unwrap();
        let manager = CatalogManager::with_clock(store.clone(), Arc::new(FixedClock(1_000)));
        let provider = Arc::new(CatalogProvider::returning(Ok(ModelCatalog {
            provider: "openai".into(),
            models: vec![ModelDescriptor {
                id: "remote-model".into(),
                display_name: "Remote".into(),
                capabilities: ModelCapabilities::default(),
                backend: None,
                raw_metadata: json!({"remote": true}),
            }],
            version: Some("etag-1".into()),
        })));
        let refreshed = manager.refresh(provider, &settings(true)).await.unwrap();
        assert_eq!(refreshed.source, ModelCatalogSource::Remote);
        assert_eq!(refreshed.catalog.models.len(), 1);
        assert_eq!(refreshed.aliases["daily"], "remote-model");
        assert_eq!(
            refreshed
                .catalog
                .models
                .iter()
                .find(|model| model.id == "remote-model")
                .unwrap()
                .capabilities
                .context_window,
            Some(42)
        );
        assert!(
            refreshed
                .catalog
                .models
                .iter()
                .all(|model| model.id != "manual-model")
        );

        let reopened = GlobalStore::open(&temporary.path().join("catalog"))
            .await
            .unwrap();
        let current = CatalogManager::with_clock(reopened, Arc::new(FixedClock(2_000)))
            .current(
                &provider_descriptor(crate::ModelDiscoverySource::ProviderApi),
                &settings(true),
                None,
            )
            .await
            .unwrap();
        assert_eq!(current.source, ModelCatalogSource::FreshCache);
        assert_eq!(current.age_millis, Some(1_000));
    }

    #[tokio::test]
    async fn invalidating_a_changed_discovery_connection_removes_its_cached_success() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("invalidate"))
            .await
            .unwrap();
        store
            .save_model_catalog(
                ModelCatalog {
                    provider: "openai".into(),
                    models: vec![ModelDescriptor {
                        id: "stale".into(),
                        display_name: "Stale".into(),
                        capabilities: ModelCapabilities::default(),
                        backend: None,
                        raw_metadata: Value::Null,
                    }],
                    version: Some("old-endpoint".into()),
                },
                1,
            )
            .await
            .unwrap();
        assert!(store.load_model_catalog("openai").await.unwrap().is_some());

        store.invalidate_model_catalog("openai").await.unwrap();

        assert!(store.load_model_catalog("openai").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn stale_cache_survives_discovery_failure_and_disabled_provider_never_refreshes() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("fallback"))
            .await
            .unwrap();
        store
            .save_model_catalog(
                ModelCatalog {
                    provider: "openai".into(),
                    models: Vec::new(),
                    version: Some("empty-is-authoritative".into()),
                },
                1,
            )
            .await
            .unwrap();
        let now = duration_millis(CATALOG_TTL) + 2;
        let manager = CatalogManager::with_clock(store, Arc::new(FixedClock(now)));
        let disabled_provider = Arc::new(CatalogProvider::returning(Err(
            ProviderError::connection("must_not_run", "disabled provider was queried"),
        )));
        let disabled = manager
            .refresh_if_stale(disabled_provider, &settings(false))
            .await
            .unwrap();
        assert_eq!(disabled.source, ModelCatalogSource::StaleCache);
        assert!(disabled.refresh_error.is_none());

        let failing_provider = Arc::new(CatalogProvider::returning(Err(
            ProviderError::connection("offline", "network unavailable"),
        )));
        let fallback = manager
            .refresh_if_stale(failing_provider, &settings(true))
            .await
            .unwrap();
        assert_eq!(fallback.source, ModelCatalogSource::StaleCache);
        assert_eq!(fallback.refresh_error.unwrap().code, "offline");
    }

    #[tokio::test]
    async fn empty_copilot_cache_is_never_fresh() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("copilot"))
            .await
            .unwrap();
        store
            .save_model_catalog(
                ModelCatalog {
                    provider: "github-copilot".into(),
                    models: Vec::new(),
                    version: None,
                },
                1_000,
            )
            .await
            .unwrap();
        let manager = CatalogManager::with_clock(store, Arc::new(FixedClock(2_000)));
        let mut descriptor = provider_descriptor(crate::ModelDiscoverySource::ProviderApi);
        descriptor.id = "github-copilot".into();

        let current = manager
            .current(&descriptor, &settings(true), None)
            .await
            .unwrap();

        assert_eq!(current.source, ModelCatalogSource::StaleCache);
    }

    #[tokio::test]
    async fn models_dev_provider_never_calls_a_provider_owned_models_endpoint() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("models-dev"))
            .await
            .unwrap();
        let manager = CatalogManager::with_clock(store, Arc::new(FixedClock(1)));
        let provider = Arc::new(CatalogProvider::from_source(
            crate::ModelDiscoverySource::ModelsDev,
            Err(ProviderError::connection(
                "must_not_run",
                "Models.dev providers do not use provider-owned discovery",
            )),
        ));

        manager
            .refresh_if_stale(provider.clone(), &settings(true))
            .await
            .unwrap();

        assert!(provider.result.lock().unwrap().is_some());
    }

    #[tokio::test]
    async fn cached_openai_models_are_rehydrated_from_models_dev() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("known"))
            .await
            .unwrap();
        store
            .save_model_catalog(
                ModelCatalog {
                    provider: "openai".into(),
                    models: vec![ModelDescriptor {
                        id: "gpt-5.6-luna".into(),
                        display_name: "GPT-5.6 Luna".into(),
                        capabilities: ModelCapabilities::default(),
                        backend: None,
                        raw_metadata: json!({
                            "id": "gpt-5.6-luna",
                            "object": "model",
                            "owned_by": "openai"
                        }),
                    }],
                    version: None,
                },
                1,
            )
            .await
            .unwrap();

        let models_dev = ModelsDevCatalog::parse(&json!({
            "openai": {
                "npm": "@ai-sdk/openai",
                "env": ["OPENAI_API_KEY"],
                "models": {
                    "gpt-5.6-luna": {
                        "id": "gpt-5.6-luna",
                        "name": "GPT 5.6 Luna",
                        "tool_call": true,
                        "reasoning_options": [{
                            "type": "effort",
                            "values": ["none", "low", "medium", "high", "xhigh", "max"]
                        }],
                        "limit": {"context": 1_050_000}
                    }
                }
            }
        }))
        .unwrap();
        let manager =
            CatalogManager::with_models_dev_catalog(store, Arc::new(FixedClock(2)), models_dev);
        let catalog = manager
            .current(
                &provider_descriptor(crate::ModelDiscoverySource::ModelsDev),
                &settings(false),
                None,
            )
            .await
            .unwrap();
        let model = catalog
            .catalog
            .models
            .iter()
            .find(|model| model.id == "gpt-5.6-luna")
            .unwrap();
        assert_eq!(model.capabilities.context_window, Some(1_050_000));
        assert_eq!(model.capabilities.supports_tools, Some(true));
    }

    #[test]
    fn openrouter_uses_upstream_models_dev_reasoning_control() {
        let models_dev = ModelsDevCatalog::parse(&json!({
            "openrouter": {
                "name": "OpenRouter",
                "models": {
                    "z-ai/glm-5.1": {
                        "name": "GLM-5.1",
                        "tool_call": true,
                        "reasoning": true,
                        "reasoning_options": [],
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 204_800}
                    }
                }
            },
            "zai": {
                "name": "Z.AI",
                "models": {
                    "glm-5.1": {
                        "name": "GLM-5.1",
                        "tool_call": true,
                        "reasoning": true,
                        "reasoning_options": [{"type": "toggle"}],
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 204_800}
                    }
                }
            }
        }))
        .unwrap();
        let mut catalog = ModelCatalog {
            provider: "openrouter".into(),
            models: vec![ModelDescriptor {
                id: "z-ai/glm-5.1".into(),
                display_name: "Z.AI: GLM-5.1".into(),
                capabilities: ModelCapabilities {
                    reasoning_control: Some(crate::ReasoningControl::Effort),
                    reasoning_efforts: Some(vec!["low".into(), "medium".into(), "high".into()]),
                    ..ModelCapabilities::default()
                },
                backend: Some(ModelBackend::OpenAiCompatible),
                raw_metadata: json!({"id": "z-ai/glm-5.1"}),
            }],
            version: None,
        };

        merge_models_dev_metadata(&mut catalog, &models_dev, None);

        let capabilities = &catalog.models[0].capabilities;
        assert_eq!(
            capabilities.reasoning_control,
            Some(crate::ReasoningControl::Toggle)
        );
        assert_eq!(
            capabilities.reasoning_efforts.as_deref(),
            Some(["off".to_owned(), "on".to_owned()].as_slice())
        );
        assert_eq!(
            catalog.models[0].raw_metadata["upstream_provider_name"],
            "Z.AI"
        );
    }

    #[test]
    fn openrouter_keeps_provider_advertised_reasoning_efforts() {
        let models_dev = ModelsDevCatalog::parse(&json!({
            "openrouter": {
                "name": "OpenRouter",
                "models": {
                    "openai/gpt-6-astra": {
                        "name": "GPT-6 Astra",
                        "tool_call": true,
                        "reasoning": true,
                        "reasoning_options": [],
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 1_050_000}
                    }
                }
            },
            "openai": {
                "name": "OpenAI",
                "models": {
                    "gpt-6-astra": {
                        "name": "GPT-6 Astra",
                        "tool_call": true,
                        "reasoning": true,
                        "reasoning_options": [{"type": "effort", "values": ["low", "high"]}],
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 1_050_000}
                    }
                }
            }
        }))
        .unwrap();
        let provider_efforts = ["max", "xhigh", "high", "medium", "low"]
            .map(str::to_owned)
            .to_vec();
        let mut catalog = ModelCatalog {
            provider: "openrouter".into(),
            models: vec![ModelDescriptor {
                id: "openai/gpt-6-astra".into(),
                display_name: "OpenAI: GPT-6 Astra".into(),
                capabilities: ModelCapabilities {
                    reasoning_control: Some(crate::ReasoningControl::Effort),
                    reasoning_efforts: Some(provider_efforts.clone()),
                    ..ModelCapabilities::default()
                },
                backend: Some(ModelBackend::OpenAiCompatible),
                raw_metadata: json!({
                    "id": "openai/gpt-6-astra",
                    "reasoning": {"supported_efforts": provider_efforts}
                }),
            }],
            version: None,
        };

        merge_models_dev_metadata(&mut catalog, &models_dev, None);

        assert_eq!(
            catalog.models[0].capabilities.reasoning_efforts.as_deref(),
            Some(
                ["max", "xhigh", "high", "medium", "low"]
                    .map(str::to_owned)
                    .as_slice()
            )
        );
    }

    #[tokio::test]
    async fn provider_api_catalog_starts_empty_before_authenticated_discovery() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("chatgpt-models"))
            .await
            .unwrap();
        let models_dev = ModelsDevCatalog::parse(&json!({
            "openai": {
                "npm": "@ai-sdk/openai",
                "models": {
                    "gpt-5.6-luna": {
                        "name": "GPT 5.6 Luna",
                        "tool_call": true,
                        "modalities": {"input": ["text"], "output": ["text"]},
                        "limit": {"context": 128_000}
                    }
                }
            }
        }))
        .unwrap();
        let manager =
            CatalogManager::with_models_dev_catalog(store, Arc::new(FixedClock(1)), models_dev);
        let mut descriptor = provider_descriptor(crate::ModelDiscoverySource::ProviderApi);
        descriptor.id = "chatgpt".into();
        descriptor.display_name = "ChatGPT subscription".into();
        descriptor.credential_source = CredentialSource::Subscription;
        descriptor.credential_environment_variable = None;
        let mut provider_settings = settings(true);
        provider_settings.kind = crate::ProviderKind::Chatgpt;
        provider_settings.api_key_env = None;
        provider_settings.models.clear();

        let catalog = manager
            .current(&descriptor, &provider_settings, Some("pro"))
            .await
            .unwrap();

        assert_eq!(catalog.source, ModelCatalogSource::BundledSeed);
        assert_eq!(catalog.catalog.provider, "chatgpt");
        assert!(catalog.catalog.models.is_empty());
    }

    #[test]
    fn provider_availability_is_joined_to_models_dev_and_unsupported_backends_are_filtered() {
        let models = ["responses-model", "compatible-model", "messages-model"]
            .into_iter()
            .map(|id| ModelDescriptor {
                id: id.into(),
                display_name: id.into(),
                capabilities: ModelCapabilities::default(),
                backend: None,
                raw_metadata: json!({"provider": {"id": id}}),
            })
            .collect();
        let metadata = ModelsDevCatalog::parse(&json!({
            "opencode": {
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
        let mut provider_settings = settings(false);
        provider_settings.kind = crate::ProviderKind::Opencode;
        provider_settings.models.clear();
        let resolved = resolve(
            ModelCatalog {
                provider: "opencode".into(),
                models,
                version: None,
            },
            &provider_settings,
            ModelCatalogSource::Remote,
            Some(0),
            None,
            Some(&metadata),
            &opencode_descriptor(),
            None,
        );

        assert_eq!(
            resolved
                .catalog
                .models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec!["responses-model"]
        );
        assert_eq!(
            resolved.catalog.models[0].backend,
            Some(ModelBackend::OpenAiResponses)
        );
        assert_eq!(
            resolved.catalog.models[0].capabilities.context_window,
            Some(64_000)
        );
    }

    #[tokio::test]
    async fn absent_cache_starts_empty_until_provider_discovery() {
        let temporary = tempfile::TempDir::new().unwrap();
        let store = GlobalStore::open(&temporary.path().join("seed"))
            .await
            .unwrap();
        let manager = CatalogManager::with_clock(store, Arc::new(FixedClock(1)));
        let current = manager
            .current(
                &provider_descriptor(crate::ModelDiscoverySource::ModelsDev),
                &settings(false),
                None,
            )
            .await
            .unwrap();
        assert_eq!(current.source, ModelCatalogSource::BundledSeed);
        assert_eq!(current.catalog.models.len(), 1);
        assert_eq!(current.catalog.models[0].id, "manual-model");
    }

    #[test]
    fn chatgpt_ultra_is_removed_from_cached_and_overridden_efforts() {
        for provider_id in ["chatgpt", "openai"] {
            let mut descriptor = provider_descriptor(crate::ModelDiscoverySource::ProviderApi);
            descriptor.id = provider_id.into();
            let mut settings = settings(false);
            settings.capability_overrides.insert(
                "model".into(),
                ModelCapabilities {
                    reasoning_efforts: Some(vec!["low".into(), "ultra".into(), "max".into()]),
                    ..Default::default()
                },
            );
            let resolved = resolve(
                ModelCatalog {
                    provider: provider_id.into(),
                    models: vec![ModelDescriptor {
                        id: "model".into(),
                        display_name: "Model".into(),
                        capabilities: ModelCapabilities {
                            reasoning_efforts: Some(vec!["ultra".into()]),
                            ..Default::default()
                        },
                        backend: Some(ModelBackend::OpenAiResponses),
                        raw_metadata: Value::Null,
                    }],
                    version: None,
                },
                &settings,
                ModelCatalogSource::Remote,
                Some(0),
                None,
                None,
                &descriptor,
                None,
            );
            let model = resolved.model_or_unknown("model");
            let efforts = model.capabilities.reasoning_efforts.as_ref().unwrap();
            assert_eq!(
                efforts.contains(&"ultra".to_owned()),
                provider_id != "chatgpt"
            );
            assert!(efforts.contains(&"max".to_owned()));
            if provider_id == "chatgpt" {
                let prepared =
                    prepare_model_capabilities(&model, Some("ultra"), false, 32_768).unwrap();
                assert!(prepared.effort.is_none());
                assert!(
                    prepared
                        .notices
                        .iter()
                        .any(|notice| notice.contains("ultra"))
                );
            }
        }
    }

    #[test]
    fn aliases_unknown_models_and_capability_degradation_are_explicit() {
        let resolved = resolve(
            ModelCatalog {
                provider: "openai".into(),
                models: vec![ModelDescriptor {
                    id: "known".into(),
                    display_name: "Known".into(),
                    capabilities: ModelCapabilities {
                        context_window: None,
                        supports_fast_mode: None,
                        supports_streaming: Some(true),
                        supports_tools: Some(true),
                        supports_structured_output: None,
                        supports_text_input: None,
                        supports_image_input: None,
                        supports_text_output: None,
                        reasoning_control: Some(crate::ReasoningControl::Effort),
                        reasoning_efforts: Some(vec!["low".into()]),
                    },
                    backend: None,
                    raw_metadata: Value::Null,
                }],
                version: None,
            },
            &ProviderSettings {
                aliases: BTreeMap::from([("daily".into(), "known".into())]),
                ..settings(false)
            },
            ModelCatalogSource::Remote,
            Some(0),
            None,
            None,
            &provider_descriptor(crate::ModelDiscoverySource::ProviderApi),
            None,
        );
        assert_eq!(resolved.model_or_unknown("daily").id, "known");
        assert_eq!(
            resolved
                .model_or_unknown("gpt-5.6-luna")
                .capabilities
                .context_window,
            None
        );
        assert_eq!(
            resolved.model_or_unknown("brand-new").raw_metadata["source"],
            "manual_unknown"
        );
        let prepared = prepare_model_capabilities(
            &resolved.model_or_unknown("daily"),
            Some("high"),
            true,
            32_768,
        )
        .unwrap();
        assert_eq!(prepared.effort, None);
        assert_eq!(prepared.context_window, 32_768);
        assert_eq!(prepared.notices.len(), 2);

        let no_tools = ModelDescriptor {
            capabilities: ModelCapabilities {
                supports_tools: Some(false),
                ..ModelCapabilities::default()
            },
            ..resolved.model_or_unknown("brand-new")
        };
        assert!(prepare_model_capabilities(&no_tools, None, true, 32_768).is_err());
    }
}
