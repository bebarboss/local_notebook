//! Paths, settings, gateway config and models.json (no secrets in any of these files).

use crate::errors::{AppError, AppResult};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::{Path, PathBuf};

pub const GROUP_NAME: &str = "Gosoft Agent Gateway";
pub const DEFAULT_GATEWAY_URL: &str = "https://agent-gateway.gosoft.co.th";
pub const API_TYPE: &str = "Chat Completions";
pub const MODEL_IDS: [&str; 6] = ["godev1", "godev2", "godev3", "gothink1", "gothink2", "gothink3"];

/// `%APPDATA%\DocAsk` (override with DOCASK_DATA_DIR for tests/portable use).
pub fn data_dir() -> PathBuf {
    if let Ok(p) = std::env::var("DOCASK_DATA_DIR") {
        return PathBuf::from(p);
    }
    dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("DocAsk")
}

pub fn read_json<T: for<'de> Deserialize<'de>>(path: &Path) -> AppResult<Option<T>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(serde_json::from_slice(&bytes)?)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Atomic write (tmp + rename) so a crash never leaves half a config file.
pub fn write_json<T: Serialize>(path: &Path, value: &T) -> AppResult<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(value)?)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

// ---------------------------------------------------------------- settings

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Settings {
    pub theme: String,
    pub user_name: String,
    pub setup_done: bool,
    pub history_turns: u32,
    pub auto_delete_days: u32,
    pub top_k: u32,
    pub threshold: f32,
    pub chunk_size: usize,
    pub chunk_overlap: usize,
    pub embedding_model: String,
    pub last_service_id: Option<String>,
    /// latest.json URL for in-app updates (empty = updates off)
    pub update_url: String,
    pub auto_check_update: bool,
    /// OCR pictures, scanned PDF pages and image files
    pub ocr_enabled: bool,
    /// keep pictures from documents and show them with answers
    pub keep_images: bool,
    /// re-order search hits with the reranker model (when it is downloaded)
    pub rerank: bool,
    /// also search by exact words (trigram keyword index) and fuse with vector search
    pub hybrid: bool,
    /// how hard indexing may work the CPU: "light" | "normal" | "fast"
    pub index_load: String,
    /// UI font: "looped" (Thai with loops, easiest to read) | "sarabun" | "plex" | "system"
    pub font: String,
    /// text size: "s" | "m" | "l" | "xl"
    pub text_size: String,
    /// when Windows starts DocAsk at sign-in, stay in the tray instead of opening the window
    pub start_hidden: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            theme: "system".into(),
            user_name: std::env::var("USERNAME").unwrap_or_default(),
            setup_done: false,
            history_turns: 6,
            auto_delete_days: 0,
            top_k: 5,
            threshold: 0.3,
            chunk_size: 1000,
            chunk_overlap: 150,
            // "bge-m3" on this PC, or "server:<model>" / "gateway:<model>" (Settings › Embedding)
            embedding_model: crate::embed::ModelId::BgeM3.as_str().into(),
            last_service_id: None,
            update_url: crate::updater::DEFAULT_UPDATE_URL.to_string(),
            auto_check_update: true,
            ocr_enabled: true,
            keep_images: true,
            rerank: true,
            hybrid: true,
            index_load: "normal".into(),
            font: "looped".into(),
            text_size: "m".into(),
            start_hidden: true,
        }
    }
}

impl Settings {
    pub fn path() -> PathBuf {
        data_dir().join("settings.json")
    }
    pub fn load() -> Settings {
        read_json::<Settings>(&Self::path()).ok().flatten().unwrap_or_default().sanitize()
    }
    pub fn save(&self) -> AppResult<()> {
        write_json(&Self::path(), self)
    }
    pub fn sanitize(mut self) -> Self {
        if !matches!(self.theme.as_str(), "light" | "dark" | "system") {
            self.theme = "system".into();
        }
        self.history_turns = self.history_turns.min(20);
        self.top_k = self.top_k.clamp(1, 30);
        self.threshold = self.threshold.clamp(0.0, 1.0);
        self.chunk_size = self.chunk_size.clamp(300, 4000);
        self.chunk_overlap = self.chunk_overlap.min(self.chunk_size / 2);
        // v0.8.0 had no local models and could save an empty choice
        // bge-m3 is the only local model since v0.12.0 (e5 indexes are redone once it is there)
        let m = self.embedding_model.trim();
        let remote = (m.starts_with(crate::gateway_embed::PREFIX) && m != crate::gateway_embed::PREFIX)
            || (m.starts_with(crate::embed_server::PREFIX) && m != crate::embed_server::PREFIX);
        if !remote {
            self.embedding_model = Self::default().embedding_model;
        }
        if !matches!(self.font.as_str(), "looped" | "sarabun" | "plex" | "system") {
            self.font = "looped".into();
        }
        if !matches!(self.text_size.as_str(), "s" | "m" | "l" | "xl") {
            self.text_size = "m".into();
        }
        if !matches!(self.index_load.as_str(), "light" | "normal" | "fast") {
            self.index_load = "normal".into();
        }
        if !matches!(self.auto_delete_days, 0 | 30 | 90) {
            self.auto_delete_days = 0;
        }
        self.user_name = self.user_name.chars().take(60).collect();
        self.update_url = self.update_url.trim().to_string();
        if !self.update_url.is_empty() && !self.update_url.starts_with("https://") {
            self.update_url.clear();
        }
        self
    }
}

// ---------------------------------------------------------------- gateway

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct GatewayConfig {
    pub url: String,
    pub last_test_at: Option<i64>,
    pub last_test_passed: Option<u32>,
    pub last_test_total: Option<u32>,
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self { url: DEFAULT_GATEWAY_URL.into(), last_test_at: None, last_test_passed: None, last_test_total: None }
    }
}

impl GatewayConfig {
    pub fn path() -> PathBuf {
        data_dir().join("gateway.json")
    }
    pub fn load() -> Self {
        read_json(&Self::path()).ok().flatten().unwrap_or_default()
    }
    pub fn save(&self) -> AppResult<()> {
        write_json(&Self::path(), self)
    }
    pub fn base(&self) -> String {
        normalize_base(&self.url)
    }
}

/// Accepts `https://host`, `https://host/` or `https://host/v1` and returns `https://host`.
pub fn normalize_base(url: &str) -> String {
    let mut u = url.trim().trim_end_matches('/').to_string();
    if u.ends_with("/v1") {
        u.truncate(u.len() - 3);
    }
    u
}

pub fn validate_gateway_url(url: &str) -> AppResult<String> {
    let u = normalize_base(url);
    let ok = u.starts_with("https://") && u.len() > "https://".len() && !u.contains(char::is_whitespace);
    if !ok {
        return Err(AppError::new("invalid_url", "Gateway URL ต้องขึ้นต้นด้วย https:// และไม่มีช่องว่าง"));
    }
    Ok(u)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInfo {
    pub group_name: String,
    pub display_name: String,
    pub url: String,
    pub default_url: String,
    pub api_type: String,
    pub has_key: bool,
    pub key_last4: Option<String>,
    pub last_test_at: Option<i64>,
    pub last_test_passed: Option<u32>,
    pub last_test_total: Option<u32>,
}

// ---------------------------------------------------------------- models

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "camelCase", default)]
pub struct ModelConfig {
    pub id: String,
    pub name: String,
    pub url: String,
    pub tool_calling: bool,
    pub vision: bool,
    pub thinking: bool,
    pub context_window: u64,
    pub max_output_tokens: u64,
    pub supports_reasoning_effort: Vec<String>,
    pub reasoning_effort_format: String,
    pub enabled: bool,
    pub hidden: bool,
}

impl Default for ModelConfig {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            url: format!("{DEFAULT_GATEWAY_URL}/v1/chat/completions"),
            tool_calling: true,
            vision: false,
            thinking: true,
            context_window: 1_000_000,
            max_output_tokens: 64_000,
            supports_reasoning_effort: vec!["low".into(), "medium".into(), "high".into()],
            reasoning_effort_format: "chat-completions".into(),
            enabled: true,
            hidden: false,
        }
    }
}

pub fn chat_url(base: &str) -> String {
    format!("{}/v1/chat/completions", normalize_base(base))
}

pub fn default_models(base: &str) -> Vec<ModelConfig> {
    MODEL_IDS
        .iter()
        .map(|id| ModelConfig { id: id.to_string(), name: id.to_string(), url: chat_url(base), ..Default::default() })
        .collect()
}

pub fn models_path() -> PathBuf {
    data_dir().join("models.json")
}

pub fn load_models(base: &str) -> Vec<ModelConfig> {
    match read_json::<Vec<ModelConfig>>(&models_path()) {
        Ok(Some(m)) if !m.is_empty() => m,
        _ => default_models(base),
    }
}

pub fn save_models(models: &[ModelConfig]) -> AppResult<()> {
    write_json(&models_path(), &models)
}

/// Point every model that used the old gateway at the new one.
pub fn rebase_models(models: &mut [ModelConfig], old_base: &str, new_base: &str) {
    let old = normalize_base(old_base);
    for m in models.iter_mut() {
        if m.url.starts_with(&old) || m.url.is_empty() {
            m.url = chat_url(new_base);
        }
    }
}

/// Read models from VS Code's `chatLanguageModels.json`.
/// The `apiKey` field (a `${input:chat.lm.secret.…}` reference) is skipped and never read.
pub fn import_vscode_models(path: &Path, base: &str) -> AppResult<Vec<ModelConfig>> {
    let bytes = std::fs::read(path)?;
    let text = String::from_utf8_lossy(&bytes);
    let v: Value = serde_json::from_str(strip_jsonc(&text).as_str())?;
    let mut out = Vec::new();
    collect_models(&v, base, &mut out);
    out.dedup_by(|a, b| a.id == b.id);
    if out.is_empty() {
        return Err(AppError::new("import_empty", "ไม่พบรายการ models ในไฟล์นี้"));
    }
    Ok(out)
}

fn collect_models(v: &Value, base: &str, out: &mut Vec<ModelConfig>) {
    match v {
        Value::Object(map) => {
            for (k, child) in map {
                if k.eq_ignore_ascii_case("apikey") {
                    continue; // secret reference — do not read
                }
                if k == "models" {
                    if let Value::Array(items) = child {
                        for it in items {
                            if let Some(m) = model_from_value(it, base) {
                                out.push(m);
                            }
                        }
                        continue;
                    }
                }
                collect_models(child, base, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|i| collect_models(i, base, out)),
        _ => {}
    }
}

fn model_from_value(v: &Value, base: &str) -> Option<ModelConfig> {
    let o = v.as_object()?;
    let id = o.get("id").and_then(Value::as_str)?.trim().to_string();
    if id.is_empty() {
        return None;
    }
    let d = ModelConfig::default();
    let b = |k: &str, def: bool| o.get(k).and_then(Value::as_bool).unwrap_or(def);
    let n = |keys: &[&str], def: u64| keys.iter().find_map(|k| o.get(*k).and_then(Value::as_u64)).unwrap_or(def);
    let efforts = o
        .get("supportsReasoningEffort")
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect::<Vec<_>>())
        .filter(|a| !a.is_empty())
        .unwrap_or(d.supports_reasoning_effort.clone());
    Some(ModelConfig {
        name: o.get("name").and_then(Value::as_str).unwrap_or(&id).to_string(),
        url: o.get("url").and_then(Value::as_str).map(str::to_string).unwrap_or_else(|| chat_url(base)),
        tool_calling: b("toolCalling", d.tool_calling),
        vision: b("vision", d.vision),
        thinking: b("thinking", d.thinking),
        context_window: n(&["contextWindow", "maxInputTokens"], d.context_window),
        max_output_tokens: n(&["maxOutputTokens"], d.max_output_tokens),
        supports_reasoning_effort: efforts,
        reasoning_effort_format: o
            .get("reasoningEffortFormat")
            .and_then(Value::as_str)
            .unwrap_or(&d.reasoning_effort_format)
            .to_string(),
        enabled: true,
        hidden: false,
        id,
    })
}

/// VS Code JSON files may contain // and /* */ comments and trailing commas.
fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    let mut in_str = false;
    while let Some(c) = chars.next() {
        if in_str {
            out.push(c);
            if c == '\\' {
                if let Some(n) = chars.next() {
                    out.push(n);
                }
            } else if c == '"' {
                in_str = false;
            }
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
            }
            '/' if chars.peek() == Some(&'/') => {
                for n in chars.by_ref() {
                    if n == '\n' {
                        out.push('\n');
                        break;
                    }
                }
            }
            '/' if chars.peek() == Some(&'*') => {
                chars.next();
                let mut prev = ' ';
                for n in chars.by_ref() {
                    if prev == '*' && n == '/' {
                        break;
                    }
                    prev = n;
                }
            }
            _ => out.push(c),
        }
    }
    // trailing commas
    let re = regex::Regex::new(r",(\s*[}\]])").unwrap();
    re.replace_all(&out, "$1").into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bge_m3_is_the_only_local_model() {
        let with = |m: &str| Settings { embedding_model: m.into(), ..Settings::default() }.sanitize().embedding_model;
        assert_eq!(Settings::default().embedding_model, "bge-m3");
        assert_eq!(with("multilingual-e5-small"), "bge-m3");
        assert_eq!(with("multilingual-e5-base"), "bge-m3");
        assert_eq!(with(""), "bge-m3");
        assert_eq!(with("server:"), "bge-m3");
        assert_eq!(with("server:bge-m3"), "server:bge-m3");
        assert_eq!(with("gateway:text-embedding-3-small"), "gateway:text-embedding-3-small");
    }

    #[test]
    fn defaults_match_guide() {
        let m = default_models(DEFAULT_GATEWAY_URL);
        assert_eq!(m.len(), 6);
        assert_eq!(m[0].url, "https://agent-gateway.gosoft.co.th/v1/chat/completions");
        assert!(m.iter().all(|x| x.context_window == 1_000_000 && x.max_output_tokens == 64_000 && x.thinking && !x.vision));
    }

    #[test]
    fn normalizes_base() {
        assert_eq!(normalize_base("https://a.b/v1/"), "https://a.b");
        assert!(validate_gateway_url("http://a.b").is_err());
    }

    #[test]
    fn import_skips_api_key() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("chatLanguageModels.json");
        std::fs::write(
            &p,
            r#"// comment
            [{ "name": "Gosoft Agent Gateway", "apiKey": "${input:chat.lm.secret.abcd}",
               "models": [ {"id":"godev1","name":"godev1","maxOutputTokens":64000,},
                           {"id":"gothink2","contextWindow":1000000} ] }]"#,
        )
        .unwrap();
        let m = import_vscode_models(&p, DEFAULT_GATEWAY_URL).unwrap();
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].id, "gothink2");
        let dump = serde_json::to_string(&m).unwrap();
        assert!(!dump.contains("secret"));
    }
}
