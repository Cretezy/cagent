use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{ModelRequest, ProviderError, ProviderStreamEvent};

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum PromptCachePolicy {
    Disabled,
    ChatGptCompatible {
        key: String,
    },
    Automatic {
        key: String,
        retention: Option<&'static str>,
    },
    Explicit {
        key: String,
        ttl: &'static str,
    },
}

pub(crate) fn encode(
    request: &ModelRequest,
    cache: &PromptCachePolicy,
) -> Result<serde_json::Value, ProviderError> {
    crate::provider::adapters::openai::request_body_with_policy(request, cache)
}

pub(crate) fn encode_responses_lite(
    request: &ModelRequest,
    cache: &PromptCachePolicy,
) -> Result<serde_json::Value, ProviderError> {
    crate::provider::adapters::openai::request_body_with_policy_and_protocol(request, cache, true)
}

pub(crate) async fn decode_http_error(response: reqwest::Response) -> ProviderError {
    crate::provider::adapters::openai::http_error(response).await
}

pub(crate) async fn decode_stream(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
) {
    crate::provider::adapters::openai::map_sse(response, sender, cancellation).await;
}
