use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use futures_core::Stream;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

pub mod adapters;
pub(crate) mod catalog;
pub(crate) mod codecs;
mod credentials;
mod mock;
pub(crate) mod models_dev;
pub(crate) mod prompt_cache;
mod registry;
pub(crate) mod wire;

pub use adapters::{
    AnthropicProvider, ChatGptProvider, GeminiProvider, GitHubCopilotProvider,
    OpenAiCompatibleProvider, OpenAiProvider, OpenCodeProvider,
};
pub use catalog::{
    ModelCatalogSource, PreparedModelCapabilities, ResolvedModelCatalog, prepare_model_capabilities,
};
pub use credentials::CredentialReference;
pub(crate) use credentials::{
    CredentialStore, ManagedApiKey, ManagedCredential, environment_api_key,
};
pub(crate) use mock::MockProvider;
#[cfg(test)]
pub(crate) use mock::ScriptedMockProvider;
pub(crate) use registry::build_provider_set;
pub use registry::{ProviderDefinition, ProviderEntry, ProviderSet, builtin_provider_definitions};

use crate::{AttemptId, RequestId};

const MOCK_FAST_RESPONSE_DELAY: Duration = Duration::from_millis(800);
const MOCK_RESPONSE_DELAY: Duration = Duration::from_secs(1);
const MOCK_SLOW_RESPONSE_DELAY: Duration = Duration::from_millis(1_200);
const MOCK_STREAM_INTERVAL: Duration = Duration::from_millis(40);

/// An object-safe future returned by provider adapters.
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A normalized stream returned by every provider adapter.
pub type ProviderStream =
    Pin<Box<dyn Stream<Item = Result<ProviderStreamEvent, ProviderError>> + Send + 'static>>;

/// Provider-neutral request for a provider-owned web-search endpoint.
#[derive(Clone, Debug)]
pub struct ProviderWebSearchRequest {
    pub id: String,
    pub model: String,
    pub query: String,
}

/// Provider-neutral response from a provider-owned web-search endpoint.
///
/// Search result objects remain opaque here so the agent layer can normalize
/// them without coupling every provider adapter to one result schema.
#[derive(Clone, Debug)]
pub struct ProviderWebSearchResponse {
    pub results: Vec<Value>,
}

/// Provider-reported account limits normalized for presentation by any frontend.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderUsageReport {
    pub windows: Vec<ProviderUsageWindow>,
}

/// One independently resetting provider usage window.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderUsageWindow {
    /// Stable provider-owned ID used by per-provider display selection.
    pub id: String,
    pub label: String,
    pub remaining_percent: u64,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderProtocol {
    Responses,
    Messages,
    OpenAiCompatible,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelBackend {
    OpenAiResponses,
    OpenAiCompatible,
    AnthropicMessages,
    Gemini,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelDiscoverySource {
    ModelsDev,
    ProviderApi,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum CredentialSource {
    Unauthenticated,
    Subscription,
    Environment,
    Managed,
    ApiKey,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderDescriptor {
    pub id: String,
    pub display_name: String,
    pub default_model_backend: Option<ModelBackend>,
    pub supported_model_backends: Vec<ModelBackend>,
    pub model_discovery: ModelDiscoverySource,
    pub credential_source: CredentialSource,
    pub credential_environment_variable: Option<String>,
    #[serde(default)]
    pub supports_managed_api_key: bool,
    #[serde(default)]
    pub auth_flows: Vec<AuthFlow>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum AuthState {
    Missing,
    Available { detail: String },
    Connected { detail: String },
    Expired { detail: String },
    Error { message: String },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuthFlow {
    BrowserPkce,
    DeviceCode,
}

/// Selects the least disruptive supported authentication flow for an interactive frontend.
#[must_use]
pub fn preferred_auth_flow(flows: &[AuthFlow]) -> Option<AuthFlow> {
    [AuthFlow::BrowserPkce, AuthFlow::DeviceCode]
        .into_iter()
        .find(|flow| flows.contains(flow))
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthChallenge {
    Browser {
        authorization_url: String,
        state: String,
        callback_url: String,
    },
    Device {
        verification_url: String,
        user_code: String,
        device_code: String,
        interval_seconds: u64,
    },
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthResponse {
    AuthorizationCode {
        code: String,
        state: String,
    },
    OAuthError {
        error: String,
        description: Option<String>,
        state: String,
    },
    DeviceCode {
        device_code: String,
    },
    Cancel,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ProviderAvailability {
    pub descriptor: ProviderDescriptor,
    pub enabled: bool,
    pub auth: AuthState,
    #[serde(default)]
    pub has_managed_api_key: bool,
}

impl ProviderAvailability {
    #[must_use]
    pub fn status_text(&self) -> &'static str {
        if matches!(self.auth, AuthState::Missing) {
            "not setup"
        } else if self.enabled {
            "enabled"
        } else {
            "disabled"
        }
    }

    #[must_use]
    pub fn setup_instructions(&self) -> Option<String> {
        if !matches!(self.auth, AuthState::Missing) {
            return None;
        }
        Some(self.configuration_instructions())
    }

    #[must_use]
    pub fn configuration_instructions(&self) -> String {
        match (
            self.descriptor.credential_source,
            self.descriptor.credential_environment_variable.as_deref(),
        ) {
            (CredentialSource::Unauthenticated, _) => {
                format!(
                    "{} does not require authentication.",
                    self.descriptor.display_name
                )
            }
            (CredentialSource::Subscription, _) => match self.descriptor.auth_flows.as_slice() {
                [AuthFlow::DeviceCode] => format!(
                    "Connect your {} with device authentication.",
                    self.descriptor.display_name
                ),
                [AuthFlow::BrowserPkce] => format!(
                    "Connect your {} with browser authentication.",
                    self.descriptor.display_name
                ),
                flows
                    if flows.contains(&AuthFlow::BrowserPkce)
                        && flows.contains(&AuthFlow::DeviceCode) =>
                {
                    format!(
                        "Connect your {} with browser or device authentication.",
                        self.descriptor.display_name
                    )
                }
                _ => format!("Connect your {} account.", self.descriptor.display_name),
            },
            (CredentialSource::ApiKey, Some(variable)) => {
                format!("Enter an API key below. {variable} remains an optional fallback.")
            }
            (_, Some(variable)) => {
                format!("Set {variable} in the environment, then restart Cagent.")
            }
            (_, None) => format!(
                "Configure credentials for {}, then restart Cagent.",
                self.descriptor.display_name
            ),
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct ModelRef {
    pub provider: String,
    pub model: String,
}

impl ModelRef {
    /// Parses a `provider/model` reference, splitting only on the first slash.
    ///
    /// # Errors
    ///
    /// Returns an error when either component is empty or the slash is absent.
    pub fn parse(value: &str) -> Result<Self, ProviderError> {
        let Some((provider, model)) = value.split_once('/') else {
            return Err(ProviderError::configuration(
                "model reference must have the form provider/model",
            ));
        };
        if provider.trim().is_empty() || model.trim().is_empty() {
            return Err(ProviderError::configuration(
                "model provider and model ID must not be empty",
            ));
        }
        Ok(Self {
            provider: provider.into(),
            model: model.into(),
        })
    }
}

impl std::fmt::Display for ModelRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}/{}", self.provider, self.model)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCapabilities {
    pub context_window: Option<u64>,
    pub supports_streaming: Option<bool>,
    pub supports_tools: Option<bool>,
    #[serde(default)]
    pub supports_structured_output: Option<bool>,
    pub supports_text_input: Option<bool>,
    #[serde(default)]
    pub supports_image_input: Option<bool>,
    pub supports_text_output: Option<bool>,
    /// Whether the model advertises a priority-backed Fast service tier.
    #[serde(default)]
    pub supports_fast_mode: Option<bool>,
    #[serde(default)]
    pub reasoning_control: Option<ReasoningControl>,
    pub reasoning_efforts: Option<Vec<String>>,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningControl {
    Effort,
    Toggle,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelDescriptor {
    pub id: String,
    pub display_name: String,
    pub capabilities: ModelCapabilities,
    #[serde(default)]
    pub backend: Option<ModelBackend>,
    #[serde(default)]
    pub raw_metadata: Value,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelCatalog {
    pub provider: String,
    pub models: Vec<ModelDescriptor>,
    pub version: Option<String>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct CachedModelCatalog {
    pub catalog: ModelCatalog,
    pub fetched_at_millis: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MessageRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelInput {
    /// Opaque Responses reasoning, replayable only to the originating provider/model.
    /// Never convert this item to transcript text or send it to another provider.
    ProviderReasoning {
        source: ModelRef,
        item: EncryptedReasoningItem,
    },
    Message {
        role: MessageRole,
        content: String,
    },
    /// Ordered multimodal content. Only user messages may contain images.
    MultimodalMessage {
        role: MessageRole,
        content: Vec<ModelContentPart>,
    },
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
        #[serde(default)]
        provider_metadata: Value,
    },
    ToolResult {
        call_id: String,
        output: Value,
        is_error: bool,
    },
    /// Changes the effective reasoning effort for a retained Responses
    /// conversation without changing the request-level cached prefix.
    ConfigurationUpdate {
        effort: String,
    },
}

/// Provider-owned ciphertext and its original Responses envelope.
/// Debug output deliberately omits all payload fields.
#[derive(Clone, Deserialize, PartialEq, Serialize)]
#[serde(transparent)]
pub struct EncryptedReasoningItem(Value);

impl EncryptedReasoningItem {
    pub(crate) fn from_value(value: &Value) -> Option<Self> {
        (value.get("type").and_then(Value::as_str) == Some("reasoning")
            && value
                .get("encrypted_content")
                .and_then(Value::as_str)
                .is_some_and(|value| !value.is_empty()))
        .then(|| Self(value.clone()))
    }

    pub(crate) fn as_value(&self) -> &Value {
        &self.0
    }
}

impl std::fmt::Debug for EncryptedReasoningItem {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("EncryptedReasoningItem([redacted])")
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ModelContentPart {
    Text {
        text: String,
    },
    Image {
        mime_type: String,
        #[serde(default)]
        sha256: String,
        /// Transient transport data. Durable requests retain only the hash and
        /// dimensions so image bytes cannot leak into continuation JSON.
        #[serde(default, skip_serializing)]
        data: String,
        width: u32,
        height: u32,
    },
}

impl PartialEq for ModelContentPart {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Text { text: left }, Self::Text { text: right }) => left == right,
            (
                Self::Image {
                    mime_type: left_mime,
                    sha256: left_hash,
                    data: left_data,
                    width: left_width,
                    height: left_height,
                },
                Self::Image {
                    mime_type: right_mime,
                    sha256: right_hash,
                    data: right_data,
                    width: right_width,
                    height: right_height,
                },
            ) => {
                left_mime == right_mime
                    && left_width == right_width
                    && left_height == right_height
                    && if left_hash.is_empty() || right_hash.is_empty() {
                        left_data == right_data
                    } else {
                        left_hash == right_hash
                    }
            }
            _ => false,
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    /// Whether the model may continue its response while this client-side tool
    /// is running. Providers that do not support async tools ignore this flag.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub asynchronous: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StablePromptPart {
    pub identity: String,
    pub content: String,
}

/// Provider-neutral request for a schema-constrained final assistant result.
/// Providers choose their native wire representation; callers provide only the
/// JSON Schema document and never a provider-specific response-format wrapper.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct StructuredOutputRequest {
    pub schema: Value,
}

/// Ephemeral provider transport state. The selected adapter translates it
/// into its native continuation fields. It is included in explicit context
/// exports, while durable requests normally leave it unset.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResponseTransportContinuation {
    pub conversation_id: crate::ConversationId,
    pub response_id: String,
    /// Index in the complete normalized input at which the incremental suffix
    /// begins. Adapters may further normalize that suffix for their protocol.
    pub input_suffix_start: usize,
}

/// Best-effort control channel for a currently streaming provider response.
/// Sending only indicates local delivery; provider acceptance is reported by
/// `SteerAccepted` and failures leave the durable boundary queue intact.
#[derive(Clone, Debug)]
pub struct ResponseSteeringHandle {
    sender: tokio::sync::mpsc::UnboundedSender<String>,
}

impl ResponseSteeringHandle {
    #[must_use]
    pub fn send(&self, input: impl Into<String>) -> bool {
        self.sender.send(input.into()).is_ok()
    }
}

impl PartialEq for ResponseSteeringHandle {
    fn eq(&self, other: &Self) -> bool {
        self.sender.same_channel(&other.sender)
    }
}

pub(crate) fn response_steering_channel() -> (
    ResponseSteeringHandle,
    tokio::sync::mpsc::UnboundedReceiver<String>,
) {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    (ResponseSteeringHandle { sender }, receiver)
}

impl StructuredOutputRequest {
    /// Validates a JSON Schema document before a provider request is started.
    pub fn validate_schema(schema: &Value) -> Result<(), String> {
        jsonschema::validator_for(schema)
            .map(|_| ())
            .map_err(|error| format!("invalid output schema: {error}"))
    }

    /// Validates one provider result against this request's schema.
    pub fn validate_value(&self, value: &Value) -> Result<(), String> {
        let validator = jsonschema::validator_for(&self.schema)
            .map_err(|error| format!("invalid output schema: {error}"))?;
        validator.iter_errors(value).next().map_or(Ok(()), |error| {
            Err(format!("structured output does not match schema: {error}"))
        })
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ModelRequest {
    pub request_id: RequestId,
    pub attempt_id: AttemptId,
    pub model: ModelRef,
    #[serde(default)]
    pub backend: Option<ModelBackend>,
    /// Supported wire backends in provider preference order.
    #[serde(default)]
    pub backend_candidates: Vec<ModelBackend>,
    pub effort: Option<String>,
    /// Provider request service tier, such as OpenAI's `priority` tier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_tier: Option<String>,
    pub input: Vec<ModelInput>,
    pub tools: Vec<ToolDefinition>,
    pub stable_prompt: Vec<StablePromptPart>,
    /// Provider-neutral cache identity. Concrete adapters decide whether and
    /// how to translate it into native cache controls.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_cache: Option<PromptCacheRequest>,
    /// Ephemeral response continuation. The complete normalized `input` is
    /// always retained so a provider can fall back to a stateless request.
    #[serde(skip)]
    pub response_transport_continuation: Option<ResponseTransportContinuation>,
    pub allow_parallel_tools: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_output: Option<StructuredOutputRequest>,
}

/// Stable identity supplied to a provider adapter's prompt-cache policy.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct PromptCacheRequest {
    pub key: String,
    pub scope: PromptCacheScope,
}

/// Which portion of a logical request owns a prompt-cache identity.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PromptCacheScope {
    StablePrefix,
    Conversation,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    #[default]
    Stop,
    ToolCalls,
    Length,
    ContentFilter,
    Cancelled,
    Other,
}

/// Normalized token accounting. Missing provider fields remain `None`, never zero.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelUsage {
    pub input_tokens: Option<u64>,
    pub non_cached_input_tokens: Option<u64>,
    pub cache_read_input_tokens: Option<u64>,
    pub cache_write_input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub reasoning_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
    #[serde(default)]
    pub provider_usage: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<ModelCost>,
}

impl ModelUsage {
    /// Restores the provider-neutral accounting invariant after a partial
    /// usage update. Cache writes are part of non-cached input, not an extra
    /// class of input tokens.
    pub(crate) fn normalize(&mut self) {
        self.non_cached_input_tokens = self
            .input_tokens
            .zip(self.cache_read_input_tokens)
            .map(|(input, read)| input.saturating_sub(read))
            .or(self.input_tokens);
        if self.total_tokens.is_none() {
            self.total_tokens = match (self.input_tokens, self.output_tokens) {
                (None, None) => None,
                (input, output) => Some(
                    input
                        .unwrap_or_default()
                        .saturating_add(output.unwrap_or_default()),
                ),
            };
        }
    }

    /// Merges a cumulative or partial provider usage fragment without
    /// clearing fields omitted by later stream events.
    pub(crate) fn merge_fragment(&mut self, fragment: Self) {
        let refresh_derived_total = fragment.total_tokens.is_none()
            && (fragment.input_tokens.is_some() || fragment.output_tokens.is_some());
        macro_rules! replace_some {
            ($field:ident) => {
                if fragment.$field.is_some() {
                    self.$field = fragment.$field;
                }
            };
        }
        replace_some!(input_tokens);
        replace_some!(cache_read_input_tokens);
        replace_some!(cache_write_input_tokens);
        replace_some!(output_tokens);
        replace_some!(reasoning_tokens);
        replace_some!(total_tokens);
        if !fragment.provider_usage.is_null() {
            merge_usage_json(&mut self.provider_usage, fragment.provider_usage);
        }
        if fragment.cost.is_some() {
            self.cost = fragment.cost;
        }
        if refresh_derived_total {
            self.total_tokens = None;
        }
        self.normalize();
    }
}

fn merge_usage_json(target: &mut Value, fragment: Value) {
    match (target, fragment) {
        (Value::Object(target), Value::Object(fragment)) => {
            for (key, value) in fragment {
                merge_usage_json(target.entry(key).or_insert(Value::Null), value);
            }
        }
        (target, fragment) => *target = fragment,
    }
}

/// Fixed-precision cost strings and immutable pricing provenance for one response.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct ModelCost {
    pub input_cost: Option<String>,
    pub cache_read_cost: Option<String>,
    pub cache_write_cost: Option<String>,
    pub output_cost: Option<String>,
    pub reasoning_cost: Option<String>,
    pub total_cost: Option<String>,
    pub currency: String,
    pub pricing_source: String,
    pub pricing_version: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct ResponseMetadata {
    pub provider_request_id: Option<String>,
    pub finish_reason: FinishReason,
    pub usage: ModelUsage,
}

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ProviderStreamEvent {
    /// A retained Responses WebSocket is ready to accept mid-turn steering.
    #[serde(skip)]
    Steerable {
        response_id: String,
        handle: ResponseSteeringHandle,
    },
    /// The server accepted a steering input for ordered processing.
    #[serde(skip)]
    SteerAccepted {
        input: String,
    },
    /// The server rejected a steering input; runtime durability fallback
    /// should keep the corresponding queued input for a normal boundary.
    #[serde(skip)]
    SteerFailed {
        input: String,
        code: String,
        message: String,
    },
    /// The provider exposed a reasoning/thinking segment in its stream.
    /// Providers that keep reasoning private may never emit this event.
    ReasoningStarted,
    /// Private provider state, not a display event. A final response snapshot
    /// replaces incremental items to preserve provider order without duplicates.
    EncryptedReasoning {
        items: Vec<EncryptedReasoningItem>,
        replace: bool,
    },
    TextDelta {
        delta: String,
    },
    ToolCallStarted {
        id: String,
        name: String,
        request_index: u64,
    },
    ToolArgumentsDelta {
        id: String,
        delta: String,
    },
    /// Opaque provider data attached to one tool call. The runtime persists it
    /// with that call and gives it back to the same provider on later rounds.
    ToolCallMetadata {
        id: String,
        metadata: Value,
    },
    Completed {
        metadata: ResponseMetadata,
    },
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderErrorKind {
    Authentication,
    Configuration,
    Connection,
    Timeout,
    RateLimit,
    Server,
    InvalidRequest,
    Protocol,
    Cancelled,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize, thiserror::Error)]
#[error("{message}")]
pub struct ProviderError {
    pub kind: ProviderErrorKind,
    pub code: String,
    pub message: String,
    pub retryable: bool,
    pub retry_after_millis: Option<u64>,
    pub status: Option<u16>,
    #[serde(default)]
    pub metadata: BTreeMap<String, String>,
}

impl ProviderError {
    #[must_use]
    pub fn configuration(message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Configuration,
            code: "configuration".into(),
            message: message.into(),
            retryable: false,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn cancelled() -> Self {
        Self {
            kind: ProviderErrorKind::Cancelled,
            code: "cancelled".into(),
            message: "provider request was cancelled".into(),
            retryable: false,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn protocol(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Protocol,
            code: code.into(),
            message: message.into(),
            retryable: false,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn connection(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Connection,
            code: code.into(),
            message: message.into(),
            retryable: true,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn timeout(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            kind: ProviderErrorKind::Timeout,
            code: code.into(),
            message: message.into(),
            retryable: true,
            retry_after_millis: None,
            status: None,
            metadata: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        self.retry_after_millis.map(Duration::from_millis)
    }
}

/// UI-independent contract implemented by all model providers.
pub trait Provider: Send + Sync {
    fn descriptor(&self) -> &ProviderDescriptor;
    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>>;
    fn begin_auth(
        &self,
        _flow: AuthFlow,
    ) -> ProviderFuture<'_, Result<AuthChallenge, ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not support managed authentication",
            ))
        })
    }
    fn begin_auth_with_callback(
        &self,
        flow: AuthFlow,
        _callback_url: Option<String>,
    ) -> ProviderFuture<'_, Result<AuthChallenge, ProviderError>> {
        self.begin_auth(flow)
    }
    fn complete_auth(
        &self,
        _response: AuthResponse,
    ) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not support managed authentication",
            ))
        })
    }
    fn disconnect(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not manage credentials",
            ))
        })
    }
    fn set_api_key(&self, _key: String) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not manage API keys",
            ))
        })
    }
    fn remove_api_key(&self) -> ProviderFuture<'_, Result<(), ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not manage API keys",
            ))
        })
    }
    fn has_managed_api_key(&self) -> ProviderFuture<'_, bool> {
        Box::pin(async { false })
    }
    fn subscription_plan(&self) -> ProviderFuture<'_, Option<String>> {
        Box::pin(async { None })
    }
    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        None
    }
    /// Returns a provider-reported account usage snapshot when supported.
    ///
    /// `None` means the provider has no reporting capability and lets the
    /// runtime avoid issuing any request for unsupported providers.
    fn provider_usage(
        &self,
    ) -> Option<ProviderFuture<'_, Result<ProviderUsageReport, ProviderError>>> {
        None
    }
    /// Whether this provider currently has credentials suitable for its
    /// provider-owned web-search endpoint.
    fn web_search_ready(&self) -> bool {
        false
    }
    /// Executes a provider-owned web search when one is available.
    fn web_search(
        &self,
        _request: ProviderWebSearchRequest,
        _cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderWebSearchResponse, ProviderError>> {
        Box::pin(async {
            Err(ProviderError::configuration(
                "provider does not support provider-owned web search",
            ))
        })
    }
    fn model_backends(&self, model: &ModelDescriptor) -> Vec<ModelBackend> {
        model
            .backend
            .or(self.descriptor().default_model_backend)
            .into_iter()
            .collect()
    }
    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>>;
    fn refresh_credentials(
        &self,
        _cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<bool, ProviderError>> {
        Box::pin(async { Ok(false) })
    }
}

#[cfg(test)]
mod tests {
    use super::{AuthFlow, ModelContentPart, StructuredOutputRequest, preferred_auth_flow};
    use serde_json::json;

    #[test]
    fn preferred_auth_flow_favors_browser_and_handles_no_supported_flow() {
        assert_eq!(
            preferred_auth_flow(&[AuthFlow::DeviceCode, AuthFlow::BrowserPkce]),
            Some(AuthFlow::BrowserPkce)
        );
        assert_eq!(
            preferred_auth_flow(&[AuthFlow::DeviceCode]),
            Some(AuthFlow::DeviceCode)
        );
        assert_eq!(preferred_auth_flow(&[]), None);
    }

    #[test]
    fn structured_output_validates_schema_and_result_locally() {
        let schema = json!({
            "type": "object",
            "properties": {"answer": {"type": "string"}},
            "required": ["answer"],
            "additionalProperties": false
        });
        StructuredOutputRequest::validate_schema(&schema).unwrap();
        let request = StructuredOutputRequest { schema };
        assert!(request.validate_value(&json!({"answer": "ok"})).is_ok());
        assert!(request.validate_value(&json!({"answer": 7})).is_err());
        assert!(request.validate_value(&json!({})).is_err());
    }

    #[test]
    fn durable_image_parts_omit_transport_payload_but_keep_identity() {
        let part = ModelContentPart::Image {
            mime_type: "image/png".into(),
            sha256: "abc123".into(),
            data: "secret-base64-payload".into(),
            width: 640,
            height: 480,
        };
        let encoded = serde_json::to_string(&part).unwrap();
        assert!(!encoded.contains("secret-base64-payload"));
        assert!(encoded.contains("abc123"));
        let decoded: ModelContentPart = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, part);
    }
}
