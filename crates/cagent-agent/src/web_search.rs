//! Configurable, provider-neutral web search.
//!
//! Search results are reference material supplied by an external service. The
//! client therefore normalizes and bounds every field before it crosses the
//! tool boundary.

use std::fmt;
use std::time::Duration;

use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;

const DEFAULT_SEARXNG_URL_ENV: &str = "SEARXNG_URL";
const DEFAULT_EXA_KEY_ENV: &str = "EXA_API_KEY";
const EXA_ENDPOINT: &str = "https://api.exa.ai/search";
const DEFAULT_TIMEOUT_SECONDS: u64 = 20;
const MAX_RESULTS: usize = 10;
const MAX_QUERY_LENGTH: usize = 1_024;
const MAX_TITLE_LENGTH: usize = 512;
const MAX_URL_LENGTH: usize = 2_048;
const MAX_SNIPPET_LENGTH: usize = 2_048;
const MAX_PUBLISHED_LENGTH: usize = 128;
const MAX_OUTPUT_BYTES: usize = 32 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WebSearchProvider {
    Searxng,
    Exa,
    Chatgpt,
}

impl WebSearchProvider {
    pub const ALL: [Self; 3] = [Self::Searxng, Self::Exa, Self::Chatgpt];
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Searxng => "searxng",
            Self::Exa => "exa",
            Self::Chatgpt => "chatgpt",
        }
    }

    #[must_use]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Searxng => "SearXNG",
            Self::Exa => "Exa",
            Self::Chatgpt => "ChatGPT",
        }
    }

    #[must_use]
    pub const fn unavailable_label(self) -> &'static str {
        match self {
            Self::Chatgpt => "subscription required",
            Self::Searxng | Self::Exa => "not setup",
        }
    }
}

/// Frontend-safe status for a selectable web-search provider.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct WebSearchProviderStatus {
    pub provider: WebSearchProvider,
    pub active: bool,
    pub ready: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct WebSearchFile {
    pub provider: Option<WebSearchProvider>,
    pub timeout_seconds: Option<u64>,
    pub searxng: Option<SearxngWebSearchFile>,
    pub exa: Option<ExaWebSearchFile>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct SearxngWebSearchFile {
    pub url: Option<String>,
    pub url_env_var: Option<String>,
}

#[derive(Clone, Default, Deserialize, Serialize)]
pub(crate) struct ExaWebSearchFile {
    pub api_key_env_var: Option<String>,
}

impl fmt::Debug for WebSearchFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebSearchFile")
            .field("provider", &self.provider)
            .field("searxng", &self.searxng.as_ref().map(|_| "<configured>"))
            .field("exa", &self.exa.as_ref().map(|_| "<configured>"))
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

/// Raw, validated web-search configuration. Environment values are resolved
/// later so hot reload and environment changes take effect at boundaries.
#[derive(Clone)]
pub struct WebSearchConfig {
    provider: Option<WebSearchProvider>,
    searxng_url: Option<String>,
    url_env_var: String,
    api_key_env_var: String,
    timeout_seconds: u64,
}

impl fmt::Debug for WebSearchConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebSearchConfig")
            .field("provider", &self.provider)
            .field("searxng_url", &self.searxng_url)
            .field("url_env_var", &self.url_env_var)
            .field("api_key_env_var", &self.api_key_env_var)
            .field("timeout_seconds", &self.timeout_seconds)
            .finish()
    }
}

impl Default for WebSearchConfig {
    fn default() -> Self {
        Self {
            provider: None,
            searxng_url: None,
            url_env_var: DEFAULT_SEARXNG_URL_ENV.into(),
            api_key_env_var: DEFAULT_EXA_KEY_ENV.into(),
            timeout_seconds: DEFAULT_TIMEOUT_SECONDS,
        }
    }
}

impl WebSearchConfig {
    pub(crate) fn parse(
        path: &std::path::Path,
        file: Option<WebSearchFile>,
    ) -> Result<Self, crate::RuntimeError> {
        let Some(file) = file else {
            return Ok(Self::default());
        };
        let provider = file.provider;
        let searxng = file.searxng.unwrap_or_default();
        let exa = file.exa.unwrap_or_default();
        let url_env_var = searxng
            .url_env_var
            .unwrap_or_else(|| DEFAULT_SEARXNG_URL_ENV.into());
        let api_key_env_var = exa
            .api_key_env_var
            .unwrap_or_else(|| DEFAULT_EXA_KEY_ENV.into());
        validate_env_name(path, "web_search.url_env_var", &url_env_var)?;
        validate_env_name(path, "web_search.api_key_env_var", &api_key_env_var)?;
        if let Some(timeout) = file.timeout_seconds
            && !(1..=120).contains(&timeout)
        {
            return Err(crate::RuntimeError::Config {
                path: path.to_path_buf(),
                message: "web_search.timeout_seconds must be between 1 and 120".into(),
            });
        }
        if let Some(provider) = provider {
            match provider {
                WebSearchProvider::Searxng => {
                    if let Some(url) = &searxng.url {
                        validate_http_url(path, "web_search.url", url)?;
                    }
                }
                WebSearchProvider::Exa => {}
                WebSearchProvider::Chatgpt => {}
            }
        }
        Ok(Self {
            provider,
            searxng_url: searxng.url,
            url_env_var,
            api_key_env_var,
            timeout_seconds: file.timeout_seconds.unwrap_or(DEFAULT_TIMEOUT_SECONDS),
        })
    }

    #[must_use]
    pub fn provider(&self) -> Option<WebSearchProvider> {
        self.provider
    }

    #[must_use]
    pub fn provider_statuses(&self) -> Vec<WebSearchProviderStatus> {
        let mut statuses = WebSearchProvider::ALL
            .into_iter()
            .map(|provider| WebSearchProviderStatus {
                provider,
                active: self.provider == Some(provider),
                ready: self.resolve_provider(provider).is_some(),
            })
            .collect::<Vec<_>>();
        statuses.sort_by_key(|status| match (status.active, status.ready) {
            (true, _) => 0,
            (false, true) => 1,
            (false, false) => 2,
        });
        statuses
    }

    /// Resolves a complete runtime configuration without exposing credentials.
    pub(crate) fn resolve(&self) -> Option<ResolvedWebSearch> {
        self.resolve_with_managed_exa_key(None)
    }

    pub(crate) fn resolve_with_managed_exa_key(
        &self,
        managed_exa_key: Option<String>,
    ) -> Option<ResolvedWebSearch> {
        let provider = self.provider?;
        if provider == WebSearchProvider::Chatgpt {
            return None;
        }
        if provider == WebSearchProvider::Exa {
            let api_key = managed_exa_key.or_else(|| std::env::var(&self.api_key_env_var).ok())?;
            return (!api_key.trim().is_empty()).then_some(ResolvedWebSearch {
                provider,
                endpoint: EXA_ENDPOINT.into(),
                api_key: Some(api_key),
                timeout: Duration::from_secs(self.timeout_seconds),
            });
        }
        self.resolve_provider(provider)
    }

    pub(crate) fn resolve_provider(
        &self,
        provider: WebSearchProvider,
    ) -> Option<ResolvedWebSearch> {
        match provider {
            WebSearchProvider::Searxng => {
                let url = self
                    .searxng_url
                    .clone()
                    .or_else(|| std::env::var(&self.url_env_var).ok())?;
                let url = validate_runtime_url(&url)?;
                Some(ResolvedWebSearch {
                    provider,
                    endpoint: format!("{}/search", url.trim_end_matches('/')),
                    api_key: None,
                    timeout: Duration::from_secs(self.timeout_seconds),
                })
            }
            WebSearchProvider::Exa => None,
            WebSearchProvider::Chatgpt => None,
        }
    }

    #[must_use]
    pub(crate) fn credential_variables(&self) -> [String; 2] {
        [self.url_env_var.clone(), self.api_key_env_var.clone()]
    }
}

pub(crate) fn validate_configured_url(url: &str) -> Result<(), crate::RuntimeError> {
    validate_http_url(
        std::path::Path::new("config.toml"),
        "web_search.searxng.url",
        url,
    )
}

#[derive(Clone)]
pub(crate) struct ResolvedWebSearch {
    pub provider: WebSearchProvider,
    endpoint: String,
    api_key: Option<String>,
    timeout: Duration,
}

impl fmt::Debug for ResolvedWebSearch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ResolvedWebSearch")
            .field("provider", &self.provider)
            .field("endpoint", &self.endpoint)
            .field("api_key", &self.api_key.as_ref().map(|_| "<redacted>"))
            .field("timeout", &self.timeout)
            .finish()
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebSearchRequest {
    pub query: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebSearchResponse {
    pub provider: WebSearchProvider,
    pub results: Vec<WebSearchResult>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebSearchResult {
    pub title: String,
    pub url: String,
    #[serde(default)]
    pub snippet: String,
    #[serde(default)]
    pub published_at: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum WebSearchError {
    #[error("web search is not configured")]
    NotConfigured,
    #[error("web search query must not be empty")]
    EmptyQuery,
    #[error("web search query is too long")]
    QueryTooLong,
    #[error("web search was cancelled")]
    Cancelled,
    #[error("web search timed out")]
    Timeout,
    #[error("web search provider returned HTTP status {0}")]
    HttpStatus(StatusCode),
    #[error("web search provider returned invalid JSON")]
    InvalidJson,
    #[error("web search provider returned an invalid result")]
    InvalidResult,
    #[error("web search request failed")]
    Request,
    #[error("web search response exceeded the output limit")]
    OutputLimit,
    #[error("ChatGPT web search failed: {0}")]
    Provider(String),
}

pub(crate) async fn search(
    config: &WebSearchConfig,
    request: WebSearchRequest,
    cancellation: &CancellationToken,
) -> Result<WebSearchResponse, WebSearchError> {
    let resolved = config.resolve().ok_or(WebSearchError::NotConfigured)?;
    search_resolved(&resolved, request, cancellation).await
}

pub(crate) async fn search_with_managed_exa_key(
    config: &WebSearchConfig,
    managed_exa_key: Option<String>,
    request: WebSearchRequest,
    cancellation: &CancellationToken,
) -> Result<WebSearchResponse, WebSearchError> {
    let resolved = config
        .resolve_with_managed_exa_key(managed_exa_key)
        .ok_or(WebSearchError::NotConfigured)?;
    search_resolved(&resolved, request, cancellation).await
}

pub(crate) async fn search_with_chatgpt(
    provider: &dyn crate::provider::Provider,
    request: crate::provider::ProviderWebSearchRequest,
    cancellation: &CancellationToken,
) -> Result<WebSearchResponse, WebSearchError> {
    let response = provider
        .web_search(request, cancellation.clone())
        .await
        .map_err(|error| WebSearchError::Provider(error.to_string()))?;
    let results = normalize_codex(&response.results)?;
    let output = WebSearchResponse {
        provider: WebSearchProvider::Chatgpt,
        results,
    };
    let encoded = serde_json::to_vec(&output).map_err(|_| WebSearchError::OutputLimit)?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        return Err(WebSearchError::OutputLimit);
    }
    Ok(output)
}

#[tracing::instrument(level = "trace", name = "agent.web_search", skip_all)]
pub(crate) async fn search_resolved(
    config: &ResolvedWebSearch,
    request: WebSearchRequest,
    cancellation: &CancellationToken,
) -> Result<WebSearchResponse, WebSearchError> {
    let query = validate_query(request.query)?;
    let client = reqwest::Client::builder()
        .timeout(config.timeout)
        .build()
        .map_err(|_| WebSearchError::Request)?;
    let response = match config.provider {
        WebSearchProvider::Searxng => client
            .get(&config.endpoint)
            .query(&[("q", query.as_str()), ("format", "json")]),
        WebSearchProvider::Exa => client
            .post(&config.endpoint)
            .header("x-api-key", config.api_key.as_deref().unwrap_or_default())
            .json(&serde_json::json!({
                "query": query,
                "numResults": MAX_RESULTS,
                "contents": { "text": { "maxCharacters": MAX_SNIPPET_LENGTH } }
            })),
        WebSearchProvider::Chatgpt => unreachable!("ChatGPT searches use the provider adapter"),
    };
    let response = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(WebSearchError::Cancelled),
        response = response.send() => response.map_err(map_request_error)?,
    };
    let status = response.status();
    if !status.is_success() {
        return Err(WebSearchError::HttpStatus(status));
    }
    let body = tokio::select! {
        biased;
        () = cancellation.cancelled() => return Err(WebSearchError::Cancelled),
        body = response.text() => body.map_err(map_request_error)?,
    };
    if body.len() > MAX_OUTPUT_BYTES * 4 {
        return Err(WebSearchError::OutputLimit);
    }
    let mut body = body.into_bytes();
    let value: serde_json::Value =
        simd_json::serde::from_slice(&mut body).map_err(|_| WebSearchError::InvalidJson)?;
    let results = match config.provider {
        WebSearchProvider::Searxng => normalize_searxng(&value),
        WebSearchProvider::Exa => normalize_exa(&value),
        WebSearchProvider::Chatgpt => unreachable!("ChatGPT searches use the provider adapter"),
    }?;
    let output = WebSearchResponse {
        provider: config.provider,
        results,
    };
    let encoded = serde_json::to_vec(&output).map_err(|_| WebSearchError::OutputLimit)?;
    if encoded.len() > MAX_OUTPUT_BYTES {
        return Err(WebSearchError::OutputLimit);
    }
    Ok(output)
}

fn normalize_searxng(value: &serde_json::Value) -> Result<Vec<WebSearchResult>, WebSearchError> {
    normalize_results(value.get("results"), |item| {
        let title = item.get("title").and_then(serde_json::Value::as_str)?;
        let url = item.get("url").and_then(serde_json::Value::as_str)?;
        let snippet = item
            .get("content")
            .or_else(|| item.get("snippet"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Some((
            title,
            url,
            snippet,
            item.get("publishedDate")
                .and_then(serde_json::Value::as_str),
        ))
    })
}

fn normalize_exa(value: &serde_json::Value) -> Result<Vec<WebSearchResult>, WebSearchError> {
    normalize_results(value.get("results"), |item| {
        let title = item.get("title").and_then(serde_json::Value::as_str)?;
        let url = item.get("url").and_then(serde_json::Value::as_str)?;
        let snippet = item
            .get("text")
            .or_else(|| item.get("snippet"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Some((
            title,
            url,
            snippet,
            item.get("publishedDate")
                .and_then(serde_json::Value::as_str),
        ))
    })
}

fn normalize_codex(values: &[serde_json::Value]) -> Result<Vec<WebSearchResult>, WebSearchError> {
    let value = serde_json::Value::Array(values.to_vec());
    normalize_results(Some(&value), |item| {
        let title = item.get("title").and_then(serde_json::Value::as_str)?;
        let url = item.get("url").and_then(serde_json::Value::as_str)?;
        let snippet = item
            .get("snippet")
            .or_else(|| item.get("text"))
            .or_else(|| item.get("content"))
            .and_then(serde_json::Value::as_str)
            .unwrap_or_default();
        Some((
            title,
            url,
            snippet,
            item.get("published_at")
                .or_else(|| item.get("publishedDate"))
                .and_then(serde_json::Value::as_str),
        ))
    })
}

fn normalize_results(
    value: Option<&serde_json::Value>,
    mut read: impl FnMut(&serde_json::Value) -> Option<(&str, &str, &str, Option<&str>)>,
) -> Result<Vec<WebSearchResult>, WebSearchError> {
    let array = value
        .and_then(serde_json::Value::as_array)
        .ok_or(WebSearchError::InvalidJson)?;
    let mut results = Vec::new();
    for item in array.iter().take(MAX_RESULTS) {
        let Some((title, url, snippet, published_at)) = read(item) else {
            continue;
        };
        let Some(url) = normalize_url(url) else {
            continue;
        };
        results.push(WebSearchResult {
            title: bound_text(title, MAX_TITLE_LENGTH),
            url,
            snippet: bound_text(snippet, MAX_SNIPPET_LENGTH),
            published_at: published_at
                .map(|value| bound_text(value, MAX_PUBLISHED_LENGTH))
                .filter(|value| !value.is_empty()),
        });
    }
    Ok(results)
}

pub(crate) fn validate_query(query: String) -> Result<String, WebSearchError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(WebSearchError::EmptyQuery);
    }
    if query.chars().count() > MAX_QUERY_LENGTH {
        return Err(WebSearchError::QueryTooLong);
    }
    Ok(query.to_owned())
}

fn map_request_error(error: reqwest::Error) -> WebSearchError {
    if error.is_timeout() {
        WebSearchError::Timeout
    } else {
        WebSearchError::Request
    }
}

fn normalize_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| bound_text(parsed.as_str(), MAX_URL_LENGTH))
}

fn bound_text(value: &str, limit: usize) -> String {
    value
        .chars()
        .filter(|character| !character.is_control() || matches!(character, '\n' | '\t'))
        .take(limit)
        .collect::<String>()
        .trim()
        .to_owned()
}

fn validate_http_url(
    path: &std::path::Path,
    field: &str,
    value: &str,
) -> Result<(), crate::RuntimeError> {
    if validate_runtime_url(value).is_none() {
        return Err(crate::RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field} must be an absolute http or https URL"),
        });
    }
    Ok(())
}

fn validate_runtime_url(value: &str) -> Option<String> {
    let parsed = url::Url::parse(value.trim()).ok()?;
    matches!(parsed.scheme(), "http" | "https")
        .then(|| value.trim().trim_end_matches('/').to_owned())
}

fn validate_env_name(
    path: &std::path::Path,
    field: &str,
    value: &str,
) -> Result<(), crate::RuntimeError> {
    if value.is_empty()
        || !value.chars().enumerate().all(|(index, character)| {
            character == '_'
                || character.is_ascii_alphanumeric()
                    && (index > 0 || character.is_ascii_alphabetic())
        })
    {
        return Err(crate::RuntimeError::Config {
            path: path.to_path_buf(),
            message: format!("{field} must be a valid environment variable name"),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        Json, Router,
        extract::Query,
        http::HeaderMap,
        routing::{get, post},
    };
    use std::collections::HashMap;
    use tokio::net::TcpListener;

    #[test]
    fn normalizes_both_provider_shapes_and_bounds_results() {
        let searx = serde_json::json!({"results":[{"title":"  hi\0 ","url":"https://example.com","content":"snippet","publishedDate":"2025"}]});
        assert_eq!(normalize_searxng(&searx).unwrap()[0].title, "hi");
        let exa = serde_json::json!({"results":[{"title":"exa","url":"https://example.com/a","text":"text"}]});
        assert_eq!(normalize_exa(&exa).unwrap()[0].snippet, "text");
        let chatgpt = vec![serde_json::json!({
            "title": "ChatGPT",
            "url": "https://example.com/chatgpt",
            "snippet": "provider result",
            "unknown_field": true,
        })];
        let normalized = normalize_codex(&chatgpt).unwrap();
        assert_eq!(normalized[0].title, "ChatGPT");
        assert_eq!(normalized[0].snippet, "provider result");
    }

    #[test]
    fn chatgpt_is_always_listed_but_not_config_resolved() {
        let config = WebSearchConfig::default();
        let status = config
            .provider_statuses()
            .into_iter()
            .find(|status| status.provider == WebSearchProvider::Chatgpt)
            .expect("ChatGPT picker row");
        assert!(!status.active);
        assert!(!status.ready);
        assert!(
            config
                .resolve_provider(WebSearchProvider::Chatgpt)
                .is_none()
        );
    }

    #[test]
    fn query_validation_rejects_empty_and_long_queries() {
        assert!(matches!(
            validate_query("  ".into()),
            Err(WebSearchError::EmptyQuery)
        ));
        assert!(matches!(
            validate_query("x".repeat(MAX_QUERY_LENGTH + 1)),
            Err(WebSearchError::QueryTooLong)
        ));
    }

    #[test]
    fn response_does_not_repeat_the_query() {
        let response = WebSearchResponse {
            provider: WebSearchProvider::Searxng,
            results: Vec::new(),
        };
        assert!(
            serde_json::to_value(response)
                .unwrap()
                .get("query")
                .is_none()
        );
    }

    #[tokio::test]
    async fn searxng_http_contract_encodes_query_and_normalizes_json() {
        async fn handler(Query(query): Query<HashMap<String, String>>) -> Json<serde_json::Value> {
            assert_eq!(query.get("q").map(String::as_str), Some("rust & async"));
            assert_eq!(query.get("format").map(String::as_str), Some("json"));
            Json(
                serde_json::json!({"results": [{"title": "Rust", "url": "https://example.com", "content": "reference"}]}),
            )
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/search", get(handler)))
                .await
                .unwrap();
        });
        let config = ResolvedWebSearch {
            provider: WebSearchProvider::Searxng,
            endpoint: format!("http://{address}/search"),
            api_key: None,
            timeout: Duration::from_secs(2),
        };
        let response = search_resolved(
            &config,
            WebSearchRequest {
                query: "rust & async".into(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(response.results.len(), 1);
    }

    #[tokio::test]
    async fn exa_http_contract_sends_auth_and_bounded_body() {
        async fn handler(
            headers: HeaderMap,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            assert_eq!(
                headers
                    .get("x-api-key")
                    .and_then(|value| value.to_str().ok()),
                Some("secret")
            );
            assert_eq!(body["query"], "rust");
            assert_eq!(body["numResults"], MAX_RESULTS);
            Json(
                serde_json::json!({"results": [{"title": "Exa", "url": "https://example.com", "text": "reference"}]}),
            )
        }
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, Router::new().route("/search", post(handler)))
                .await
                .unwrap();
        });
        let config = ResolvedWebSearch {
            provider: WebSearchProvider::Exa,
            endpoint: format!("http://{address}/search"),
            api_key: Some("secret".into()),
            timeout: Duration::from_secs(2),
        };
        let response = search_resolved(
            &config,
            WebSearchRequest {
                query: "rust".into(),
            },
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(response.results[0].title, "Exa");
    }

    #[tokio::test]
    async fn cancellation_stops_a_search_before_the_request() {
        let cancellation = CancellationToken::new();
        cancellation.cancel();
        let config = ResolvedWebSearch {
            provider: WebSearchProvider::Searxng,
            endpoint: "http://127.0.0.1:1/search".into(),
            api_key: None,
            timeout: Duration::from_secs(2),
        };
        assert!(matches!(
            search_resolved(
                &config,
                WebSearchRequest {
                    query: "rust".into()
                },
                &cancellation,
            )
            .await,
            Err(WebSearchError::Cancelled)
        ));
    }
}
