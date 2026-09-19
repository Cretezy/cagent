#[allow(clippy::wildcard_imports)]
use super::*;

pub(super) async fn receive_oauth_callback(
    listener: tokio::net::TcpListener,
    provider: Arc<dyn Provider>,
) -> Result<(), RuntimeError> {
    use tokio::io::AsyncReadExt as _;

    tokio::time::timeout(std::time::Duration::from_secs(5 * 60), async move {
        loop {
            let (mut stream, _) = listener.accept().await.map_err(|error| {
                RuntimeError::InvalidOption(format!("OAuth callback failed: {error}"))
            })?;
            let mut request = Vec::with_capacity(1024);
            loop {
                let mut buffer = [0_u8; 1024];
                let read = stream.read(&mut buffer).await.map_err(|error| {
                    RuntimeError::InvalidOption(format!("OAuth callback read failed: {error}"))
                })?;
                if read == 0 || request.len().saturating_add(read) > 8 * 1024 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.contains(&b'\n') {
                    break;
                }
            }
            let request = std::str::from_utf8(&request).map_err(|_| {
                RuntimeError::InvalidOption("OAuth callback was not valid HTTP".into())
            })?;
            let target = request
                .lines()
                .next()
                .and_then(|line| line.split_whitespace().nth(1))
                .ok_or_else(|| {
                    RuntimeError::InvalidOption("OAuth callback request line is invalid".into())
                })?;
            let callback =
                reqwest::Url::parse(&format!("http://localhost{target}")).map_err(|error| {
                    RuntimeError::InvalidOption(format!("OAuth callback query is invalid: {error}"))
                })?;
            if callback.path() != "/auth/callback" {
                write_callback_response(
                    &mut stream,
                    "404 Not Found",
                    "Page not found",
                    "This isn't a valid Cagent authentication callback.",
                )
                .await;
                continue;
            }
            let parameters = callback
                .query_pairs()
                .collect::<std::collections::HashMap<_, _>>();
            let result = async {
                let state = parameters
                    .get("state")
                    .ok_or_else(|| {
                        RuntimeError::InvalidOption("OAuth callback omitted state".into())
                    })?
                    .to_string();
                let response = if let Some(error) = parameters.get("error") {
                    crate::AuthResponse::OAuthError {
                        error: error.to_string(),
                        description: parameters.get("error_description").map(ToString::to_string),
                        state,
                    }
                } else {
                    let code = parameters
                        .get("code")
                        .ok_or_else(|| {
                            RuntimeError::InvalidOption("OAuth callback omitted code".into())
                        })?
                        .to_string();
                    crate::AuthResponse::AuthorizationCode { code, state }
                };
                provider
                    .complete_auth(response)
                    .await
                    .map_err(|error| RuntimeError::InvalidOption(error.to_string()))
            }
            .await;
            if result.is_ok() {
                write_callback_response(
                    &mut stream,
                    "200 OK",
                    "Authentication completed",
                    "You can close this page and return to Cagent.",
                )
                .await;
            } else {
                write_callback_response(
                    &mut stream,
                    "400 Bad Request",
                    "Authentication failed",
                    "Return to Cagent for details and try again.",
                )
                .await;
            }
            return result;
        }
    })
    .await
    .map_err(|_| RuntimeError::InvalidOption("OAuth callback timed out".into()))?
}

async fn write_callback_response(
    stream: &mut tokio::net::TcpStream,
    status: &str,
    title: &str,
    message: &str,
) {
    use tokio::io::AsyncWriteExt as _;

    let body = callback_page(title, message, status == "200 OK");
    let response = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes()).await;
}

fn callback_page(title: &str, message: &str, success: bool) -> String {
    let (icon, accent) = if success {
        ("&#10003;", "#22c55e")
    } else {
        ("!", "#ef4444")
    };
    format!(
        r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>{title} · Cagent</title>
  <style>
    :root {{ color-scheme: light dark; font-family: ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif; }}
    * {{ box-sizing: border-box; }}
    body {{ margin: 0; min-height: 100vh; display: grid; place-items: center; padding: 24px; background: #f6f7f9; color: #18181b; }}
    main {{ width: min(100%, 440px); padding: 40px; text-align: center; background: white; border: 1px solid #e4e4e7; border-radius: 18px; box-shadow: 0 18px 50px rgba(0, 0, 0, .08); }}
    .icon {{ display: grid; place-items: center; width: 52px; height: 52px; margin: 0 auto 24px; border-radius: 50%; background: {accent}; color: white; font-size: 28px; font-weight: 700; }}
    h1 {{ margin: 0 0 12px; font-size: 24px; line-height: 1.25; }}
    p {{ margin: 0; color: #52525b; font-size: 16px; line-height: 1.6; }}
    .brand {{ margin-top: 28px; color: #a1a1aa; font-size: 13px; font-weight: 600; letter-spacing: .08em; text-transform: uppercase; }}
    @media (prefers-color-scheme: dark) {{
      body {{ background: #09090b; color: #fafafa; }}
      main {{ background: #18181b; border-color: #27272a; box-shadow: 0 18px 50px rgba(0, 0, 0, .35); }}
      p {{ color: #a1a1aa; }}
    }}
  </style>
</head>
<body>
  <main>
    <div class="icon" aria-hidden="true">{icon}</div>
    <h1>{title}</h1>
    <p>{message}</p>
    <div class="brand">Cagent</div>
  </main>
</body>
</html>"#
    )
}

pub(super) async fn bind_oauth_callback(
    callback_url: &str,
) -> Result<tokio::net::TcpListener, RuntimeError> {
    let callback = reqwest::Url::parse(callback_url).map_err(|error| {
        RuntimeError::InvalidOption(format!("invalid OAuth callback URL: {error}"))
    })?;
    let host = callback
        .host_str()
        .ok_or_else(|| RuntimeError::InvalidOption("OAuth callback has no host".into()))?;
    if !matches!(host, "localhost" | "127.0.0.1" | "::1") {
        return Err(RuntimeError::InvalidOption(
            "OAuth callback must use loopback".into(),
        ));
    }
    let port = callback
        .port_or_known_default()
        .ok_or_else(|| RuntimeError::InvalidOption("OAuth callback has no port".into()))?;
    let bind_host = if host == "localhost" {
        "127.0.0.1"
    } else {
        host
    };
    tokio::net::TcpListener::bind((bind_host, port))
        .await
        .map_err(|error| {
            RuntimeError::InvalidOption(format!(
                "OAuth callback port {port} is unavailable: {error}"
            ))
        })
}
