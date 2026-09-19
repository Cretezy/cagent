use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use crate::{ModelRequest, ProviderError, ProviderStreamEvent};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum PromptCachePolicy {
    Disabled,
    OneHourRolling,
}

pub(crate) fn encode(
    request: &ModelRequest,
    cache: PromptCachePolicy,
) -> Result<serde_json::Value, ProviderError> {
    crate::provider::adapters::anthropic::request_body_with_policy(request, cache)
}

pub(crate) async fn decode_http_error(response: reqwest::Response) -> ProviderError {
    crate::provider::adapters::anthropic::http_error(response).await
}

pub(crate) async fn decode_stream(
    response: reqwest::Response,
    sender: mpsc::Sender<Result<ProviderStreamEvent, ProviderError>>,
    cancellation: CancellationToken,
) {
    crate::provider::adapters::anthropic::map_sse(response, sender, cancellation).await;
}
