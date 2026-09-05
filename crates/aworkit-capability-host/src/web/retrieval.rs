//! Cancellable HTTP retrieval retains useful prefixes, with a decoded byte cap.

use super::{CancellationToken, REQUEST_TIMEOUT, document::WebSourceV1};

pub(super) fn retrieve(
    url: &str,
    maximum: usize,
    cancellation: &CancellationToken,
) -> Result<WebSourceV1, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    runtime.block_on(async {
        let work = async {
            let client = reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .redirect(reqwest::redirect::Policy::custom(|attempt| {
                    if attempt.previous().len() >= 5 {
                        return attempt.error("too many web redirects");
                    }
                    if super::parse_https_url(attempt.url().as_str()).is_err() {
                        return attempt.error("redirect must remain HTTPS without credentials");
                    }
                    attempt.follow()
                }))
                .user_agent("Aworkit/1.0 web-fetch")
                .build()
                .map_err(|e| format!("web client unavailable: {e}"))?;
            let mut response = client
                .get(url)
                .send()
                .await
                .map_err(|e| format!("web fetch failed: {e}"))?;
            if !response.status().is_success() {
                return Err(format!("web fetch failed: HTTP {}", response.status()));
            }
            let final_url = response.url().to_string();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|h| h.to_str().ok())
                .unwrap_or("application/octet-stream")
                .to_owned();
            let mut body = Vec::new();
            let mut truncated = false;
            let mut warning = None;
            loop {
                match response.chunk().await {
                    Ok(Some(chunk)) => {
                        let remaining = maximum.saturating_sub(body.len());
                        body.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
                        if chunk.len() > remaining {
                            truncated = true;
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(error) if !body.is_empty() => {
                        truncated = true;
                        warning = Some(format!("download interrupted: {error}"));
                        break;
                    }
                    Err(error) => return Err(format!("web fetch stream failed: {error}")),
                }
            }
            let bytes_downloaded = body.len() as u64;
            let charset = content_type
                .split(';')
                .filter_map(|p| p.trim().split_once('='))
                .find(|(name, _)| name.eq_ignore_ascii_case("charset"))
                .and_then(|(_, label)| {
                    encoding_rs::Encoding::for_label(
                        label.trim().trim_matches(['\'', '"']).as_bytes(),
                    )
                });
            static META_CHARSET: std::sync::LazyLock<regex::Regex> =
                std::sync::LazyLock::new(|| {
                    regex::Regex::new(r#"(?i)<meta\b[^>]*charset\s*=\s*["']?\s*([a-z0-9_-]+)"#)
                        .expect("static charset pattern")
                });
            let head = String::from_utf8_lossy(&body[..body.len().min(1024)]);
            let meta_charset = META_CHARSET
                .captures(&head)
                .and_then(|c| encoding_rs::Encoding::for_label(c[1].as_bytes()));
            let encoding = encoding_rs::Encoding::for_bom(&body)
                .map(|(encoding, _)| encoding)
                .or(charset)
                .or(meta_charset)
                .unwrap_or(encoding_rs::UTF_8);
            if truncated && encoding == encoding_rs::UTF_8 {
                if let Err(error) = std::str::from_utf8(&body) {
                    if error.error_len().is_none() {
                        body.truncate(error.valid_up_to());
                    }
                }
            }
            let (body, _, had_errors) = encoding.decode(&body);
            if had_errors {
                warning =
                    Some("Some invalid encoded characters were replaced during decoding.".into());
            }
            Ok(WebSourceV1 {
                final_url,
                body: body.into_owned(),
                content_type,
                bytes_downloaded,
                truncated,
                warning,
                title: None,
            })
        };
        tokio::pin!(work);
        loop {
            if cancellation.is_cancelled() {
                return Err("web request was cancelled".into());
            }
            tokio::select! {
                result = &mut work => return result,
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
    })
}
