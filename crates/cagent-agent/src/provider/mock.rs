#![allow(clippy::type_complexity)] // The mock stores complete stream fixtures in their test-facing form.

#[allow(clippy::wildcard_imports)]
use super::*;

#[derive(Debug, Default)]
pub(crate) struct MockProvider;

impl Provider for MockProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        static DESCRIPTOR: std::sync::LazyLock<ProviderDescriptor> =
            std::sync::LazyLock::new(|| ProviderDescriptor {
                id: "mock".into(),
                display_name: "Mock".into(),
                default_model_backend: None,
                supported_model_backends: Vec::new(),
                model_discovery: ModelDiscoverySource::ProviderApi,
                credential_source: CredentialSource::Unauthenticated,
                credential_environment_variable: None,
                supports_managed_api_key: false,
                auth_flows: Vec::new(),
            });
        &DESCRIPTOR
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async {
            Ok(AuthState::Connected {
                detail: "built in".into(),
            })
        })
    }

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        Some(Box::pin(async {
            Ok(ModelCatalog {
                provider: "mock".into(),
                models: vec![
                    mock_model("echo-fast", "Echo fast"),
                    mock_model("echo-slow", "Echo slow"),
                ],
                version: None,
            })
        }))
    }

    fn stream(
        &self,
        request: ModelRequest,
        _cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        let prompt = request
            .input
            .iter()
            .rev()
            .find_map(|input| match input {
                ModelInput::Message {
                    role: MessageRole::User,
                    content,
                } => Some(content.as_str()),
                _ => None,
            })
            .unwrap_or_default();
        let response = format!("Mock response: {prompt}");
        let mut events = fragment(&response, 4)
            .into_iter()
            .map(|delta| Ok(ProviderStreamEvent::TextDelta { delta }))
            .collect::<Vec<_>>();
        events.push(Ok(ProviderStreamEvent::Completed {
            metadata: ResponseMetadata {
                provider_request_id: None,
                finish_reason: FinishReason::Stop,
                usage: ModelUsage::default(),
            },
        }));
        let response_delay = mock_response_delay(&request.model);
        let stream = futures_util::stream::unfold(
            (events.into_iter(), response_delay),
            |(mut events, delay)| async move {
                let event = events.next()?;
                tokio::time::sleep(delay).await;
                Some((event, (events, MOCK_STREAM_INTERVAL)))
            },
        );
        Box::pin(async { Ok(Box::pin(stream) as ProviderStream) })
    }
}

fn mock_model(id: &str, display_name: &str) -> ModelDescriptor {
    ModelDescriptor {
        id: id.into(),
        display_name: display_name.into(),
        capabilities: ModelCapabilities {
            supports_streaming: Some(true),
            supports_tools: Some(true),
            supports_structured_output: Some(true),
            supports_text_input: Some(true),
            supports_text_output: Some(true),
            ..ModelCapabilities::default()
        },
        backend: None,
        raw_metadata: Value::Null,
    }
}

fn mock_response_delay(model: &ModelRef) -> Duration {
    match model.model.as_str() {
        "echo-fast" => MOCK_FAST_RESPONSE_DELAY,
        "echo-slow" => MOCK_SLOW_RESPONSE_DELAY,
        _ => MOCK_RESPONSE_DELAY,
    }
}

pub(crate) fn fragment(value: &str, characters: usize) -> Vec<String> {
    assert!(characters > 0, "fragment size must be non-zero");
    let mut fragments = Vec::new();
    let mut current = String::new();
    for character in value.chars() {
        current.push(character);
        if current.chars().count() == characters {
            fragments.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        fragments.push(current);
    }
    fragments
}

#[cfg(test)]
pub(crate) struct ScriptedMockProvider {
    descriptor: ProviderDescriptor,
    auth: AuthState,
    streams: std::sync::Mutex<
        std::collections::VecDeque<Vec<Result<ProviderStreamEvent, ProviderError>>>,
    >,
    requests: std::sync::Mutex<Vec<ModelRequest>>,
    refresh_results: std::sync::Mutex<std::collections::VecDeque<Result<bool, ProviderError>>>,
    refresh_count: std::sync::atomic::AtomicUsize,
    usage_results: std::sync::Mutex<
        Option<std::collections::VecDeque<(Duration, Result<ProviderUsageReport, ProviderError>)>>,
    >,
    usage_count: std::sync::atomic::AtomicUsize,
    wait_for_stream_setup_cancellation: bool,
    wait_for_title_cancellation: bool,
    scripted_title_responses: bool,
    title_cancellation_count: std::sync::atomic::AtomicUsize,
    title_response_delay: Duration,
}

#[cfg(test)]
impl ScriptedMockProvider {
    pub(crate) fn new(events: Vec<ProviderStreamEvent>) -> Self {
        Self {
            descriptor: MockProvider.descriptor().clone(),
            auth: AuthState::Connected {
                detail: "mock credentials".into(),
            },
            streams: std::sync::Mutex::new(std::collections::VecDeque::from([events
                .into_iter()
                .map(Ok)
                .collect()])),
            requests: std::sync::Mutex::new(Vec::new()),
            refresh_results: std::sync::Mutex::new(std::collections::VecDeque::new()),
            refresh_count: std::sync::atomic::AtomicUsize::new(0),
            usage_results: std::sync::Mutex::new(None),
            usage_count: std::sync::atomic::AtomicUsize::new(0),
            wait_for_stream_setup_cancellation: false,
            wait_for_title_cancellation: false,
            scripted_title_responses: false,
            title_cancellation_count: std::sync::atomic::AtomicUsize::new(0),
            title_response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn sequence(streams: Vec<Vec<ProviderStreamEvent>>) -> Self {
        Self {
            descriptor: MockProvider.descriptor().clone(),
            auth: AuthState::Connected {
                detail: "mock credentials".into(),
            },
            streams: std::sync::Mutex::new(
                streams
                    .into_iter()
                    .map(|events| events.into_iter().map(Ok).collect())
                    .collect(),
            ),
            requests: std::sync::Mutex::new(Vec::new()),
            refresh_results: std::sync::Mutex::new(std::collections::VecDeque::new()),
            refresh_count: std::sync::atomic::AtomicUsize::new(0),
            usage_results: std::sync::Mutex::new(None),
            usage_count: std::sync::atomic::AtomicUsize::new(0),
            wait_for_stream_setup_cancellation: false,
            wait_for_title_cancellation: false,
            scripted_title_responses: false,
            title_cancellation_count: std::sync::atomic::AtomicUsize::new(0),
            title_response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn sequence_results(
        streams: Vec<Vec<Result<ProviderStreamEvent, ProviderError>>>,
    ) -> Self {
        Self {
            descriptor: MockProvider.descriptor().clone(),
            auth: AuthState::Connected {
                detail: "mock credentials".into(),
            },
            streams: std::sync::Mutex::new(streams.into_iter().collect()),
            requests: std::sync::Mutex::new(Vec::new()),
            refresh_results: std::sync::Mutex::new(std::collections::VecDeque::new()),
            refresh_count: std::sync::atomic::AtomicUsize::new(0),
            usage_results: std::sync::Mutex::new(None),
            usage_count: std::sync::atomic::AtomicUsize::new(0),
            wait_for_stream_setup_cancellation: false,
            wait_for_title_cancellation: false,
            scripted_title_responses: false,
            title_cancellation_count: std::sync::atomic::AtomicUsize::new(0),
            title_response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn with_error(events: Vec<ProviderStreamEvent>, error: ProviderError) -> Self {
        let mut events = events.into_iter().map(Ok).collect::<Vec<_>>();
        events.push(Err(error));
        Self {
            descriptor: MockProvider.descriptor().clone(),
            auth: AuthState::Connected {
                detail: "mock credentials".into(),
            },
            streams: std::sync::Mutex::new(std::collections::VecDeque::from([events])),
            requests: std::sync::Mutex::new(Vec::new()),
            refresh_results: std::sync::Mutex::new(std::collections::VecDeque::new()),
            refresh_count: std::sync::atomic::AtomicUsize::new(0),
            usage_results: std::sync::Mutex::new(None),
            usage_count: std::sync::atomic::AtomicUsize::new(0),
            wait_for_stream_setup_cancellation: false,
            wait_for_title_cancellation: false,
            scripted_title_responses: false,
            title_cancellation_count: std::sync::atomic::AtomicUsize::new(0),
            title_response_delay: Duration::ZERO,
        }
    }

    pub(crate) fn requests(&self) -> Vec<ModelRequest> {
        self.requests.lock().unwrap().clone()
    }

    pub(crate) fn with_provider_id(mut self, id: impl Into<String>) -> Self {
        let id = id.into();
        self.descriptor.id.clone_from(&id);
        self.descriptor.display_name = id;
        // Scripted providers do not implement API model discovery. Treat a
        // renamed scripted provider as metadata-backed so test configurations
        // can supply its available models explicitly.
        self.descriptor.model_discovery = ModelDiscoverySource::ModelsDev;
        self
    }

    pub(crate) fn with_auth_state(mut self, auth: AuthState) -> Self {
        self.auth = auth;
        self
    }

    pub(crate) fn with_credential_refresh(self, results: Vec<Result<bool, ProviderError>>) -> Self {
        *self.refresh_results.lock().unwrap() = results.into_iter().collect();
        self
    }

    pub(crate) fn with_provider_usage(
        self,
        results: Vec<(Duration, Result<ProviderUsageReport, ProviderError>)>,
    ) -> Self {
        *self.usage_results.lock().unwrap() = Some(results.into_iter().collect());
        self
    }

    pub(crate) fn usage_count(&self) -> usize {
        self.usage_count.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn wait_for_stream_setup_cancellation(mut self) -> Self {
        self.wait_for_stream_setup_cancellation = true;
        self
    }

    pub(crate) fn with_title_response_delay(mut self, delay: Duration) -> Self {
        self.title_response_delay = delay;
        self
    }

    pub(crate) fn wait_for_title_cancellation(mut self) -> Self {
        self.wait_for_title_cancellation = true;
        self
    }

    pub(crate) fn with_scripted_title_responses(mut self) -> Self {
        self.scripted_title_responses = true;
        self
    }

    pub(crate) fn title_cancellation_count(&self) -> usize {
        self.title_cancellation_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    pub(crate) fn refresh_count(&self) -> usize {
        self.refresh_count
            .load(std::sync::atomic::Ordering::Relaxed)
    }
}

#[cfg(test)]
impl Provider for ScriptedMockProvider {
    fn descriptor(&self) -> &ProviderDescriptor {
        &self.descriptor
    }

    fn auth_state(&self) -> ProviderFuture<'_, Result<AuthState, ProviderError>> {
        Box::pin(async { Ok(self.auth.clone()) })
    }

    fn discover_models(&self) -> Option<ProviderFuture<'_, Result<ModelCatalog, ProviderError>>> {
        let provider = self.descriptor.id.clone();
        Some(Box::pin(async move {
            let mut catalog = MockProvider
                .discover_models()
                .expect("mock discovery is available")
                .await?;
            catalog.provider = provider;
            Ok(catalog)
        }))
    }

    fn provider_usage(
        &self,
    ) -> Option<ProviderFuture<'_, Result<ProviderUsageReport, ProviderError>>> {
        let result = self.usage_results.lock().unwrap().as_mut()?.pop_front();
        self.usage_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Some(Box::pin(async move {
            let (delay, result) = result.unwrap_or_else(|| {
                (
                    Duration::ZERO,
                    Err(ProviderError::protocol(
                        "usage_script_exhausted",
                        "scripted provider has no remaining usage response",
                    )),
                )
            });
            tokio::time::sleep(delay).await;
            result
        }))
    }

    fn stream(
        &self,
        request: ModelRequest,
        cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<ProviderStream, ProviderError>> {
        let title_request = request
            .stable_prompt
            .iter()
            .any(|part| part.identity == "cagent:conversation-title:shared");
        self.requests.lock().unwrap().push(request);
        if title_request && !self.scripted_title_responses {
            if self.wait_for_title_cancellation {
                let cancellation_count = &self.title_cancellation_count;
                return Box::pin(async move {
                    cancellation.cancelled().await;
                    cancellation_count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    Err(ProviderError::cancelled())
                });
            }
            let events = vec![
                Ok(ProviderStreamEvent::TextDelta {
                    delta: r#"{"title":"Generated test title"}"#.into(),
                }),
                Ok(ProviderStreamEvent::Completed {
                    metadata: ResponseMetadata {
                        provider_request_id: None,
                        finish_reason: FinishReason::Stop,
                        usage: ModelUsage::default(),
                    },
                }),
            ];
            let stream = futures_util::stream::unfold(
                (events.into_iter(), self.title_response_delay),
                |(mut events, delay)| async move {
                    let event = events.next()?;
                    tokio::time::sleep(delay).await;
                    Some((event, (events, Duration::ZERO)))
                },
            );
            return Box::pin(async { Ok(Box::pin(stream) as ProviderStream) });
        }
        if self.wait_for_stream_setup_cancellation {
            return Box::pin(async move {
                tokio::select! {
                    () = cancellation.cancelled() => Err(ProviderError::cancelled()),
                    () = std::future::pending::<()>() => unreachable!(),
                }
            });
        }
        let events = self.streams.lock().unwrap().pop_front().unwrap_or_else(|| {
            vec![Err(ProviderError::protocol(
                "script_exhausted",
                "scripted provider has no remaining stream",
            ))]
        });
        Box::pin(async { Ok(Box::pin(tokio_stream::iter(events)) as ProviderStream) })
    }

    fn refresh_credentials(
        &self,
        _cancellation: CancellationToken,
    ) -> ProviderFuture<'_, Result<bool, ProviderError>> {
        self.refresh_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let result = self
            .refresh_results
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false));
        Box::pin(async move { result })
    }
}

#[cfg(test)]
mod tests {
    use tokio_stream::StreamExt;

    use super::*;

    fn request(model: &str) -> ModelRequest {
        ModelRequest {
            request_id: RequestId::new(),
            attempt_id: AttemptId::new(),
            model: ModelRef::parse(&format!("mock/{model}")).unwrap(),
            backend: None,
            backend_candidates: Vec::new(),
            effort: None,
            service_tier: None,
            input: Vec::new(),
            tools: Vec::new(),
            stable_prompt: Vec::new(),
            prompt_cache: None,
            response_transport_continuation: None,
            allow_parallel_tools: false,
            structured_output: None,
        }
    }

    #[test]
    fn model_reference_splits_only_on_the_first_slash() {
        let model = ModelRef::parse("openrouter/openai/gpt-example").unwrap();
        assert_eq!(model.provider, "openrouter");
        assert_eq!(model.model, "openai/gpt-example");
        assert_eq!(model.to_string(), "openrouter/openai/gpt-example");
        assert!(ModelRef::parse("missing-provider").is_err());
        assert!(ModelRef::parse("/missing-provider").is_err());
    }

    #[test]
    fn usage_preserves_unavailable_values_as_null() {
        let encoded = serde_json::to_value(ModelUsage::default()).unwrap();
        assert!(encoded.get("input_tokens").unwrap().is_null());
        assert!(encoded.get("cache_read_input_tokens").unwrap().is_null());
        assert!(encoded.get("reasoning_tokens").unwrap().is_null());
    }

    #[test]
    fn available_key_does_not_imply_provider_enablement() {
        let availability = ProviderAvailability {
            descriptor: MockProvider.descriptor().clone(),
            enabled: false,
            auth: AuthState::Available {
                detail: "key found".into(),
            },
            has_managed_api_key: false,
        };
        assert_eq!(availability.status_text(), "disabled");
        assert_eq!(availability.setup_instructions(), None);
    }

    #[test]
    fn missing_credentials_are_projected_as_setup_instructions() {
        let availability = ProviderAvailability {
            descriptor: ProviderDescriptor {
                credential_source: CredentialSource::Environment,
                credential_environment_variable: Some("OPENAI_API_KEY".into()),
                ..MockProvider.descriptor().clone()
            },
            enabled: false,
            auth: AuthState::Missing,
            has_managed_api_key: false,
        };
        assert_eq!(availability.status_text(), "not setup");
        assert_eq!(
            availability.setup_instructions().as_deref(),
            Some("Set OPENAI_API_KEY in the environment, then restart Cagent.")
        );
    }

    #[test]
    fn fragments_on_character_boundaries() {
        assert_eq!(fragment("aéå🦀z", 2), ["aé", "å🦀", "z"]);
    }

    #[tokio::test]
    async fn mock_lists_fast_and_slow_echo_models() {
        let catalog = MockProvider.discover_models().unwrap().await.unwrap();
        let models = catalog
            .models
            .iter()
            .map(|model| (model.id.as_str(), model.display_name.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            models,
            [("echo-fast", "Echo fast"), ("echo-slow", "Echo slow")]
        );
    }

    #[test]
    fn mock_echo_models_select_delays_around_the_legacy_timing() {
        let fast = ModelRef::parse("mock/echo-fast").unwrap();
        let slow = ModelRef::parse("mock/echo-slow").unwrap();
        let legacy = ModelRef::parse("mock/echo").unwrap();

        assert_eq!(mock_response_delay(&fast), Duration::from_millis(800));
        assert_eq!(mock_response_delay(&legacy), Duration::from_secs(1));
        assert_eq!(mock_response_delay(&slow), Duration::from_millis(1_200));
    }

    #[tokio::test]
    async fn mock_waits_before_responding_and_paces_stream_events() {
        let provider = MockProvider;
        let mut stream = provider
            .stream(request("echo-fast"), CancellationToken::new())
            .await
            .unwrap();

        assert!(
            tokio::time::timeout(Duration::from_millis(700), stream.next())
                .await
                .is_err(),
            "the first response event arrived before the simulated lag elapsed"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), stream.next())
                .await
                .unwrap()
                .is_some()
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(20), stream.next())
                .await
                .is_err(),
            "consecutive stream events were not paced"
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(75), stream.next())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn scripted_mock_fragments_tool_arguments_exactly() {
        let provider = ScriptedMockProvider::new(vec![
            ProviderStreamEvent::ToolCallStarted {
                id: "call-1".into(),
                name: "read".into(),
                request_index: 0,
            },
            ProviderStreamEvent::ToolArgumentsDelta {
                id: "call-1".into(),
                delta: "{\"pa".into(),
            },
            ProviderStreamEvent::ToolArgumentsDelta {
                id: "call-1".into(),
                delta: "th\":\"SPEC.md\"}".into(),
            },
            ProviderStreamEvent::Completed {
                metadata: ResponseMetadata {
                    provider_request_id: Some("response-1".into()),
                    finish_reason: FinishReason::ToolCalls,
                    usage: ModelUsage::default(),
                },
            },
        ]);
        let stream = provider
            .stream(request("echo"), CancellationToken::new())
            .await
            .unwrap();
        let events = stream.collect::<Vec<_>>().await;
        let arguments = events
            .iter()
            .filter_map(|event| match event.as_ref().unwrap() {
                ProviderStreamEvent::ToolArgumentsDelta { delta, .. } => Some(delta.as_str()),
                _ => None,
            })
            .collect::<String>();
        assert_eq!(arguments, r#"{"path":"SPEC.md"}"#);
    }
}
