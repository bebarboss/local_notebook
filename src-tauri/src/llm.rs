//! OpenAI-compatible Chat Completions client for the Gosoft Agent Gateway (LiteLLM),
//! with SSE streaming. The Bearer key is attached here and nowhere else.

use crate::errors::{AppError, AppResult};
use crate::redact::redact_secret;
use futures_util::StreamExt;
use once_cell::sync::Lazy;
use serde::Serialize;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone, Serialize)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn new(role: &str, content: impl Into<String>) -> Self {
        Self { role: role.into(), content: content.into() }
    }
}

/// One shared client: system proxy (reqwest reads the Windows proxy settings and
/// HTTP(S)_PROXY) + Windows certificate store via rustls-native-certs.
pub static CLIENT: Lazy<reqwest::Client> = Lazy::new(|| {
    reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .read_timeout(Duration::from_secs(180))
        .user_agent(concat!("DocAsk/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("http client")
});

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    messages: &'a [ChatMessage],
    stream: bool,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
}

pub struct Request<'a> {
    pub url: &'a str,
    pub key: &'a str,
    pub model: &'a str,
    pub effort: Option<&'a str>,
    pub max_tokens: u64,
    pub messages: &'a [ChatMessage],
}

async fn send(req: &Request<'_>, stream: bool, total_timeout: Option<Duration>) -> AppResult<reqwest::Response> {
    let body = Body {
        model: req.model,
        messages: req.messages,
        stream,
        max_tokens: req.max_tokens,
        reasoning_effort: req.effort,
    };
    let mut rb = CLIENT.post(req.url).bearer_auth(req.key).json(&body);
    if stream {
        rb = rb.header("Accept", "text/event-stream");
    }
    if let Some(t) = total_timeout {
        rb = rb.timeout(t);
    }
    let resp = rb.send().await.map_err(AppError::from)?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let mut e = AppError::from_status(status.as_u16(), &text);
        e.message = redact_secret(&e.message, Some(req.key));
        return Err(e);
    }
    Ok(resp)
}

/// Non-streaming call (connection test).
pub async fn complete(req: &Request<'_>) -> AppResult<String> {
    let resp = send(req, false, Some(Duration::from_secs(90))).await?;
    let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
    Ok(v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or_default().to_string())
}

pub enum StreamEnd {
    Finished(String),
    Cancelled(String),
}

/// Stream a completion, calling `on_delta` for each content fragment.
pub async fn stream(
    req: &Request<'_>,
    cancel: &CancellationToken,
    mut on_delta: impl FnMut(&str),
) -> AppResult<StreamEnd> {
    let resp = tokio::select! {
        r = send(req, true, None) => r?,
        _ = cancel.cancelled() => return Ok(StreamEnd::Cancelled(String::new())),
    };
    let mut body = resp.bytes_stream();
    let mut buf: Vec<u8> = Vec::new();
    let mut full = String::new();
    loop {
        let chunk = tokio::select! {
            c = body.next() => c,
            _ = cancel.cancelled() => return Ok(StreamEnd::Cancelled(full)),
        };
        let Some(chunk) = chunk else { break };
        let chunk = chunk.map_err(AppError::from)?;
        buf.extend_from_slice(&chunk);
        // process complete lines only (UTF-8 may be split across chunks)
        while let Some(pos) = buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            match parse_sse_line(line.trim()) {
                SseLine::Delta(d) => {
                    full.push_str(&d);
                    on_delta(&d);
                }
                SseLine::Done => return Ok(StreamEnd::Finished(full)),
                SseLine::Error(msg) => {
                    return Err(AppError::new("stream_error", redact_secret(&msg, Some(req.key))));
                }
                SseLine::Skip => {}
            }
        }
    }
    Ok(StreamEnd::Finished(full))
}

#[derive(Debug, PartialEq)]
pub enum SseLine {
    Delta(String),
    Done,
    Error(String),
    Skip,
}

pub fn parse_sse_line(line: &str) -> SseLine {
    let Some(data) = line.strip_prefix("data:") else { return SseLine::Skip };
    let data = data.trim();
    if data == "[DONE]" {
        return SseLine::Done;
    }
    let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else { return SseLine::Skip };
    if let Some(err) = v.get("error") {
        let msg = err.get("message").and_then(|m| m.as_str()).map(str::to_string).unwrap_or_else(|| err.to_string());
        return SseLine::Error(msg);
    }
    match v.pointer("/choices/0/delta/content").and_then(|c| c.as_str()) {
        Some(s) if !s.is_empty() => SseLine::Delta(s.to_string()),
        _ => SseLine::Skip,
    }
}

/// GET /v1/models — used to tell "VPN down" apart from "key bad".
pub async fn list_models(base: &str, key: &str) -> AppResult<Vec<String>> {
    let url = format!("{}/v1/models", crate::config::normalize_base(base));
    let resp = CLIENT
        .get(&url)
        .bearer_auth(key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(AppError::from)?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AppError::from_status(status.as_u16(), &redact_secret(&text, Some(key))));
    }
    let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
    Ok(v.get("data")
        .and_then(|d| d.as_array())
        .map(|a| a.iter().filter_map(|m| m.get("id").and_then(|i| i.as_str()).map(str::to_string)).collect())
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sse() {
        assert_eq!(
            parse_sse_line(r#"data: {"choices":[{"delta":{"content":"สวัสดี"}}]}"#),
            SseLine::Delta("สวัสดี".into())
        );
        assert_eq!(parse_sse_line("data: [DONE]"), SseLine::Done);
        assert_eq!(parse_sse_line(": keep-alive"), SseLine::Skip);
        assert_eq!(parse_sse_line(r#"data: {"choices":[{"delta":{"role":"assistant"}}]}"#), SseLine::Skip);
        assert!(matches!(parse_sse_line(r#"data: {"error":{"message":"quota"}}"#), SseLine::Error(_)));
    }
}
