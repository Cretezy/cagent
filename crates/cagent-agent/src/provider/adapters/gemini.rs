#![allow(
    clippy::obfuscated_if_else,
    clippy::unnecessary_unwrap,
    clippy::unnecessary_get_then_check
)] // Provider request shaping follows Gemini's optional-field protocol.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use google_cloud_auth::credentials::{AccessTokenCredentials, Builder};
use serde_json::Value;
use tokio::sync::{Mutex, OnceCell, RwLock, mpsc};
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::CancellationToken;

use crate::config::VertexAuth;
use crate::{
    AuthState, CredentialSource, ModelBackend, ModelCapabilities, ModelCatalog, ModelDescriptor,
    ModelDiscoverySource, ModelRequest, Provider, ProviderDescriptor, ProviderError,
    ProviderFuture, ProviderStream,
};

use crate::provider::codecs::gemini::{self, GeminiProfile};
use crate::provider::prompt_cache::{
    CACHE_ADVANCE_TOKENS, CACHE_FAILURE_COOLDOWN, NAMED_CACHE_TTL, NamedCacheIdentity,
    NamedCacheResource, PromptCacheCoordinator, fingerprint, prefix_hash,
};
use crate::store::GlobalStore;

const DEVELOPER_BASE_URL: &str = "https://generativelanguage.googleapis.com/v1beta";
const VERTEX_EXPRESS_BASE_URL: &str = "https://aiplatform.googleapis.com/v1";
const ZEN_BASE_URL: &str = "https://opencode.ai/zen/v1";
const GOOGLE_KEY_FALLBACKS: &[&str] = &[
    "GOOGLE_API_KEY",
    "GEMINI_API_KEY",
    "GOOGLE_GENERATIVE_AI_API_KEY",
];
const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
#[derive(Clone, Debug)]
struct ResolvedCache {
    resource: NamedCacheResource,
    cache_write_tokens: Option<u64>,
}

/// One Google GenerateContent adapter profiled for Gemini, Vertex, Express, and Zen.
#[derive(Clone, Debug)]
pub struct GeminiProvider {
    client: reqwest::Client,
    provider_id: String,
    display_name: String,
    profile: GeminiProfile,
    base_url: String,
    key_variable: String,
    credentials: crate::provider::CredentialStore,
    api_key: Arc<RwLock<Option<crate::provider::ManagedApiKey>>>,
    vertex_project: Option<String>,
    vertex_location: String,
    adc: Arc<OnceCell<AccessTokenCredentials>>,
    cache_store: Option<GlobalStore>,
    cache_resources: Arc<Mutex<HashMap<NamedCacheIdentity, NamedCacheResource>>>,
    cache_cooldowns: Arc<Mutex<HashMap<(NamedCacheIdentity, String), Instant>>>,
    cache_coordinator: PromptCacheCoordinator,
    descriptor: ProviderDescriptor,
    #[cfg(test)]
    api_key_override: Option<String>,
    #[cfg(test)]
    adc_token_override: Option<String>,
}

impl GeminiProvider {
    #[must_use]
    pub fn google() -> Self {
        Self::new(
            "google",
            "Google Gemini",
            GeminiProfile::Developer,
            DEVELOPER_BASE_URL,
            "GOOGLE_API_KEY",
            None,
            None,
        )
    }

    #[must_use]
    pub fn vertex(auth: VertexAuth, project: Option<String>, location: Option<String>) -> Self {
        let profile = if auth == VertexAuth::ApiKey {
            GeminiProfile::VertexExpress
        } else {
            GeminiProfile::Vertex
        };
        Self::new(
            "google-vertex",
            "Google Vertex AI",
            profile,
            if profile == GeminiProfile::VertexExpress {
                VERTEX_EXPRESS_BASE_URL
            } else {
                ""
            },
            "GOOGLE_API_KEY",
            project,
            location,
        )
    }

    #[must_use]
    pub fn open_code() -> Self {
        Self::new(
            "opencode",
            "OpenCode Zen",
            GeminiProfile::Zen,
            ZEN_BASE_URL,
            "OPENCODE_API_KEY",
            None,
            None,
        )
    }

    fn new(
        provider_id: &str,
        display_name: &str,
        profile: GeminiProfile,
        base_url: &str,
        key_variable: &str,
        vertex_project: Option<String>,
        vertex_location: Option<String>,
    ) -> Self {
        let credentials =
            crate::provider::CredentialStore::api_key(Path::new("."), "default", provider_id);
        Self {
            client: reqwest::Client::new(),
            provider_id: provider_id.into(),
            display_name: display_name.into(),
            profile,
            base_url: base_url.trim_end_matches('/').into(),
            key_variable: key_variable.into(),
            api_key: Arc::new(RwLock::new(credentials.load_api_key().ok().flatten())),
            credentials,
            vertex_project,
            vertex_location: vertex_location.unwrap_or_else(|| "global".into()),
            adc: Arc::new(OnceCell::new()),
            cache_store: None,
            cache_resources: Arc::new(Mutex::new(HashMap::new())),
            cache_cooldowns: Arc::new(Mutex::new(HashMap::new())),
            cache_coordinator: PromptCacheCoordinator::default(),
            descriptor: ProviderDescriptor {
                id: provider_id.into(),
                display_name: display_name.into(),
                default_model_backend: Some(ModelBackend::Gemini),
                supported_model_backends: vec![ModelBackend::Gemini],
                model_discovery: if profile == GeminiProfile::Developer {
                    ModelDiscoverySource::ProviderApi
                } else {
                    ModelDiscoverySource::ModelsDev
                },
                credential_source: CredentialSource::ApiKey,
                credential_environment_variable: Some(key_variable.into()),
                supports_managed_api_key: profile != GeminiProfile::Vertex,
                auth_flows: Vec::new(),
            },
            #[cfg(test)]
            api_key_override: None,
            #[cfg(test)]
            adc_token_override: None,
        }
    }

    #[must_use]
    pub fn with_credential_dir(mut self, data_dir: &Path) -> Self {
        self.credentials =
            crate::provider::CredentialStore::api_key(data_dir, "default", &self.provider_id);
        self.api_key = Arc::new(RwLock::new(self.credentials.load_api_key().ok().flatten()));
        self
    }

    pub(crate) fn with_cache_store(mut self, store: GlobalStore) -> Self {
        self.cache_store = Some(store);
        self
    }

    #[must_use]
    pub fn with_environment_variable(mut self, variable: impl Into<String>) -> Self {
        self.key_variable = variable.into();
        self.descriptor.credential_environment_variable = Some(self.key_variable.clone());
        self
    }

    #[must_use]
    pub fn with_endpoint(mut self, base_url: impl Into<String>) -> Self {
        self.base_url = base_url.into().trim_end_matches('/').into();
        self
    }

    #[cfg(test)]
    fn with_test_api_key(mut self, key: impl Into<String>) -> Self {
        self.api_key_override = Some(key.into());
        self
    }

    async fn api_key(&self) -> Result<String, ProviderError> {
        #[cfg(test)]
        if let Some(key) = &self.api_key_override {
            return Ok(key.clone());
        }
        if let Some(key) = self.api_key.read().await.as_ref() {
            return Ok(key.value.clone());
        }
        crate::provider::credentials::environment_api_key_with_fallbacks(
            &self.key_variable,
            GOOGLE_KEY_FALLBACKS,
        )
    }

    async fn adc_token(&self) -> Result<String, ProviderError> {
        #[cfg(test)]
        if let Some(token) = &self.adc_token_override {
            return Ok(token.clone());
        }
        let credentials = self
            .adc
            .get_or_try_init(|| async {
                Builder::default()
                    .with_scopes([CLOUD_PLATFORM_SCOPE])
                    .build_access_token_credentials()
                    .map_err(|error| {
                        ProviderError::configuration(format!("unable to load Google ADC: {error}"))
                    })
            })
            .await?;
        credentials
            .access_token()
            .await
            .map(|token| token.token)
            .map_err(|error| {
                ProviderError::configuration(format!("unable to obtain Google ADC token: {error}"))
            })
    }

    fn endpoint(&self, model: &str) -> Result<String, ProviderError> {
        let model = model.trim_start_matches("models/");
        match self.profile {
            GeminiProfile::Developer | GeminiProfile::Zen | GeminiProfile::VertexExpress => {
                Ok(format!(
                    "{}/models/{}:streamGenerateContent?alt=sse",
                    self.base_url, model
                ))
            }
            GeminiProfile::Vertex => {
                let project = self
                    .vertex_project
                    .as_deref()
                    .filter(|project| !project.trim().is_empty())
                    .ok_or_else(|| {
                        ProviderError::configuration(
                            "providers.google-vertex.project is required when auth = \"adc\"",
                        )
                    })?;
                Ok(format!(
                    "{}/publishers/google/models/{}:streamGenerateContent?alt=sse",
                    self.vertex_api_root(project),
                    model
                ))
            }
        }
    }

    fn vertex_api_root(&self, project: &str) -> String {
        if self.base_url.is_empty() {
            format!(
                "https://{}-aiplatform.googleapis.com/v1/projects/{}/locations/{}",
                self.vertex_location, project, self.vertex_location
            )
        } else {
            self.base_url.clone()
        }
    }

    fn cache_endpoint_and_model(
        &self,
        model: &str,
    ) -> Result<Option<(String, String)>, ProviderError> {
        let model = model.trim_start_matches("models/");
        match self.profile {
            GeminiProfile::Developer => Ok(Some((
                format!("{}/cachedContents", self.base_url),
                format!("models/{model}"),
            ))),
            GeminiProfile::Vertex => {
                let project = self
                    .vertex_project
                    .as_deref()
                    .filter(|project| !project.trim().is_empty())
                    .ok_or_else(|| {
                        ProviderError::configuration(
                            "providers.google-vertex.project is required when auth = \"adc\"",
                        )
                    })?;
                let root = self.vertex_api_root(project);
                Ok(Some((
                    format!("{root}/cachedContents"),
                    format!(
                        "projects/{project}/locations/{}/publishers/google/models/{model}",
                        self.vertex_location
                    ),
                )))
            }
            GeminiProfile::VertexExpress | GeminiProfile::Zen => Ok(None),
        }
    }

    fn credential_scope_hash(
        &self,
        api_key: Option<&str>,
        adc_token: Option<&str>,
    ) -> Option<String> {
        match self.profile {
            GeminiProfile::Developer => api_key.map(fingerprint),
            GeminiProfile::Vertex => {
                let credential_path = std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS")
                    .map(std::path::PathBuf::from)
                    .or_else(|| {
                        directories::BaseDirs::new().map(|dirs| {
                            dirs.home_dir()
                                .join(".config/gcloud/application_default_credentials.json")
                        })
                    });
                let mut scope = format!(
                    "vertex:{}:{}:",
                    self.vertex_project.as_deref().unwrap_or_default(),
                    self.vertex_location
                )
                .into_bytes();
                if let Some(credentials) = credential_path.and_then(|path| std::fs::read(path).ok())
                {
                    scope.extend(credentials);
                } else {
                    // Metadata/workload credentials do not expose a stable
                    // principal identifier. A token fingerprint is safer than
                    // risking cross-account reuse, at the cost of reuse after
                    // a token rotation on those environments.
                    scope.extend_from_slice(adc_token?.as_bytes());
                }
                Some(fingerprint(scope))
            }
            GeminiProfile::VertexExpress | GeminiProfile::Zen => None,
        }
    }

    async fn cached_content(
        &self,
        request: &ModelRequest,
        api_key: Option<&str>,
        adc_token: Option<&str>,
    ) -> Option<ResolvedCache> {
        let cache_request = request.prompt_cache.as_ref()?;
        let (create_endpoint, model_resource) =
            self.cache_endpoint_and_model(&request.model.model).ok()??;
        let identity = NamedCacheIdentity {
            provider: self.provider_id.clone(),
            credential_scope_hash: self.credential_scope_hash(api_key, adc_token)?,
            model: request.model.model.trim_start_matches("models/").to_owned(),
            conversation_key_hash: fingerprint(&cache_request.key),
        };
        let _single_flight = self.cache_coordinator.lock(&identity).await;
        let now = now_millis();

        self.cache_resources
            .lock()
            .await
            .retain(|_, resource| resource.expires_at_millis > now);
        let instant_now = Instant::now();
        self.cache_cooldowns
            .lock()
            .await
            .retain(|_, retry_at| *retry_at > instant_now);

        let mut current = self.cache_resources.lock().await.get(&identity).cloned();
        if current.is_none()
            && let Some(store) = &self.cache_store
        {
            current = store
                .load_prompt_cache_resource(identity.clone())
                .await
                .ok()
                .flatten();
        }
        if let Some(resource) = current.as_ref() {
            let valid = resource.expires_at_millis > now
                && gemini::cached_content_body(
                    request,
                    self.profile,
                    &model_resource,
                    resource.cached_input_boundary,
                )
                .ok()
                .flatten()
                .and_then(|body| prefix_hash(&body))
                .as_deref()
                    == Some(resource.prefix_hash.as_str());
            if valid {
                self.cache_resources
                    .lock()
                    .await
                    .insert(identity.clone(), resource.clone());
            } else {
                tracing::debug!(provider = %self.provider_id, model = %request.model.model, "discarding expired or mismatched prompt cache resource");
                self.invalidate_cached_content(&identity).await;
                current = None;
            }
        }

        let candidate_boundary = gemini::largest_safe_cache_boundary(&request.input);
        let desired_boundary = current.as_ref().map_or_else(
            || {
                (candidate_boundary > 0
                    && gemini::estimated_input_tokens(&request.input[..candidate_boundary])
                        >= CACHE_ADVANCE_TOKENS)
                    .then_some(candidate_boundary)
                    .unwrap_or_default()
            },
            |resource| {
                let growth = candidate_boundary
                    .checked_sub(resource.cached_input_boundary)
                    .map(|_| {
                        gemini::estimated_input_tokens(
                            &request.input[resource.cached_input_boundary..candidate_boundary],
                        )
                    })
                    .unwrap_or_default();
                if candidate_boundary > resource.cached_input_boundary
                    && growth >= CACHE_ADVANCE_TOKENS
                {
                    candidate_boundary
                } else {
                    resource.cached_input_boundary
                }
            },
        );
        if let Some(resource) = current
            .as_ref()
            .filter(|resource| resource.cached_input_boundary == desired_boundary)
        {
            tracing::debug!(provider = %self.provider_id, model = %request.model.model, cached_tokens = resource.cached_token_count, "reusing prompt cache resource");
            return Some(ResolvedCache {
                resource: resource.clone(),
                cache_write_tokens: None,
            });
        }

        let body =
            gemini::cached_content_body(request, self.profile, &model_resource, desired_boundary)
                .ok()??;
        let body_hash = prefix_hash(&body)?;
        let cooldown_key = (identity.clone(), body_hash.clone());
        if self
            .cache_cooldowns
            .lock()
            .await
            .contains_key(&cooldown_key)
        {
            return current.map(|resource| ResolvedCache {
                resource,
                cache_write_tokens: None,
            });
        }
        let mut create = self
            .client
            .post(create_endpoint)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .json(&body);
        create = match self.profile {
            GeminiProfile::Developer => {
                create.header("x-goog-api-key", api_key.unwrap_or_default())
            }
            GeminiProfile::Vertex => create.bearer_auth(adc_token.unwrap_or_default()),
            GeminiProfile::VertexExpress | GeminiProfile::Zen => return None,
        };
        let response = match create.send().await {
            Ok(response) if response.status().is_success() => response,
            Ok(response) => {
                tracing::debug!(provider = %self.provider_id, model = %request.model.model, status = %response.status(), "prompt cache creation failed; retaining previous resource");
                self.cache_cooldowns
                    .lock()
                    .await
                    .insert(cooldown_key, Instant::now() + CACHE_FAILURE_COOLDOWN);
                return current.map(|resource| ResolvedCache {
                    resource,
                    cache_write_tokens: None,
                });
            }
            Err(error) => {
                tracing::debug!(%error, provider = %self.provider_id, model = %request.model.model, "prompt cache creation failed; retaining previous resource");
                self.cache_cooldowns
                    .lock()
                    .await
                    .insert(cooldown_key, Instant::now() + CACHE_FAILURE_COOLDOWN);
                return current.map(|resource| ResolvedCache {
                    resource,
                    cache_write_tokens: None,
                });
            }
        };
        let payload = match response.json::<Value>().await {
            Ok(payload) => payload,
            Err(error) => {
                tracing::debug!(%error, provider = %self.provider_id, model = %request.model.model, "invalid prompt cache creation response; retaining previous resource");
                self.cache_cooldowns
                    .lock()
                    .await
                    .insert(cooldown_key, Instant::now() + CACHE_FAILURE_COOLDOWN);
                return current.map(|resource| ResolvedCache {
                    resource,
                    cache_write_tokens: None,
                });
            }
        };
        let name = payload.get("name").and_then(Value::as_str)?.to_owned();
        let cached_tokens = payload
            .pointer("/usageMetadata/totalTokenCount")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        let expires_at_millis = payload
            .get("expireTime")
            .and_then(Value::as_str)
            .and_then(parse_rfc3339_millis)
            .unwrap_or_else(|| now.saturating_add(NAMED_CACHE_TTL.as_millis() as u64));
        let resource = NamedCacheResource {
            provider: identity.provider.clone(),
            credential_scope_hash: identity.credential_scope_hash.clone(),
            model: identity.model.clone(),
            conversation_key_hash: identity.conversation_key_hash.clone(),
            prefix_hash: body_hash,
            cached_input_boundary: desired_boundary,
            resource_name: name,
            cached_token_count: cached_tokens,
            created_at_millis: now,
            expires_at_millis,
        };
        self.cache_resources
            .lock()
            .await
            .insert(identity.clone(), resource.clone());
        if let Some(store) = &self.cache_store
            && let Err(error) = store.save_prompt_cache_resource(resource.clone()).await
        {
            tracing::debug!(%error, provider = %self.provider_id, model = %request.model.model, "failed to persist prompt cache resource");
        }
        tracing::debug!(provider = %self.provider_id, model = %request.model.model, cached_tokens, boundary = desired_boundary, "created prompt cache resource");
        if current.is_some() {
            tracing::debug!(provider = %self.provider_id, model = %request.model.model, "superseded prompt cache resource will expire at its short TTL");
        }
        Some(ResolvedCache {
            resource,
            cache_write_tokens: Some(cached_tokens),
        })
    }

    async fn invalidate_cached_content(&self, identity: &NamedCacheIdentity) {
        self.cache_resources.lock().await.remove(identity);
        if let Some(store) = &self.cache_store {
            let _ = store.delete_prompt_cache_resource(identity.clone()).await;
        }
    }

    async fn invalidate_resolved_cache(&self, cache: &ResolvedCache) {
        self.invalidate_cached_content(&cache.resource.identity())
            .await;
        tracing::debug!(provider = %self.provider_id, model = %cache.resource.model, cached_tokens = cache.resource.cached_token_count, "invalidated prompt cache resource");
    }
}

fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as u64
}

fn parse_rfc3339_millis(value: &str) -> Option<u64> {
    let timestamp =
        time::OffsetDateTime::parse(value, &time::format_description::well_known::Rfc3339).ok()?;
    u64::try_from(timestamp.unix_timestamp_nanos() / 1_000_000).ok()
}

fn is_missing_cache_error(status: u16, body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    matches!(status, 400 | 404)
        && [
            "cachedcontent not found",
            "cached content not found",
            "cachedcontents/",
            "cachedcontent has expired",
            "cached content has expired",
            "invalid cachedcontent",
            "invalid cached content",
        ]
        .iter()
        .any(|needle| body.contains(needle))
}

impl Provider for GeminiProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async move {
            let available = match self.profile {
                GeminiProfile::Vertex => self.adc_token().await.is_ok(),
                _ => self.api_key().await.is_ok(),
            };
            Ok(if available {
                AuthState::Available {
                    detail: if self.profile == GeminiProfile::Vertex {
                        "Application Default Credentials found".into()
                    } else {
                        "key found".into()
                    },
                }
            } else {
                AuthState::Missing
            })
        })
    }

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        (self.profile == GeminiProfile::Developer).then(|| {
            Box::pin(async move {
                let key = self.api_key().await?;
                let mut page_token = None;
                let mut models = Vec::new();
                loop {
                    let mut request = self
                        .client
                        .get(format!("{}/models", self.base_url))
                        .header("x-goog-api-key", &key);
                    if let Some(token) = page_token.as_deref() {
                        request = request.query(&[("pageToken", token)]);
                    }
                    let response = request.send().await.map_err(|error| {
                        ProviderError::connection("model_discovery", error.to_string())
                    })?;
                    if !response.status().is_success() {
                        return Err(gemini::http_error(response).await);
                    }
                    let payload: Value = response.json().await.map_err(|error| {
                        ProviderError::protocol("invalid_models_response", error.to_string())
                    })?;
                    models.extend(parse_developer_models(&payload));
                    page_token = payload
                        .get("nextPageToken")
                        .and_then(Value::as_str)
                        .map(str::to_owned);
                    if page_token.is_none() {
                        break;
                    }
                }
                Ok(ModelCatalog {
                    provider: self.provider_id.clone(),
                    models,
                    version: None,
                })
            }) as ProviderFuture<'_, Result<ModelCatalog, ProviderError>>
        })
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        Box::pin(async move {
            if request.model.provider != self.provider_id {
                return Err(ProviderError::configuration(format!(
                    "{} adapter cannot serve {}",
                    self.display_name, request.model
                )));
            }
            if request
                .backend
                .is_some_and(|backend| backend != ModelBackend::Gemini)
            {
                return Err(ProviderError::configuration(format!(
                    "{} does not support the requested model backend",
                    self.display_name
                )));
            }
            let endpoint = self.endpoint(&request.model.model)?;
            let api_key = if self.profile == GeminiProfile::Vertex {
                None
            } else {
                Some(self.api_key().await?)
            };
            let adc_token = if self.profile == GeminiProfile::Vertex {
                Some(self.adc_token().await?)
            } else {
                None
            };
            let cached_content = self
                .cached_content(&request, api_key.as_deref(), adc_token.as_deref())
                .await;
            let body = gemini::request_body_with_cached_content(
                &request,
                self.profile,
                cached_content
                    .as_ref()
                    .map(|cache| cache.resource.resource_name.as_str()),
                cached_content
                    .as_ref()
                    .map_or(0, |cache| cache.resource.cached_input_boundary),
            )?;
            let send = |body: &Value| {
                let request = self
                    .client
                    .post(&endpoint)
                    .header(reqwest::header::CONTENT_TYPE, "application/json")
                    .json(body);
                match self.profile {
                    GeminiProfile::Vertex => {
                        request.bearer_auth(adc_token.as_deref().unwrap_or_default())
                    }
                    _ => request.header("x-goog-api-key", api_key.as_deref().unwrap_or_default()),
                }
            };
            let mut response = send(&body)
                .send()
                .await
                .map_err(|error| ProviderError::connection("request", error.to_string()))?;
            if cached_content.is_some() && matches!(response.status().as_u16(), 400 | 404) {
                let status = response.status();
                let error_body = response.text().await.unwrap_or_default();
                if is_missing_cache_error(status.as_u16(), &error_body) {
                    self.invalidate_resolved_cache(cached_content.as_ref().unwrap())
                        .await;
                    let fallback_body = gemini::request_body(&request, self.profile)?;
                    response = send(&fallback_body)
                        .send()
                        .await
                        .map_err(|error| ProviderError::connection("request", error.to_string()))?;
                } else {
                    return Err(gemini::http_error_from_parts(status, error_body));
                }
            }
            if !response.status().is_success() {
                return Err(gemini::http_error(response).await);
            }
            let (sender, receiver) = mpsc::channel(64);
            tokio::spawn(gemini::map_sse(
                response,
                sender,
                cancellation,
                self.profile,
                cached_content
                    .as_ref()
                    .and_then(|cache| cache.cache_write_tokens),
            ));
            Ok(Box::pin(ReceiverStream::new(receiver)) as ProviderStream)
        })
    }

    fn set_api_key(&self, key: String) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            if self.profile == GeminiProfile::Vertex {
                return Err(ProviderError::configuration(
                    "Vertex ADC credentials are managed outside Cagent",
                ));
            }
            if key.trim().is_empty() {
                return Err(ProviderError::configuration("API key must not be blank"));
            }
            let key = crate::provider::ManagedApiKey { value: key };
            self.credentials.save_api_key(&key)?;
            *self.api_key.write().await = Some(key);
            Ok(())
        })
    }
    fn remove_api_key(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async move {
            self.credentials.delete_api_key()?;
            *self.api_key.write().await = None;
            Ok(())
        })
    }
    fn has_managed_api_key(&self) -> ProviderFuture<'_, bool> {
        Box::pin(async move { self.api_key.read().await.is_some() })
    }
}

fn parse_developer_models(payload: &Value) -> Vec<ModelDescriptor> {
    payload
        .get("models")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|model| {
            let methods = model
                .get("supportedGenerationMethods")
                .and_then(Value::as_array);
            if !methods.is_some_and(|methods| {
                methods
                    .iter()
                    .any(|method| method.as_str() == Some("generateContent"))
            }) {
                return None;
            }
            let name = model.get("name").and_then(Value::as_str)?;
            let id = name.trim_start_matches("models/").to_owned();
            Some(ModelDescriptor {
                id: id.clone(),
                display_name: model
                    .get("displayName")
                    .and_then(Value::as_str)
                    .unwrap_or(&id)
                    .into(),
                capabilities: ModelCapabilities {
                    context_window: model.get("inputTokenLimit").and_then(Value::as_u64),
                    supports_streaming: Some(true),
                    supports_tools: Some(true),
                    supports_text_input: Some(true),
                    supports_image_input: Some(true),
                    supports_text_output: Some(true),
                    ..ModelCapabilities::default()
                },
                backend: Some(ModelBackend::Gemini),
                raw_metadata: model.clone(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::{Query, State},
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::{get, post},
    };
    use serde_json::json;
    use std::{collections::HashMap, sync::Mutex};

    #[derive(Default)]
    struct DiscoveryState {
        requests: Mutex<Vec<(Option<String>, Option<String>)>>,
    }

    #[derive(Default)]
    struct CacheState {
        requests: Mutex<Vec<(Value, Option<String>)>>,
    }

    fn cache_request() -> ModelRequest {
        ModelRequest {
            request_id: crate::RequestId::new(),
            attempt_id: crate::AttemptId::new(),
            model: crate::ModelRef::parse("google/gemini-2.5-flash").unwrap(),
            backend: Some(ModelBackend::Gemini),
            backend_candidates: vec![ModelBackend::Gemini],
            effort: None,
            service_tier: None,
            input: Vec::new(),
            tools: vec![crate::ToolDefinition {
                name: "read".into(),
                description: "Read a file".into(),
                input_schema: json!({"type":"object"}),
                asynchronous: false,
            }],
            stable_prompt: vec![crate::StablePromptPart {
                identity: "policy".into(),
                content: "stable policy".into(),
            }],
            prompt_cache: Some(crate::PromptCacheRequest {
                key: "cagent:conversation:test".into(),
                scope: crate::PromptCacheScope::Conversation,
            }),
            response_transport_continuation: None,
            allow_parallel_tools: true,
            structured_output: None,
        }
    }

    #[test]
    fn discovery_normalizes_google_names_and_filters_non_generation_models() {
        let models = parse_developer_models(&json!({ "models": [
            { "name": "models/gemini-2.5-flash", "displayName": "Flash", "inputTokenLimit": 1048576, "supportedGenerationMethods": ["generateContent", "countTokens"] },
            { "name": "models/text-embedding-004", "supportedGenerationMethods": ["embedContent"] }
        ] }));
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "gemini-2.5-flash");
        assert_eq!(models[0].capabilities.context_window, Some(1_048_576));
    }

    #[test]
    fn only_cache_specific_generation_errors_trigger_fallback() {
        assert!(is_missing_cache_error(
            404,
            r#"{"error":{"message":"CachedContent cachedContents/one not found"}}"#
        ));
        assert!(is_missing_cache_error(
            400,
            r#"{"error":{"message":"Cached content has expired"}}"#
        ));
        assert!(!is_missing_cache_error(
            400,
            r#"{"error":{"message":"unrecognized type at properties.timeout"}}"#
        ));
        assert!(!is_missing_cache_error(
            404,
            r#"{"error":{"message":"model not found"}}"#
        ));
    }

    #[tokio::test]
    async fn discovery_paginates_filters_and_authenticates_google_models() {
        async fn models(
            State(state): State<Arc<DiscoveryState>>,
            Query(query): Query<HashMap<String, String>>,
            headers: HeaderMap,
        ) -> impl IntoResponse {
            state.requests.lock().unwrap().push((
                query.get("pageToken").cloned(),
                headers
                    .get("x-goog-api-key")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            ));
            if query.get("pageToken").is_none() {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "models": [
                            {"name": "models/gemini-first", "supportedGenerationMethods": ["generateContent"]},
                            {"name": "models/embed", "supportedGenerationMethods": ["embedContent"]}
                        ],
                        "nextPageToken": "second"
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "models": [{"name": "models/gemini-second", "supportedGenerationMethods": ["generateContent"]}]
                    })),
                )
                    .into_response()
            }
        }

        let state = Arc::new(DiscoveryState::default());
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            // Some sandboxed test runners prohibit opening loopback sockets.
            return;
        };
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/models", get(models))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let provider = GeminiProvider::google()
            .with_endpoint(format!("http://{address}"))
            .with_test_api_key("test-key");
        let catalog = provider.discover_models().unwrap().await.unwrap();
        server.abort();

        assert_eq!(
            catalog
                .models
                .iter()
                .map(|model| &model.id)
                .collect::<Vec<_>>(),
            ["gemini-first", "gemini-second"]
        );
        assert_eq!(
            *state.requests.lock().unwrap(),
            vec![
                (None, Some("test-key".into())),
                (Some("second".into()), Some("test-key".into())),
            ]
        );
    }

    #[tokio::test]
    async fn explicit_cache_is_created_once_and_reused_after_restart() {
        async fn create_cache(
            State(state): State<Arc<CacheState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            state.requests.lock().unwrap().push((
                body,
                headers
                    .get("x-goog-api-key")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            ));
            axum::Json(json!({
                "name": "cachedContents/cagent-cache",
                "usageMetadata": {"totalTokenCount": 123}
            }))
        }

        let state = Arc::new(CacheState::default());
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/cachedContents", post(create_cache))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });

        let temporary = tempfile::tempdir().unwrap();
        let store = GlobalStore::open(temporary.path()).await.unwrap();
        let provider = GeminiProvider::google()
            .with_endpoint(format!("http://{address}"))
            .with_cache_store(store.clone())
            .with_test_api_key("test-key");
        let request = cache_request();
        let (first, concurrent) = tokio::join!(
            provider.cached_content(&request, Some("test-key"), None),
            provider.cached_content(&request, Some("test-key"), None),
        );
        let first = first.unwrap();
        let concurrent = concurrent.unwrap();
        drop(provider);
        let restarted = GeminiProvider::google()
            .with_endpoint(format!("http://{address}"))
            .with_cache_store(store)
            .with_test_api_key("test-key");
        let second = restarted
            .cached_content(&request, Some("test-key"), None)
            .await
            .unwrap();
        server.abort();

        assert_eq!(first.resource.resource_name, "cachedContents/cagent-cache");
        assert_eq!(
            [first.cache_write_tokens, concurrent.cache_write_tokens]
                .into_iter()
                .filter(Option::is_some)
                .count(),
            1
        );
        assert_eq!(second.resource.resource_name, first.resource.resource_name);
        assert_eq!(second.cache_write_tokens, None);
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].0["model"], "models/gemini-2.5-flash");
        assert_eq!(requests[0].0["ttl"], "3600s");
        assert!(requests[0].0.get("tools").is_some());
        assert_eq!(requests[0].1.as_deref(), Some("test-key"));
    }

    #[tokio::test]
    async fn vertex_cache_uses_adc_endpoint_auth_and_publisher_model_resource() {
        async fn create_cache(
            State(state): State<Arc<CacheState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            state.requests.lock().unwrap().push((
                body,
                headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
            ));
            axum::Json(json!({
                "name": "projects/project/locations/us-central1/cachedContents/cache",
                "expireTime": "2099-01-01T00:00:00Z",
                "usageMetadata": {"totalTokenCount": 321}
            }))
        }

        let state = Arc::new(CacheState::default());
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/cachedContents", post(create_cache))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let provider = GeminiProvider::vertex(
            VertexAuth::Adc,
            Some("project".into()),
            Some("us-central1".into()),
        )
        .with_endpoint(format!("http://{address}"));
        let mut request = cache_request();
        request.model.provider = "google-vertex".into();
        let cache = provider
            .cached_content(&request, None, Some("vertex-token"))
            .await
            .unwrap();
        server.abort();

        assert_eq!(cache.cache_write_tokens, Some(321));
        let requests = state.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(
            requests[0].0["model"],
            "projects/project/locations/us-central1/publishers/google/models/gemini-2.5-flash"
        );
        assert_eq!(requests[0].1.as_deref(), Some("Bearer vertex-token"));
    }

    #[tokio::test]
    async fn failed_rolling_replacement_keeps_the_previous_resource() {
        async fn create_cache(
            State(state): State<Arc<CacheState>>,
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> impl IntoResponse {
            let request_number = {
                let mut requests = state.requests.lock().unwrap();
                requests.push((
                    body,
                    headers
                        .get("x-goog-api-key")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_owned),
                ));
                requests.len()
            };
            if request_number == 1 {
                (
                    StatusCode::OK,
                    axum::Json(json!({
                        "name": "cachedContents/original",
                        "usageMetadata": {"totalTokenCount": 2_500}
                    })),
                )
                    .into_response()
            } else {
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    axum::Json(json!({"error":{"message":"temporary"}})),
                )
                    .into_response()
            }
        }

        let state = Arc::new(CacheState::default());
        let Ok(listener) = tokio::net::TcpListener::bind("127.0.0.1:0").await else {
            return;
        };
        let address = listener.local_addr().unwrap();
        let server_state = state.clone();
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new()
                    .route("/cachedContents", post(create_cache))
                    .with_state(server_state),
            )
            .await
            .unwrap();
        });
        let provider = GeminiProvider::google()
            .with_endpoint(format!("http://{address}"))
            .with_test_api_key("test-key");
        let mut request = cache_request();
        let original = provider
            .cached_content(&request, Some("test-key"), None)
            .await
            .unwrap();
        request.input = vec![
            crate::ModelInput::Message {
                role: crate::MessageRole::User,
                content: "x".repeat(9_000),
            },
            crate::ModelInput::Message {
                role: crate::MessageRole::Assistant,
                content: "y".repeat(9_000),
            },
            crate::ModelInput::Message {
                role: crate::MessageRole::User,
                content: "continue".into(),
            },
        ];
        let retained = provider
            .cached_content(&request, Some("test-key"), None)
            .await
            .unwrap();
        server.abort();

        assert_eq!(original.resource.resource_name, "cachedContents/original");
        assert_eq!(retained.resource.resource_name, "cachedContents/original");
        assert_eq!(state.requests.lock().unwrap().len(), 2);
    }

    #[test]
    fn express_and_zen_never_create_named_cache_resources() {
        let express = GeminiProvider::vertex(VertexAuth::ApiKey, None, None);
        let zen = GeminiProvider::open_code();
        assert_eq!(
            express
                .cache_endpoint_and_model("gemini-2.5-flash")
                .unwrap(),
            None
        );
        assert_eq!(
            zen.cache_endpoint_and_model("gemini-2.5-flash").unwrap(),
            None
        );
    }
}
