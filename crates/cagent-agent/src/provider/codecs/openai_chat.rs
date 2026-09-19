use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{ModelRequest, ProviderError, ProviderStreamEvent};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptCachePolicy {
    Disabled,
    OpenRouterAutomatic {
        session_id: Option<String>,
    },
    OpenRouterClaude {
        session_id: String,
        ttl: &'static str,
    },
    OpenRouterExplicit {
        session_id: String,
        ttl: &'static str,
    },
}

pub(crate) fn encode(
    request: &ModelRequest,
    cache: &PromptCachePolicy,
) -> Result<serde_json::Value, ProviderError> {
    crate::provider::adapters::openai_compatible::request_body_with_policy(request, cache)
}

pub(crate) async fn decode_http_error(response: reqwest::Response) -> ProviderError {
    crate::provider::adapters::openai_compatible::http_error(response).await
}

pub(crate) async fn decode_stream(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
) {
    crate::provider::adapters::openai_compatible::map_sse(response, sender, cancellation).await;
}
