use futures_util::StreamExt as _;

const DEFAULT_ERROR_BODY_LIMIT: usize = 16 * 1024;

#[derive(Debug)]
pub(crate) struct HttpErrorBody {
    pub status: reqwest::StatusCode,
    pub retry_after_millis: Option<u64>,
    pub body: String,
}

pub(crate) async fn read_http_error(response: reqwest::Response) -> HttpErrorBody {
    let status = response.status();
    let retry_after_millis = response
        .headers()
        .get(reqwest::header::RETRY_AFTER)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok())
        .map(|seconds| seconds.saturating_mul(1_000));
    let mut stream = response.bytes_stream();
    let mut bytes = Vec::new();
    while bytes.len() < DEFAULT_ERROR_BODY_LIMIT {
        let Some(chunk) = stream.next().await else {
            break;
        };
        let Ok(chunk) = chunk else {
            break;
        };
        let remaining = DEFAULT_ERROR_BODY_LIMIT - bytes.len();
        bytes.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
    }
    let body = String::from_utf8_lossy(&bytes).into_owned();
    HttpErrorBody {
        status,
        retry_after_millis,
        body,
    }
}

pub(crate) fn find_sse_boundary(buffer: &[u8]) -> Option<(usize, usize)> {
    buffer
        .windows(2)
        .position(|bytes| bytes == b"\n\n")
        .map_or_else(
            || {
                buffer
                    .windows(4)
                    .position(|bytes| bytes == b"\r\n\r\n")
                    .map(|position| (position, 4))
            },
            |position| Some((position, 2)),
        )
}

#[cfg(test)]
mod tests {
    use super::find_sse_boundary;

    #[test]
    fn frames_fragmented_lf_and_crlf_events() {
        assert_eq!(find_sse_boundary(b"data: {}\n"), None);
        assert_eq!(find_sse_boundary(b"data: {}\n\nrest"), Some((8, 2)));
        assert_eq!(find_sse_boundary(b"data: {}\r\n\r\nrest"), Some((8, 4)));
    }
}
