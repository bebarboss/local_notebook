//! Virtual Key storage — Windows Credential Manager only (via `keyring`).
//! The key never touches a file, a log line, the clipboard or the frontend.

use crate::errors::{AppError, AppResult};
use crate::redact::last4;

pub const SERVICE: &str = "DocAsk/Gosoft Agent Gateway";
const USER: &str = "virtual-key";

fn entry() -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE, USER)?)
}

pub fn validate_format(key: &str) -> AppResult<()> {
    let k = key.trim();
    if !k.starts_with("sk-") {
        return Err(AppError::new("key_format", "Key ต้องขึ้นต้นด้วย sk-"));
    }
    if k.chars().any(char::is_whitespace) || k.len() < 12 {
        return Err(AppError::new("key_format", "รูปแบบ Key ไม่ถูกต้อง (ห้ามมีช่องว่าง)"));
    }
    Ok(())
}

pub fn set_key(key: &str) -> AppResult<()> {
    validate_format(key)?;
    entry()?.set_password(key.trim())?;
    Ok(())
}

pub fn get_key() -> AppResult<Option<String>> {
    match entry()?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn require_key() -> AppResult<String> {
    get_key()?.ok_or_else(AppError::no_key)
}

pub fn delete_key() -> AppResult<()> {
    match entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// (has_key, last 4 chars) — the only information the UI receives.
pub fn key_status() -> (bool, Option<String>) {
    match get_key() {
        Ok(Some(k)) => (true, Some(last4(&k))),
        _ => (false, None),
    }
}

// ------------------------------------------------- embedding server token (home server)

const SERVER_SERVICE: &str = "DocAsk/Embedding server";
const SERVER_USER: &str = "token";

fn server_entry() -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(SERVER_SERVICE, SERVER_USER)?)
}

pub fn validate_server_token(token: &str) -> AppResult<()> {
    let t = token.trim();
    if t.len() < 16 || t.chars().any(char::is_whitespace) {
        return Err(AppError::new("key_format", "Token ต้องยาวอย่างน้อย 16 ตัวและไม่มีช่องว่าง"));
    }
    Ok(())
}

pub fn set_server_token(token: &str) -> AppResult<()> {
    validate_server_token(token)?;
    server_entry()?.set_password(token.trim())?;
    Ok(())
}

pub fn get_server_token() -> AppResult<Option<String>> {
    match server_entry()?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete_server_token() -> AppResult<()> {
    match server_entry()?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}

/// (has_token, last 4 chars) — the only information the UI receives.
pub fn server_token_status() -> (bool, Option<String>) {
    match get_server_token() {
        Ok(Some(k)) => (true, Some(last4(&k))),
        _ => (false, None),
    }
}

// ------------------------------------------------- share keys (one per shared service)

const SERVICE_KEY: &str = "DocAsk/Service key";

fn service_entry(service_id: &str) -> AppResult<keyring::Entry> {
    Ok(keyring::Entry::new(SERVICE_KEY, service_id)?)
}

pub fn set_service_key(service_id: &str, key: &str) -> AppResult<()> {
    validate_server_token(key)?;
    service_entry(service_id)?.set_password(key.trim())?;
    Ok(())
}

pub fn get_service_key(service_id: &str) -> AppResult<Option<String>> {
    match service_entry(service_id)?.get_password() {
        Ok(k) => Ok(Some(k)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

pub fn delete_service_key(service_id: &str) -> AppResult<()> {
    match service_entry(service_id)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(e) => Err(e.into()),
    }
}
