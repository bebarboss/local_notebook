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

/// Non-streaming call with its own time limit (long jobs such as writing notes).
pub async fn complete_within(req: &Request<'_>, limit: Duration) -> AppResult<String> {
    let resp = send(req, false, Some(limit)).await?;
    let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
    Ok(v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or_default().to_string())
}

/// One function the model asked to call during a tool-calling round.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    /// the arguments as the model wrote them (a JSON string; may be malformed)
    pub arguments: String,
}

/// A tool-calling round's result: any text the model wrote and the tools it wants run.
#[derive(Debug, Clone, PartialEq)]
pub struct Reply {
    pub content: String,
    pub tool_calls: Vec<ToolCall>,
}

#[derive(Serialize)]
struct ToolBody<'a> {
    model: &'a str,
    messages: &'a serde_json::Value,
    stream: bool,
    max_tokens: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    reasoning_effort: Option<&'a str>,
    tools: &'a serde_json::Value,
    tool_choice: &'a str,
}

/// One non-streaming tool-calling round: send the conversation so far plus the tool schemas,
/// read back the model's text and any tool calls. The Bearer key is attached here only.
/// `messages` is a raw JSON array so a round can carry assistant `tool_calls` and `tool` results.
#[allow(clippy::too_many_arguments)]
pub async fn complete_tools(
    url: &str,
    key: &str,
    model: &str,
    effort: Option<&str>,
    max_tokens: u64,
    messages: &serde_json::Value,
    tools: &serde_json::Value,
    limit: Duration,
) -> AppResult<Reply> {
    let body = ToolBody { model, messages, stream: false, max_tokens, reasoning_effort: effort, tools, tool_choice: "auto" };
    let resp = CLIENT.post(url).bearer_auth(key).json(&body).timeout(limit).send().await.map_err(AppError::from)?;
    let status = resp.status();
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        let mut e = AppError::from_status(status.as_u16(), &text);
        e.message = redact_secret(&e.message, Some(key));
        return Err(e);
    }
    let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
    Ok(parse_tool_reply(&v))
}

/// Pull the text and tool calls out of a chat-completions response.
pub fn parse_tool_reply(v: &serde_json::Value) -> Reply {
    let content = v.pointer("/choices/0/message/content").and_then(|c| c.as_str()).unwrap_or_default().to_string();
    let tool_calls = v
        .pointer("/choices/0/message/tool_calls")
        .and_then(|t| t.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|tc| {
                    let f = tc.get("function")?;
                    Some(ToolCall {
                        id: tc.get("id").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                        name: f.get("name").and_then(|x| x.as_str())?.to_string(),
                        arguments: f.get("arguments").and_then(|x| x.as_str()).unwrap_or_default().to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    Reply { content, tool_calls }
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

/// LiteLLM virtual-key usage: spend/budget/limits for one key (`GET /key/info`).
/// All fields optional because a gateway may report only a subset.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyUsage {
    pub key_alias: Option<String>,
    pub spend: Option<f64>,
    pub max_budget: Option<f64>,
    pub soft_budget: Option<f64>,
    pub budget_duration: Option<String>,
    pub budget_reset_at: Option<String>,
    pub expires: Option<String>,
    pub tpm_limit: Option<i64>,
    pub rpm_limit: Option<i64>,
    pub max_parallel_requests: Option<i64>,
    pub models: Vec<String>,
}

fn parse_key_usage(info: &serde_json::Value) -> KeyUsage {
    let s = |k: &str| info.get(k).and_then(|x| x.as_str()).filter(|x| !x.is_empty()).map(str::to_string);
    let f = |k: &str| info.get(k).and_then(|x| x.as_f64());
    let i = |k: &str| info.get(k).and_then(|x| x.as_i64());
    KeyUsage {
        key_alias: s("key_alias").or_else(|| s("key_name")),
        spend: f("spend"),
        max_budget: f("max_budget"),
        soft_budget: f("soft_budget"),
        budget_duration: s("budget_duration"),
        budget_reset_at: s("budget_reset_at"),
        expires: s("expires"),
        tpm_limit: i("tpm_limit"),
        rpm_limit: i("rpm_limit"),
        max_parallel_requests: i("max_parallel_requests"),
        models: info
            .get("models")
            .and_then(|m| m.as_array())
            .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect())
            .unwrap_or_default(),
    }
}

/// GET /key/info — usage for the calling virtual key, read from the Authorization
/// header (the key is never put in the URL). Returns Ok(None) when this gateway
/// does not expose the endpoint, or will not report it for this key — so the UI
/// can say "not available here" instead of showing an error. The Bearer key is
/// attached here only.
pub async fn key_info(base: &str, key: &str) -> AppResult<Option<KeyUsage>> {
    let url = format!("{}/key/info", crate::config::normalize_base(base));
    let resp = CLIENT
        .get(&url)
        .bearer_auth(key)
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(AppError::from)?;
    let status = resp.status();
    // endpoint missing / disabled / needs admin rights to read → "not available", not an error
    if matches!(status.as_u16(), 400 | 403 | 404 | 405 | 501) {
        return Ok(None);
    }
    if !status.is_success() {
        let text = resp.text().await.unwrap_or_default();
        return Err(AppError::from_status(status.as_u16(), &redact_secret(&text, Some(key))));
    }
    let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
    let info = v.get("info").filter(|i| i.is_object()).unwrap_or(&v);
    Ok(Some(parse_key_usage(info)))
}

/// Spend/token/request totals for a period (or one day, or one model). Mirrors LiteLLM's SpendMetrics.
#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ActivityMetrics {
    pub spend: f64,
    pub prompt_tokens: i64,
    pub completion_tokens: i64,
    pub total_tokens: i64,
    pub requests: i64,
    pub successful_requests: i64,
    pub failed_requests: i64,
}

impl ActivityMetrics {
    fn add(&mut self, m: &serde_json::Value) {
        let f = |k: &str| m.get(k).and_then(|x| x.as_f64()).unwrap_or(0.0);
        let i = |k: &str| m.get(k).and_then(|x| x.as_i64()).unwrap_or(0);
        self.spend += f("spend");
        self.prompt_tokens += i("prompt_tokens");
        self.completion_tokens += i("completion_tokens");
        self.total_tokens += i("total_tokens");
        self.requests += i("api_requests");
        self.successful_requests += i("successful_requests");
        self.failed_requests += i("failed_requests");
    }
}

/// One day's spend/tokens/requests, for the daily chart.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DayActivity {
    pub date: String,
    pub spend: f64,
    pub total_tokens: i64,
    pub requests: i64,
}

/// One model's share of the period's spend/tokens/requests.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelActivity {
    pub model: String,
    pub spend: f64,
    pub total_tokens: i64,
    pub requests: i64,
}

/// Aggregated usage over a date range: period totals, a per-day series, and a per-model breakdown.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KeyActivity {
    pub total: ActivityMetrics,
    pub daily: Vec<DayActivity>,
    pub models: Vec<ModelActivity>,
}

/// GET /user/daily/activity — per-day spend/tokens/requests with a per-model breakdown, aggregated
/// client-side over the whole range (LiteLLM paginates by DB row, not by day, and its page totals
/// cover only that page, so we sum the rows ourselves across pages). `start`/`end` are YYYY-MM-DD.
/// Returns Ok(None) when the gateway does not expose the endpoint for this key. Bearer key only.
pub async fn key_activity(base: &str, key: &str, start: &str, end: &str) -> AppResult<Option<KeyActivity>> {
    let base = crate::config::normalize_base(base);
    let mut total = ActivityMetrics::default();
    let mut per_day: Vec<DayActivity> = Vec::new();
    let mut per_model: std::collections::HashMap<String, ActivityMetrics> = std::collections::HashMap::new();
    let mut page = 1u32;
    loop {
        let url = format!("{base}/user/daily/activity?start_date={start}&end_date={end}&page={page}&page_size=1000");
        let resp = CLIENT.get(&url).bearer_auth(key).timeout(Duration::from_secs(20)).send().await.map_err(AppError::from)?;
        let status = resp.status();
        if matches!(status.as_u16(), 400 | 403 | 404 | 405 | 501) {
            return Ok(None);
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            return Err(AppError::from_status(status.as_u16(), &redact_secret(&text, Some(key))));
        }
        let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
        let results = v.get("results").and_then(|r| r.as_array()).cloned().unwrap_or_default();
        fold_results(&results, &mut total, &mut per_day, &mut per_model);
        let has_more = v.pointer("/metadata/has_more").and_then(|h| h.as_bool()).unwrap_or(false);
        if !has_more || results.is_empty() || page >= 50 {
            break;
        }
        page += 1;
    }
    Ok(Some(finalize_activity(total, per_day, per_model)))
}

/// Accumulate one page's `results` (days) into the running totals, per-day list and per-model map.
fn fold_results(
    results: &[serde_json::Value],
    total: &mut ActivityMetrics,
    per_day: &mut Vec<DayActivity>,
    per_model: &mut std::collections::HashMap<String, ActivityMetrics>,
) {
    for day in results {
        let Some(metrics) = day.get("metrics") else { continue };
        total.add(metrics);
        per_day.push(DayActivity {
            date: day.get("date").and_then(|d| d.as_str()).unwrap_or("").to_string(),
            spend: metrics.get("spend").and_then(|x| x.as_f64()).unwrap_or(0.0),
            total_tokens: metrics.get("total_tokens").and_then(|x| x.as_i64()).unwrap_or(0),
            requests: metrics.get("api_requests").and_then(|x| x.as_i64()).unwrap_or(0),
        });
        if let Some(models) = day.pointer("/breakdown/models").and_then(|m| m.as_object()) {
            for (name, data) in models {
                if let Some(m) = data.get("metrics") {
                    per_model.entry(name.clone()).or_default().add(m);
                }
            }
        }
    }
}

/// Merge same-date rows (one date can span two pages under row-based paging), sort models by spend.
fn finalize_activity(
    total: ActivityMetrics,
    mut per_day: Vec<DayActivity>,
    per_model: std::collections::HashMap<String, ActivityMetrics>,
) -> KeyActivity {
    per_day.sort_by(|a, b| a.date.cmp(&b.date));
    let mut daily: Vec<DayActivity> = Vec::new();
    for d in per_day {
        match daily.last_mut() {
            Some(last) if last.date == d.date => {
                last.spend += d.spend;
                last.total_tokens += d.total_tokens;
                last.requests += d.requests;
            }
            _ => daily.push(d),
        }
    }
    let mut models: Vec<ModelActivity> = per_model
        .into_iter()
        .map(|(model, m)| ModelActivity { model, spend: m.spend, total_tokens: m.total_tokens, requests: m.requests })
        .collect();
    models.sort_by(|a, b| b.spend.partial_cmp(&a.spend).unwrap_or(std::cmp::Ordering::Equal));
    KeyActivity { total, daily, models }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_key_usage_from_info_envelope() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"key":"sk-x","info":{"key_alias":"docask","spend":1.25,"max_budget":10.0,
                "tpm_limit":1000,"rpm_limit":60,"models":["gpt-4o","claude"],"budget_duration":"30d"}}"#,
        )
        .unwrap();
        let info = v.get("info").unwrap();
        let u = parse_key_usage(info);
        assert_eq!(u.key_alias.as_deref(), Some("docask"));
        assert_eq!(u.spend, Some(1.25));
        assert_eq!(u.max_budget, Some(10.0));
        assert_eq!(u.tpm_limit, Some(1000));
        assert_eq!(u.models, vec!["gpt-4o", "claude"]);
        assert_eq!(u.soft_budget, None);
    }

    #[test]
    fn parses_key_usage_with_nulls_and_empty() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"spend":0,"max_budget":null,"key_alias":"","models":null}"#).unwrap();
        let u = parse_key_usage(&v);
        assert_eq!(u.spend, Some(0.0));
        assert_eq!(u.max_budget, None);
        assert_eq!(u.key_alias, None); // empty string treated as absent
        assert!(u.models.is_empty());
    }

    #[test]
    fn aggregates_daily_activity_across_days_and_models() {
        // two days, model breakdown; a duplicate date (as row-based paging can produce) must merge
        let results: Vec<serde_json::Value> = serde_json::from_str(
            r#"[
              {"date":"2026-11-24","metrics":{"spend":2.0,"total_tokens":100,"api_requests":5},
               "breakdown":{"models":{"gw/godev1":{"metrics":{"spend":1.5,"total_tokens":80,"api_requests":4}},
                                      "gw/gothink1":{"metrics":{"spend":0.5,"total_tokens":20,"api_requests":1}}}}},
              {"date":"2026-11-25","metrics":{"spend":3.0,"total_tokens":200,"api_requests":7},
               "breakdown":{"models":{"gw/godev1":{"metrics":{"spend":3.0,"total_tokens":200,"api_requests":7}}}}},
              {"date":"2026-11-25","metrics":{"spend":1.0,"total_tokens":50,"api_requests":2},
               "breakdown":{"models":{"gw/godev1":{"metrics":{"spend":1.0,"total_tokens":50,"api_requests":2}}}}}
            ]"#,
        )
        .unwrap();
        let mut total = ActivityMetrics::default();
        let mut per_day = Vec::new();
        let mut per_model = std::collections::HashMap::new();
        fold_results(&results, &mut total, &mut per_day, &mut per_model);
        let a = finalize_activity(total, per_day, per_model);
        assert_eq!(a.total.spend, 6.0);
        assert_eq!(a.total.total_tokens, 350);
        assert_eq!(a.total.requests, 14);
        // the two 11-25 rows merged into one day
        assert_eq!(a.daily.len(), 2);
        assert_eq!(a.daily[0].date, "2026-11-24");
        assert_eq!(a.daily[1].date, "2026-11-25");
        assert_eq!(a.daily[1].spend, 4.0);
        // models sorted by spend desc: godev1 (5.5) before gothink1 (0.5)
        assert_eq!(a.models[0].model, "gw/godev1");
        assert_eq!(a.models[0].spend, 5.5);
        assert_eq!(a.models[1].model, "gw/gothink1");
    }

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

    #[test]
    fn reads_tool_calls_and_plain_replies() {
        let v: serde_json::Value = serde_json::from_str(
            r#"{"choices":[{"message":{"content":null,"tool_calls":[
                {"id":"c1","type":"function","function":{"name":"search_documents","arguments":"{\"queries\":[\"PS_X\"]}"}},
                {"id":"c2","type":"function","function":{"name":"read_around","arguments":"{\"source\":2}"}}
            ]}}]}"#,
        )
        .unwrap();
        let r = parse_tool_reply(&v);
        assert_eq!(r.content, "");
        assert_eq!(r.tool_calls.len(), 2);
        assert_eq!(r.tool_calls[0].name, "search_documents");
        assert_eq!(r.tool_calls[0].arguments, r#"{"queries":["PS_X"]}"#);
        assert_eq!(r.tool_calls[1].id, "c2");
        // no tool_calls → just content, empty list
        let v2: serde_json::Value = serde_json::from_str(r#"{"choices":[{"message":{"content":"พอแล้ว"}}]}"#).unwrap();
        let r2 = parse_tool_reply(&v2);
        assert_eq!(r2.content, "พอแล้ว");
        assert!(r2.tool_calls.is_empty());
    }
}
