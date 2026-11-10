//! Error type returned to the frontend. Every message passes through `redact()`.

use crate::redact::redact;
use serde::Serialize;
use std::fmt;

pub const MSG_UNREACHABLE: &str = "ต่อ Gateway ไม่ได้ กรุณาต่อ VPN บริษัทแล้วลองใหม่";
pub const MSG_401: &str = "Virtual Key หมดอายุ ผิด หรือถูก revoke — กด Update API Key";
pub const MSG_403: &str = "ไม่มีสิทธิ์ใช้โมเดลนี้ ตรวจสิทธิ์ Model/Policy และชนิดของ Key กับ Admin";
pub const MSG_404: &str = "Gateway URL หรือชื่อโมเดล (Public Model ID) ไม่ถูกต้อง";
pub const MSG_429: &str = "เกินโควตา RPM, TPM หรือ budget — รอสักครู่หรือติดต่อ Admin";

#[derive(Debug, Clone, Serialize)]
pub struct AppError {
    pub code: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
}

pub type AppResult<T> = Result<T, AppError>;

impl AppError {
    pub fn new(code: &str, message: impl AsRef<str>) -> Self {
        Self { code: code.to_string(), message: redact(message.as_ref()), status: None }
    }

    pub fn unreachable() -> Self {
        Self::new("gateway_unreachable", MSG_UNREACHABLE)
    }

    pub fn no_key() -> Self {
        Self::new("no_key", "ยังไม่ได้ใส่ Virtual Key — ไปที่ ตั้งค่า › Gateway และ Key")
    }

    /// Map an HTTP status from the Gateway to the texts in SPEC §3.3.
    pub fn from_status(status: u16, server_body: &str) -> Self {
        let (code, msg) = match status {
            401 => ("http_401", MSG_401.to_string()),
            403 => ("http_403", MSG_403.to_string()),
            404 => ("http_404", MSG_404.to_string()),
            429 => ("http_429", MSG_429.to_string()),
            _ => ("http_error", format!("HTTP {status}: {}", server_message(server_body))),
        };
        let mut e = Self::new(code, msg);
        e.status = Some(status);
        e
    }

    pub fn is_unreachable(&self) -> bool {
        self.code == "gateway_unreachable"
    }
}

/// Pull `error.message` out of an OpenAI/LiteLLM error body, else a trimmed body.
fn server_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(m) = v.pointer("/error/message").and_then(|m| m.as_str()) {
            return truncate(m, 300);
        }
        if let Some(m) = v.get("detail").and_then(|m| m.as_str()) {
            return truncate(m, 300);
        }
    }
    truncate(body.trim(), 300)
}

fn truncate(s: &str, n: usize) -> String {
    let mut out: String = s.chars().take(n).collect();
    if s.chars().count() > n {
        out.push('…');
    }
    out
}

impl fmt::Display for AppError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for AppError {}

impl From<std::io::Error> for AppError {
    fn from(e: std::io::Error) -> Self {
        Self::new("io", format!("อ่าน/เขียนไฟล์ไม่สำเร็จ: {e}"))
    }
}

impl From<rusqlite::Error> for AppError {
    fn from(e: rusqlite::Error) -> Self {
        Self::new("db", format!("ฐานข้อมูลผิดพลาด: {e}"))
    }
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        Self::new("json", format!("รูปแบบ JSON ไม่ถูกต้อง: {e}"))
    }
}

impl From<keyring::Error> for AppError {
    fn from(e: keyring::Error) -> Self {
        // keyring errors never contain the secret itself, but redact anyway
        Self::new("keyring", format!("Windows Credential Manager ผิดพลาด: {e}"))
    }
}

impl From<reqwest::Error> for AppError {
    fn from(e: reqwest::Error) -> Self {
        if e.is_connect() || e.is_timeout() || is_dns(&e) {
            return Self::unreachable();
        }
        if let Some(s) = e.status() {
            return Self::from_status(s.as_u16(), "");
        }
        // reqwest's Display may include the URL — never the headers.
        Self::new("network", format!("เครือข่ายผิดพลาด: {e}"))
    }
}

fn is_dns(e: &reqwest::Error) -> bool {
    let mut src: Option<&dyn std::error::Error> = std::error::Error::source(e);
    while let Some(s) = src {
        let t = s.to_string().to_lowercase();
        if t.contains("dns") || t.contains("resolve") || t.contains("no such host") || t.contains("refused") {
            return true;
        }
        src = s.source();
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_messages_follow_spec() {
        assert_eq!(AppError::from_status(401, "").message, MSG_401);
        assert_eq!(AppError::from_status(403, "").message, MSG_403);
        assert_eq!(AppError::from_status(404, "").message, MSG_404);
        assert_eq!(AppError::from_status(429, "").message, MSG_429);
    }

    #[test]
    fn other_status_is_redacted() {
        let e = AppError::from_status(500, r#"{"error":{"message":"bad key sk-live12345678 boom"}}"#);
        assert!(e.message.starts_with("HTTP 500"));
        assert!(e.message.contains("boom"));
        assert!(!e.message.contains("live12345678"));
    }
}
