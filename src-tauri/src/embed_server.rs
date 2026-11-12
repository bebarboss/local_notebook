//! Embeddings from the user's own server (`server/embed` in this repo: Docker on a home
//! machine, reached over Tailscale). OpenAI-style `/v1/embeddings` plus `/health`.
//!
//! Opt-in (Settings › Embedding): every chunk's text goes to that server when indexing, and
//! each question when searching. The Bearer token lives in the Windows credential store and
//! is attached here by Rust only — it never reaches the frontend, a file or a log line.

use crate::config::{data_dir, read_json, write_json};
use crate::errors::{AppError, AppResult};
use crate::gateway_embed::{post, Endpoint};
use crate::llm::CLIENT;
use serde::{Deserialize, Serialize};
use std::net::IpAddr;
use parking_lot::Mutex;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// `settings.embedding_model` value prefix for a model on the embedding server.
pub const PREFIX: &str = "server:";

/// The server accepts up to 64; the indexer sends 32 at a time anyway.
const MAX_INPUTS: usize = 32;
/// After the server failed, questions go straight to the local fallback for this long.
const DOWN_FOR: Duration = Duration::from_secs(120);

static DOWN_AT: Mutex<Option<Instant>> = Mutex::new(None);

/// The server could not be reached or is broken (502 from `tailscale serve` while the
/// container is down) — as opposed to a wrong token or model, which a fallback would hide.
pub fn is_down(e: &AppError) -> bool {
    e.code == "server_unreachable" || e.status.is_some_and(|s| s >= 500)
}

/// The server failed within the last two minutes.
pub fn recently_down() -> bool {
    DOWN_AT.lock().is_some_and(|t| t.elapsed() < DOWN_FOR)
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerConfig {
    pub url: String,
}

impl ServerConfig {
    fn path() -> PathBuf {
        data_dir().join("embed-server.json")
    }
    pub fn load() -> Self {
        read_json(&Self::path()).ok().flatten().unwrap_or_default()
    }
    pub fn save(&self) -> AppResult<()> {
        write_json(&Self::path(), self)
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub url: String,
    pub has_token: bool,
    pub token_last4: Option<String>,
}

pub fn info() -> ServerInfo {
    let (has_token, token_last4) = crate::secrets::server_token_status();
    ServerInfo { url: ServerConfig::load().url, has_token, token_last4 }
}

/// What `/health` says (no token needed).
#[derive(Debug, Clone, Deserialize)]
pub struct Health {
    pub model: String,
    pub dim: usize,
    #[serde(default)]
    pub device: String,
}

/// `https://` anywhere; plain `http://` only where the network itself is private and
/// encrypted or never leaves the machine: localhost, Tailscale addresses (100.64.0.0/10,
/// fd7a:115c:a1e0::/48) and `*.ts.net` names.
pub fn validate_url(url: &str) -> AppResult<String> {
    let bad = |m: &str| AppError::new("invalid_url", m);
    let base = crate::config::normalize_base(url);
    if base.contains(char::is_whitespace) {
        return Err(bad("URL ต้องไม่มีช่องว่าง"));
    }
    let parsed = reqwest::Url::parse(&base).map_err(|_| bad("URL ไม่ถูกต้อง เช่น https://mac-mini.xxxx.ts.net"))?;
    if !parsed.username().is_empty() || parsed.password().is_some() || parsed.query().is_some() {
        return Err(bad("URL ต้องไม่มีชื่อผู้ใช้ รหัสผ่าน หรือ ?query"));
    }
    let host = parsed.host_str().unwrap_or_default().trim_matches(['[', ']']).to_lowercase();
    if host.is_empty() {
        return Err(bad("URL ไม่มีชื่อเครื่อง"));
    }
    match parsed.scheme() {
        "https" => Ok(base),
        "http" if private_host(&host) => Ok(base),
        "http" => Err(bad("http:// ใช้ได้เฉพาะ localhost หรือที่อยู่ Tailscale (100.x.x.x, *.ts.net) — ที่อื่นต้องเป็น https://")),
        _ => Err(bad("URL ต้องขึ้นต้นด้วย https:// หรือ http://")),
    }
}

fn private_host(host: &str) -> bool {
    if host == "localhost" || host.ends_with(".ts.net") {
        return true;
    }
    match host.parse::<IpAddr>() {
        Ok(IpAddr::V4(ip)) => {
            let o = ip.octets();
            ip.is_loopback() || (o[0] == 100 && (64..128).contains(&o[1]))
        }
        Ok(IpAddr::V6(ip)) => {
            let s = ip.segments();
            ip.is_loopback() || (s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0)
        }
        Err(_) => false,
    }
}

fn unreachable(base: &str) -> AppError {
    AppError::new(
        "server_unreachable",
        format!("ติดต่อเซิร์ฟเวอร์ embedding ไม่ได้ ({base}) — เครื่องที่บ้านเปิดอยู่ไหม, Docker ทำงานอยู่ไหม, Tailscale ต่ออยู่ทั้งสองเครื่องไหม"),
    )
}

/// The base URL and token, or a message saying what is missing.
fn target() -> AppResult<(String, String)> {
    let url = ServerConfig::load().url;
    if url.trim().is_empty() {
        return Err(AppError::new("no_server", "ยังไม่ได้ตั้งเซิร์ฟเวอร์ embedding — ไปที่ ตั้งค่า › Embedding และ Index"));
    }
    let token = crate::secrets::get_server_token()?
        .ok_or_else(|| AppError::new("no_key", "ยังไม่ได้ใส่ token ของเซิร์ฟเวอร์ embedding — ไปที่ ตั้งค่า › Embedding และ Index"))?;
    Ok((url, token))
}

/// Configured well enough to try (URL + token saved) — not a network check.
pub fn configured() -> bool {
    !ServerConfig::load().url.trim().is_empty() && matches!(crate::secrets::get_server_token(), Ok(Some(_)))
}

pub async fn health(base: &str) -> AppResult<Health> {
    let r = CLIENT
        .get(format!("{base}/health"))
        .timeout(Duration::from_secs(10))
        .send()
        .await
        .map_err(|e| if e.is_connect() || e.is_timeout() { unreachable(base) } else { AppError::from(e) })?;
    if !r.status().is_success() {
        return Err(AppError::new("server", format!("เซิร์ฟเวอร์ embedding ตอบ HTTP {} ที่ /health", r.status().as_u16())));
    }
    r.json::<Health>()
        .await
        .map_err(|_| AppError::new("server", "ที่อยู่นี้ไม่ใช่เซิร์ฟเวอร์ embedding ของ DocAsk (/health ตอบไม่ตรงรูปแบบ)"))
}

/// Passages for the index (patient: retries while the home machine wakes up).
pub async fn embed_passages(model: &str, texts: &[String]) -> AppResult<Vec<Vec<f32>>> {
    embed(model, texts, "passage", 4, Duration::from_secs(120)).await
}

/// One question — fails fast (no retry) so the local fallback can answer instead.
pub async fn embed_query(model: &str, q: &str) -> AppResult<Vec<f32>> {
    Ok(embed(model, &[q.to_string()], "query", 0, Duration::from_secs(15)).await?.pop().unwrap_or_default())
}

async fn embed(model: &str, texts: &[String], kind: &str, retries: u32, timeout: Duration) -> AppResult<Vec<Vec<f32>>> {
    let (base, token) = target()?;
    let r = embed_at(&base, &token, model, texts, kind, retries, timeout).await;
    match &r {
        Ok(_) => *DOWN_AT.lock() = None,
        Err(e) if is_down(e) => *DOWN_AT.lock() = Some(Instant::now()),
        Err(_) => {}
    }
    r
}

async fn embed_at(base: &str, token: &str, model: &str, texts: &[String], kind: &str, retries: u32, timeout: Duration) -> AppResult<Vec<Vec<f32>>> {
    let url = format!("{base}/v1/embeddings");
    let ep = Endpoint { url: &url, key: token, input_type: Some(kind), retries, timeout, label: "เซิร์ฟเวอร์ embedding" };
    let mut out = Vec::with_capacity(texts.len());
    for part in texts.chunks(MAX_INPUTS) {
        let v = post(&ep, model, part).await.map_err(|e| {
            if e.is_unreachable() {
                unreachable(base)
            } else if e.status == Some(401) {
                AppError::new("http_401", "token ของเซิร์ฟเวอร์ embedding ไม่ถูกต้อง — ใส่ใหม่ที่ ตั้งค่า › Embedding และ Index")
            } else if e.status == Some(404) {
                AppError::new("http_404", format!("เซิร์ฟเวอร์ embedding ไม่มีโมเดล {model} — กดทดสอบใหม่ที่ ตั้งค่า › Embedding และ Index"))
            } else {
                e
            }
        })?;
        out.extend(v);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gateway_embed::tests::fake_gateway;

    #[test]
    fn down_means_unreachable_or_broken_not_a_wrong_token() {
        assert!(is_down(&unreachable("http://x")));
        assert!(is_down(&AppError::from_status(502, "")));
        assert!(!is_down(&AppError::from_status(401, "")));
        assert!(!is_down(&AppError::from_status(404, "")));
    }

    #[test]
    fn server_models_map_to_local_fallbacks() {
        use crate::embed::ModelId;
        assert_eq!(ModelId::from_server_model("bge-m3"), Some(ModelId::BgeM3));
        assert_eq!(ModelId::from_server_model("BAAI/bge-m3"), Some(ModelId::BgeM3));
        assert_eq!(ModelId::from_server_model("intfloat/multilingual-e5-small"), Some(ModelId::MultilingualE5Small));
        assert_eq!(ModelId::from_server_model("all-MiniLM-L6-v2"), None);
    }

    #[test]
    fn urls() {
        assert_eq!(validate_url("https://mac-mini.tail1234.ts.net/").unwrap(), "https://mac-mini.tail1234.ts.net");
        assert_eq!(validate_url("https://home.example.com/v1").unwrap(), "https://home.example.com");
        assert!(validate_url("http://100.101.102.103:8080").is_ok());
        assert!(validate_url("http://mac-mini.tail1234.ts.net:8080").is_ok());
        assert!(validate_url("http://localhost:8080").is_ok());
        assert!(validate_url("http://127.0.0.1:8080").is_ok());
        assert!(validate_url("http://[fd7a:115c:a1e0::1]:8080").is_ok());
        // plain http over the internet or a LAN would expose documents and the token
        assert!(validate_url("http://home.example.com").is_err());
        assert!(validate_url("http://192.168.1.20:8080").is_err());
        assert!(validate_url("http://100.200.1.1").is_err());
        assert!(validate_url("http://evil.ts.net.example.com").is_err());
        assert!(validate_url("https://user:pw@host").is_err());
        assert!(validate_url("ftp://host").is_err());
        assert!(validate_url("mac-mini").is_err());
    }

    #[test]
    fn sends_input_type_and_hides_the_token() {
        let ok = r#"{"data":[{"index":0,"embedding":[3.0,4.0]}]}"#;
        let (url, server) = fake_gateway(vec![(200, ok.into()), (401, r#"{"error":{"message":"invalid token"}}"#.into())]);
        let base = url.trim_end_matches("/v1/embeddings").to_string();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let v = rt.block_on(embed_at(&base, "home-token-0123456789", "bge-m3", &["ลาพักร้อน".into()], "query", 0, Duration::from_secs(30))).unwrap();
        assert_eq!(v, vec![vec![0.6, 0.8]]);
        let e = rt.block_on(embed_at(&base, "home-token-0123456789", "bge-m3", &["x".into()], "passage", 0, Duration::from_secs(30))).unwrap_err();
        assert!(!e.message.contains("home-token"), "{}", e.message);
        let seen = server.join().unwrap();
        assert!(seen[0].contains(r#""input_type":"query""#), "{}", seen[0]);
        assert!(seen[1].contains(r#""input_type":"passage""#));
    }

    /// Against a running server: DOCASK_EMBED_URL=http://127.0.0.1:8080 DOCASK_EMBED_TOKEN=…
    /// cargo test --lib real_server -- --ignored
    #[test]
    #[ignore]
    fn real_server() {
        let base = validate_url(&std::env::var("DOCASK_EMBED_URL").unwrap()).unwrap();
        let token = std::env::var("DOCASK_EMBED_TOKEN").unwrap();
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let h = rt.block_on(health(&base)).unwrap();
        let texts: Vec<String> = (0..40).map(|i| format!("ท่อนที่ {i} ของเอกสาร")).collect();
        let v = rt.block_on(embed_at(&base, &token, &h.model, &texts, "passage", 0, Duration::from_secs(30))).unwrap();
        assert_eq!(v.len(), 40, "two requests of 32 + 8");
        assert!(v.iter().all(|x| x.len() == h.dim));
        let e = rt.block_on(embed_at(&base, "wrong-token-0000000000", &h.model, &texts[..1], "query", 0, Duration::from_secs(30))).unwrap_err();
        assert!(e.message.contains("token"), "{}", e.message);
    }

    #[test]
    fn a_closed_port_says_what_to_check() {
        let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let base = format!("http://127.0.0.1:{port}");
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap();
        let e = rt.block_on(embed_at(&base, "home-token-0123456789", "bge-m3", &["x".into()], "query", 0, Duration::from_secs(30))).unwrap_err();
        assert_eq!(e.code, "server_unreachable", "{}", e.message);
        assert!(e.message.contains("Tailscale"));
    }
}
