//! Shared bits for the live "activity" steps that the web-research and coding loops emit, plus a
//! small tool-argument parser they reuse. (The old document "gather"/agentic loop was removed — docs
//! mode now answers on the first retrieval, no extra Gateway round before the reply.)

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One line in the live activity list ("กำลังทำอะไร"), shown under a chat's answer.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Step {
    /// e.g. "websearch" | "webread" | "codeedit" | "codewrite" | "done" | "error"
    pub kind: String,
    /// the search terms, a path/host, or an error code — never passage/file text
    pub detail: String,
    /// new items this step added
    pub found: usize,
}

/// Pull search terms out of a `web_search` tool call's arguments: `{"queries":[...]}` (also tolerates
/// a single `{"query":"..."}`). At most three, trimmed, non-empty.
pub fn parse_search_queries(args: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(args) else { return Vec::new() };
    let mut out: Vec<String> = Vec::new();
    if let Some(arr) = v.get("queries").and_then(|q| q.as_array()) {
        out.extend(arr.iter().filter_map(|x| x.as_str()).map(|s| s.trim().to_string()));
    }
    if let Some(q) = v.get("query").and_then(|x| x.as_str()) {
        out.push(q.trim().to_string());
    }
    out.retain(|s| !s.is_empty());
    out.truncate(3);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_arguments() {
        assert_eq!(parse_search_queries(r#"{"queries":["A"," B ",""]}"#), vec!["A", "B"]);
        assert_eq!(parse_search_queries(r#"{"query":"solo"}"#), vec!["solo"]);
        assert!(parse_search_queries("broken").is_empty());
        assert_eq!(parse_search_queries(r#"{"queries":["a","b","c","d"]}"#).len(), 3);
    }
}
