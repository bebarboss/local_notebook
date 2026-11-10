//! "Reply exactly: GOSOFT LITELLM OK" connection test, one model at a time.

use crate::config::ModelConfig;
use crate::llm::{self, ChatMessage};
use serde::Serialize;
use std::time::Instant;

pub const PROMPT: &str = "Reply exactly: GOSOFT LITELLM OK";
pub const EXPECTED: &str = "GOSOFT LITELLM OK";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TestResult {
    pub model: String,
    pub ok: bool,
    pub reply: Option<String>,
    pub error: Option<String>,
    pub status: Option<u16>,
    pub elapsed_ms: u64,
    #[serde(skip)]
    pub unreachable: bool,
}

/// Pass when the reply contains the phrase, ignoring case and surrounding whitespace.
pub fn passes(reply: &str) -> bool {
    let collapsed = reply.split_whitespace().collect::<Vec<_>>().join(" ").to_uppercase();
    collapsed.contains(EXPECTED)
}

pub async fn test_model(m: &ModelConfig, key: &str) -> TestResult {
    let messages = [ChatMessage::new("user", PROMPT)];
    let effort = m.supports_reasoning_effort.iter().any(|e| e == "low").then_some("low");
    let req = llm::Request { url: &m.url, key, model: &m.id, effort, max_tokens: m.max_output_tokens.min(2048), messages: &messages };
    let t = Instant::now();
    let r = llm::complete(&req).await;
    let elapsed_ms = t.elapsed().as_millis() as u64;
    match r {
        Ok(reply) => {
            let ok = passes(&reply);
            let shown: String = reply.trim().chars().take(120).collect();
            TestResult {
                model: m.id.clone(),
                ok,
                error: (!ok).then(|| format!("ได้คำตอบแต่ไม่ตรง: \"{shown}\"")),
                reply: Some(shown),
                status: Some(200),
                elapsed_ms,
                unreachable: false,
            }
        }
        Err(e) => TestResult {
            model: m.id.clone(),
            ok: false,
            reply: None,
            unreachable: e.is_unreachable(),
            status: e.status,
            error: Some(e.message),
            elapsed_ms,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pass_rules() {
        assert!(passes("GOSOFT LITELLM OK"));
        assert!(passes("  gosoft litellm ok \n"));
        assert!(passes("GOSOFT  LITELLM\nOK"));
        assert!(!passes("GOSOFT OK"));
    }
}
