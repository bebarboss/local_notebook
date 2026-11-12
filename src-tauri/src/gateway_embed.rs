//! Embeddings from the company Agent Gateway (LiteLLM, OpenAI-compatible `/v1/embeddings`).
//!
//! Opt-in (Settings › Embedding): every chunk's text is sent to the Gateway when indexing,
//! and each question when searching. The Bearer key is read from the Windows credential
//! store here and attached by Rust only — it never reaches the frontend.

use crate::errors::{AppError, AppResult};
use crate::llm::CLIENT;
use crate::redact::redact_secret;
use serde::Serialize;
use std::time::Duration;

/// `settings.embedding_model` value prefix for a Gateway model.
pub const PREFIX: &str = "gateway:";

/// Inputs per request (the indexer already sends batches of 32).
const MAX_INPUTS: usize = 32;
const RETRIES: u32 = 4;

#[derive(Serialize)]
struct Body<'a> {
    model: &'a str,
    input: &'a [String],
    encoding_format: &'static str,
    /// only for DocAsk's own embedding server ("query" | "passage"); never sent to the Gateway
    #[serde(skip_serializing_if = "Option::is_none")]
    input_type: Option<&'a str>,
}

/// One OpenAI-style `/v1/embeddings` endpoint: where, with which key, and how patient.
pub(crate) struct Endpoint<'a> {
    pub url: &'a str,
    pub key: &'a str,
    pub input_type: Option<&'a str>,
    pub retries: u32,
    /// whole request, including the model's work
    pub timeout: Duration,
    /// error message prefix, e.g. "Gateway embedding"
    pub label: &'a str,
}

fn base() -> String {
    crate::config::GatewayConfig::load().base()
}

/// Embed `texts` with `model`, L2-normalised, in input order.
pub async fn embed(model: &str, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
    let key = crate::secrets::require_key()?;
    let url = format!("{}/v1/embeddings", base());
    let mut out = Vec::with_capacity(texts.len());
    for part in texts.chunks(MAX_INPUTS) {
        let ep = Endpoint { url: &url, key: &key, input_type: None, retries: RETRIES, timeout: Duration::from_secs(120), label: "Gateway embedding" };
        out.extend(post(&ep, model, part).await?);
    }
    Ok(out)
}

pub(crate) async fn post(ep: &Endpoint<'_>, model: &str, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
    let (url, key) = (ep.url, ep.key);
    let body = Body { model, input: texts, encoding_format: "float", input_type: ep.input_type };
    let mut attempt = 0;
    loop {
        let sent = CLIENT.post(url).bearer_auth(key).timeout(ep.timeout).json(&body).send().await;
        let retry_in = Duration::from_secs(2u64.pow(attempt + 1));
        let resp = match sent {
            Ok(r) => r,
            Err(e) if attempt < ep.retries && (e.is_timeout() || e.is_connect()) => {
                attempt += 1;
                tokio::time::sleep(retry_in).await;
                continue;
            }
            Err(e) => return Err(AppError::from(e)),
        };
        let status = resp.status();
        if status.as_u16() == 429 || status.is_server_error() {
            if attempt < ep.retries {
                attempt += 1;
                tokio::time::sleep(retry_in).await;
                continue;
            }
        }
        if !status.is_success() {
            let text = resp.text().await.unwrap_or_default();
            let mut e = AppError::from_status(status.as_u16(), &text);
            e.message = redact_secret(&format!("{}: {}", ep.label, e.message), Some(key));
            return Err(e);
        }
        let v: serde_json::Value = resp.json().await.map_err(AppError::from)?;
        return parse(&v, texts.len());
    }
}

/// `{"data":[{"index":0,"embedding":[...]}, ...]}` → vectors in input order, normalised.
pub(crate) fn parse(v: &serde_json::Value, n: usize) -> AppResult<Vec<Vec<f32>>> {
    let bad = || AppError::new("embedding", "Gateway ตอบกลับ embedding ในรูปแบบที่ไม่รู้จัก");
    let data = v.get("data").and_then(|d| d.as_array()).ok_or_else(bad)?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; n];
    for (pos, item) in data.iter().enumerate() {
        let i = item.get("index").and_then(|i| i.as_u64()).map(|i| i as usize).unwrap_or(pos);
        let emb: Vec<f32> = item
            .get("embedding")
            .and_then(|e| e.as_array())
            .ok_or_else(bad)?
            .iter()
            .map(|x| x.as_f64().unwrap_or(0.0) as f32)
            .collect();
        if i < n {
            out[i] = Some(normalize(emb));
        }
    }
    out.into_iter().map(|e| e.ok_or_else(bad)).collect()
}

pub(crate) fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Embedding models the Gateway offers: `/public/model_hub` (no key, has `mode`), else
/// `/v1/models` with the key, keeping names that look like embedding models.
pub async fn list_models() -> AppResult<Vec<String>> {
    let base = base();
    if let Ok(r) = CLIENT.get(format!("{base}/public/model_hub")).timeout(Duration::from_secs(15)).send().await {
        if r.status().is_success() {
            if let Ok(v) = r.json::<serde_json::Value>().await {
                let found = from_model_hub(&v);
                if !found.is_empty() {
                    return Ok(found);
                }
            }
        }
    }
    let key = crate::secrets::require_key()?;
    let all = crate::llm::list_models(&base, &key).await?;
    Ok(all.into_iter().filter(|m| looks_like_embedding(m)).collect())
}

fn from_model_hub(v: &serde_json::Value) -> Vec<String> {
    let items = v.as_array().or_else(|| v.get("data").and_then(|d| d.as_array()));
    let mut out: Vec<String> = items
        .into_iter()
        .flatten()
        .filter(|m| m.get("mode").and_then(|x| x.as_str()) == Some("embedding"))
        .filter_map(|m| {
            ["model_group", "model_name", "id", "name"]
                .iter()
                .find_map(|k| m.get(*k).and_then(|x| x.as_str()))
                .map(str::to_string)
        })
        .collect();
    out.sort();
    out.dedup();
    out
}

fn looks_like_embedding(name: &str) -> bool {
    let n = name.to_lowercase();
    ["embed", "bge", "e5", "gte", "minilm", "mpnet"].iter().any(|k| n.contains(k))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    #[test]
    fn parses_in_index_order_and_normalises() {
        let v = serde_json::json!({"data":[
            {"index":1,"embedding":[0.0, 2.0]},
            {"index":0,"embedding":[3.0, 4.0]}
        ]});
        let out = parse(&v, 2).unwrap();
        assert_eq!(out[0], vec![0.6, 0.8]);
        assert_eq!(out[1], vec![0.0, 1.0]);
        assert!(parse(&serde_json::json!({"data":[]}), 1).is_err());
    }

    /// A one-shot-per-connection HTTP server (also used by `embed_server` tests) that answers each request with the next reply.
    pub(crate) fn fake_gateway(replies: Vec<(u16, String)>) -> (String, std::thread::JoinHandle<Vec<String>>) {
        use std::io::{Read, Write};
        let l = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/v1/embeddings", l.local_addr().unwrap());
        let h = std::thread::spawn(move || {
            let mut seen = Vec::new();
            for (code, body) in replies {
                let (mut s, _) = l.accept().unwrap();
                let mut buf = vec![0u8; 65536];
                let mut req = String::new();
                // read headers + body (Content-Length)
                loop {
                    let n = s.read(&mut buf).unwrap();
                    req.push_str(&String::from_utf8_lossy(&buf[..n]));
                    if let Some(end) = req.find("\r\n\r\n") {
                        let len = req[..end]
                            .lines()
                            .find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap()))
                            .unwrap_or(0);
                        if req.len() >= end + 4 + len {
                            break;
                        }
                    }
                }
                seen.push(req);
                let resp = format!("HTTP/1.1 {code} X\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                s.write_all(resp.as_bytes()).unwrap();
            }
            seen
        });
        (url, h)
    }

    #[test]
    fn calls_the_gateway_and_retries_when_busy() {
        let ok = r#"{"object":"list","data":[{"object":"embedding","index":0,"embedding":[1.0,0.0]},{"object":"embedding","index":1,"embedding":[0.0,3.0]}]}"#;
        let (url, server) = fake_gateway(vec![(429, r#"{"error":{"message":"rate limit"}}"#.into()), (200, ok.into())]);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let texts = vec!["ลาพักร้อน".to_string(), "รหัสผ่าน".to_string()];
        let v = rt.block_on(post(&Endpoint { url: &url, key: "sk-test-key", input_type: None, retries: RETRIES, timeout: Duration::from_secs(30), label: "t" }, "bge-m3", &texts)).unwrap();
        assert_eq!(v, vec![vec![1.0, 0.0], vec![0.0, 1.0]]);
        let seen = server.join().unwrap();
        assert_eq!(seen.len(), 2, "429 must be retried");
        assert!(seen[1].starts_with("POST /v1/embeddings"));
        assert!(seen[1].contains("authorization: Bearer sk-test-key") || seen[1].contains("Authorization: Bearer sk-test-key"));
        assert!(seen[1].contains(r#""model":"bge-m3""#) && seen[1].contains("ลาพักร้อน"));
        assert!(!seen[1].contains("input_type"), "the Gateway never gets the extra field");
    }

    #[test]
    fn gateway_errors_never_show_the_key() {
        let (url, server) = fake_gateway(vec![(400, r#"{"error":{"message":"model not found for key sk-test-key"}}"#.into())]);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let e = rt.block_on(post(&Endpoint { url: &url, key: "sk-test-key", input_type: None, retries: RETRIES, timeout: Duration::from_secs(30), label: "t" }, "nope", &["x".to_string()])).unwrap_err();
        assert!(!e.message.contains("sk-test-key"), "{}", e.message);
        server.join().unwrap();
    }

    #[test]
    fn picks_embedding_models() {
        let hub = serde_json::json!([
            {"model_group":"godev1","mode":"chat"},
            {"model_group":"text-embedding-3-small","mode":"embedding"},
            {"model_group":"bge-m3","mode":"embedding"}
        ]);
        assert_eq!(from_model_hub(&hub), vec!["bge-m3", "text-embedding-3-small"]);
        assert!(looks_like_embedding("cohere-embed-multilingual"));
        assert!(!looks_like_embedding("gothink1"));
    }
}
