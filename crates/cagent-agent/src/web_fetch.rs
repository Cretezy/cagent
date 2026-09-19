//! Native, provider-neutral HTTP(S) fetching.
//!
//! Web pages are untrusted input. This module only validates, bounds, and
//! normalizes the response; permission checks remain at the runtime boundary.

use std::future::Future;
use std::time::Duration;

use futures_util::StreamExt as _;
use reqwest::{header, redirect::Policy};
use serde::{Deserialize, Serialize};
use tokio_util::sync::CancellationToken;
use url::Url;

pub const DEFAULT_TIMEOUT_SECONDS: u64 = 30;
pub const MAX_TIMEOUT_SECONDS: u64 = 120;
pub const MAX_REDIRECTS: usize = 10;
pub const MAX_RESPONSE_BYTES: usize = 5 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum WebFetchFormat {
    #[default]
    Markdown,
    Text,
    Html,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WebFetchRequest {
    pub url: String,
    #[serde(default)]
    pub format: WebFetchFormat,
    #[serde(default = "default_timeout")]
    pub timeout: u64,
}

const fn default_timeout() -> u64 {
    DEFAULT_TIMEOUT_SECONDS
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct WebFetchResponse {
    /// The destination after redirects, included only when it differs from
    /// the requested URL.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    pub content_type: String,
    pub output: String,
}

#[derive(Debug, thiserror::Error, Eq, PartialEq)]
pub enum WebFetchError {
    #[error("invalid URL: {0}")]
    InvalidUrl(String),
    #[error("web fetch permission denied")]
    PermissionDenied,
    #[error("redirect limit exceeded")]
    RedirectLimit,
    #[error("redirect response did not include a valid Location")]
    InvalidRedirect,
    #[error("HTTP status {0}")]
    HttpStatus(u16),
    #[error("web fetch timed out")]
    Timeout,
    #[error("web fetch was cancelled")]
    Cancelled,
    #[error("unsupported content type: {0}")]
    UnsupportedContent(String),
    #[error("response exceeds the 5 MiB limit")]
    ResponseLimit,
    #[error("could not convert fetched HTML")]
    Conversion,
    #[error("web fetch request failed")]
    Request,
}

impl WebFetchRequest {
    pub fn validate(mut self) -> Result<(Self, Url), WebFetchError> {
        let url = validate_url(&self.url)?;
        if self.timeout == 0 || self.timeout > MAX_TIMEOUT_SECONDS {
            return Err(WebFetchError::InvalidUrl(format!(
                "timeout must be between 1 and {MAX_TIMEOUT_SECONDS} seconds"
            )));
        }
        self.url = url.to_string();
        Ok((self, url))
    }
}

pub fn validate_url(value: &str) -> Result<Url, WebFetchError> {
    let url = Url::parse(value)
        .map_err(|_| WebFetchError::InvalidUrl("must be an absolute HTTP(S) URL".into()))?;
    if !matches!(url.scheme(), "http" | "https") || url.host().is_none() {
        return Err(WebFetchError::InvalidUrl(
            "must be an absolute HTTP(S) URL".into(),
        ));
    }
    Ok(url)
}

/// Whether a redirect can reuse authorization for the initial fetch URL.
///
/// This intentionally recognizes only narrow canonical and same-site changes;
/// callers must still evaluate explicit permission rules for the destination.
#[must_use]
pub fn is_safe_redirect(
    origin: &Url,
    destination: &Url,
    generally_safe: bool,
    same_site: bool,
) -> bool {
    if !generally_safe && !same_site
        || origin.username() != destination.username()
        || origin.password() != destination.password()
        || non_default_port(origin) != non_default_port(destination)
    {
        return false;
    }

    let Some(origin_host) = origin.host_str() else {
        return false;
    };
    let Some(destination_host) = destination.host_str() else {
        return false;
    };
    let scheme_changed = origin.scheme() != destination.scheme();
    let host_changed = origin_host != destination_host;
    let route_changed =
        origin.path() != destination.path() || origin.query() != destination.query();

    if scheme_changed
        && !(generally_safe && origin.scheme() == "http" && destination.scheme() == "https")
    {
        return false;
    }
    if host_changed && !(generally_safe && differs_only_by_www(origin_host, destination_host)) {
        return false;
    }
    !route_changed || same_site
}

fn differs_only_by_www(left: &str, right: &str) -> bool {
    left.strip_prefix("www.") == Some(right) || right.strip_prefix("www.") == Some(left)
}

fn non_default_port(url: &Url) -> Option<u16> {
    let default = match url.scheme() {
        "http" => Some(80),
        "https" => Some(443),
        _ => None,
    };
    url.port().filter(|port| Some(*port) != default)
}

/// Fetches a document, calling `authorize` before every requested URL,
/// including every redirect destination. `redirect_chain` contains the URLs
/// that led to the destination, in request order.
pub async fn fetch<Authorize, AuthorizationFuture>(
    request: WebFetchRequest,
    cancellation: &CancellationToken,
    mut authorize: Authorize,
) -> Result<WebFetchResponse, WebFetchError>
where
    Authorize: FnMut(Url, Vec<Url>) -> AuthorizationFuture,
    AuthorizationFuture: Future<Output = Result<(), WebFetchError>>,
{
    let (request, original) = request.validate()?;
    let client = reqwest::Client::builder()
        .redirect(Policy::none())
        .build()
        .map_err(|_| WebFetchError::Request)?;
    let mut current = original.clone();
    let mut redirect_chain = Vec::new();
    for redirect_count in 0..=MAX_REDIRECTS {
        authorize(current.clone(), redirect_chain.clone()).await?;
        let accept = match request.format {
            WebFetchFormat::Html => {
                "text/html, application/xhtml+xml;q=0.9, text/plain;q=0.5, */*;q=0.1"
            }
            WebFetchFormat::Markdown | WebFetchFormat::Text => {
                "text/html, text/plain, application/json, application/xml, text/xml, application/javascript, text/javascript;q=0.9, */*;q=0.1"
            }
        };
        let response = tokio::select! {
            () = cancellation.cancelled() => return Err(WebFetchError::Cancelled),
            result = client.get(current.clone()).header(header::ACCEPT, accept).timeout(Duration::from_secs(request.timeout)).send() => result.map_err(map_request_error)?,
        };
        if response.status().is_redirection() {
            if redirect_count == MAX_REDIRECTS {
                return Err(WebFetchError::RedirectLimit);
            }
            let location = response
                .headers()
                .get(header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .ok_or(WebFetchError::InvalidRedirect)?;
            redirect_chain.push(current.clone());
            current = current
                .join(location)
                .map_err(|_| WebFetchError::InvalidRedirect)?;
            validate_url(current.as_str()).map_err(|_| WebFetchError::InvalidRedirect)?;
            continue;
        }
        if !response.status().is_success() {
            return Err(WebFetchError::HttpStatus(response.status().as_u16()));
        }
        let content_type = response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_owned();
        if !is_textual_content_type(&content_type) {
            return Err(WebFetchError::UnsupportedContent(content_type));
        }
        if response
            .content_length()
            .is_some_and(|length| length > MAX_RESPONSE_BYTES as u64)
        {
            return Err(WebFetchError::ResponseLimit);
        }
        let mut stream = response.bytes_stream();
        let mut body = Vec::new();
        loop {
            let chunk = tokio::select! {
                () = cancellation.cancelled() => return Err(WebFetchError::Cancelled),
                next = stream.next() => next,
            };
            let Some(chunk) = chunk else {
                break;
            };
            let chunk = chunk.map_err(map_request_error)?;
            if body.len().saturating_add(chunk.len()) > MAX_RESPONSE_BYTES {
                return Err(WebFetchError::ResponseLimit);
            }
            body.extend_from_slice(&chunk);
        }
        let raw = String::from_utf8_lossy(&body).into_owned();
        let output = normalize(&raw, &content_type, request.format)?;
        return Ok(WebFetchResponse {
            url: (current.as_str() != request.url).then(|| current.to_string()),
            content_type,
            output,
        });
    }
    Err(WebFetchError::RedirectLimit)
}

fn map_request_error(error: reqwest::Error) -> WebFetchError {
    if error.is_timeout() {
        WebFetchError::Timeout
    } else {
        WebFetchError::Request
    }
}

fn is_textual_content_type(content_type: &str) -> bool {
    let mime = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    mime.is_empty()
        || mime.starts_with("text/")
        || mime == "application/json"
        || mime.ends_with("+json")
        || mime == "application/xml"
        || mime.ends_with("+xml")
        || matches!(
            mime.as_str(),
            "application/javascript" | "application/x-javascript" | "application/ecmascript"
        )
}

fn normalize(
    raw: &str,
    content_type: &str,
    format: WebFetchFormat,
) -> Result<String, WebFetchError> {
    match format {
        WebFetchFormat::Html => Ok(raw.to_owned()),
        WebFetchFormat::Markdown
            if content_type.to_ascii_lowercase().starts_with("text/html")
                || content_type.to_ascii_lowercase().contains("xhtml") =>
        {
            Ok(html2md::parse_html(raw))
        }
        WebFetchFormat::Markdown | WebFetchFormat::Text => Ok(html2md::parse_html(raw)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_http_urls_and_defaults() {
        let request: WebFetchRequest =
            serde_json::from_value(serde_json::json!({"url":"https://example.com"})).unwrap();
        assert_eq!(request.format, WebFetchFormat::Markdown);
        assert_eq!(request.timeout, DEFAULT_TIMEOUT_SECONDS);
        assert!(validate_url("http://127.0.0.1:3000/a").is_ok());
        assert!(validate_url("ftp://example.com").is_err());
        assert!(
            WebFetchRequest {
                url: "https://example.com".into(),
                format: WebFetchFormat::Text,
                timeout: 121
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn converts_html_and_preserves_raw_html() {
        let html = "<h1>Hello</h1><script>bad()</script><p>World</p>";
        assert!(
            normalize(html, "text/html", WebFetchFormat::Markdown)
                .unwrap()
                .contains("Hello")
        );
        assert_eq!(
            normalize(html, "text/html", WebFetchFormat::Html).unwrap(),
            html
        );
        assert!(is_textual_content_type("application/json"));
        assert!(!is_textual_content_type("application/pdf"));
    }

    #[test]
    fn response_omits_url_without_a_redirect() {
        let direct = WebFetchResponse {
            url: None,
            content_type: "text/plain".into(),
            output: "direct content".into(),
        };
        let direct = serde_json::to_value(direct).unwrap();
        assert!(direct.get("url").is_none());
        assert!(direct.get("format").is_none());

        let redirected = WebFetchResponse {
            url: Some("https://www.example.com/".into()),
            content_type: "text/plain".into(),
            output: "redirected content".into(),
        };
        assert_eq!(
            serde_json::to_value(redirected)
                .unwrap()
                .get("url")
                .and_then(serde_json::Value::as_str),
            Some("https://www.example.com/")
        );
    }

    #[test]
    fn recognizes_only_configured_safe_redirects() {
        let origin = Url::parse("http://example.com/path?query=yes").unwrap();
        let canonical = Url::parse("https://www.example.com/path?query=yes").unwrap();
        let same_site = Url::parse("https://www.example.com/other?query=no").unwrap();
        assert!(is_safe_redirect(&origin, &canonical, true, false));
        assert!(is_safe_redirect(&canonical, &same_site, true, true));
        assert!(!is_safe_redirect(&canonical, &same_site, true, false));
        assert!(!is_safe_redirect(
            &canonical,
            &Url::parse("http://www.example.com/path?query=yes").unwrap(),
            true,
            true,
        ));
        assert!(!is_safe_redirect(
            &canonical,
            &Url::parse("https://login.example.com/path?query=yes").unwrap(),
            true,
            true,
        ));
        assert!(!is_safe_redirect(
            &canonical,
            &Url::parse("https://www.example.com:8443/path?query=yes").unwrap(),
            true,
            true,
        ));
    }
}
