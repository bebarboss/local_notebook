//! Removes secrets from any text that may reach the UI, logs or error reports.

use once_cell::sync::Lazy;
use regex::Regex;

static SK: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bsk-[A-Za-z0-9_\-\.]{2,}").unwrap());
static BEARER: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)(bearer\s+)[A-Za-z0-9_\-\.=]+").unwrap());
static AUTH_JSON: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?i)("(?:api[_-]?key|authorization|x-api-key)"\s*:\s*")[^"]*(")"#).unwrap());

/// Replace every `sk-...` token, `Bearer <token>` and JSON api-key fields.
pub fn redact(input: &str) -> String {
    let s = SK.replace_all(input, "sk-***");
    let s = BEARER.replace_all(&s, "${1}***");
    let s = AUTH_JSON.replace_all(&s, "${1}***${2}");
    s.into_owned()
}

/// Like [`redact`] but also removes an exact secret even when it does not look like `sk-`.
pub fn redact_secret(input: &str, secret: Option<&str>) -> String {
    let mut s = input.to_string();
    if let Some(k) = secret {
        let k = k.trim();
        if k.len() >= 6 {
            s = s.replace(k, "***");
        }
    }
    redact(&s)
}

/// `sk-••••••••` + last 4 — the only form of the key the frontend ever sees.
pub fn last4(key: &str) -> String {
    let chars: Vec<char> = key.trim().chars().collect();
    let n = chars.len();
    if n <= 4 {
        return String::new();
    }
    chars[n - 4..].iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn removes_sk_keys() {
        let s = "error: invalid key sk-AbC123_xyz-9.q for model godev1";
        let r = redact(s);
        assert!(!r.contains("AbC123"));
        assert!(r.contains("sk-***"));
        assert!(r.contains("godev1"));
    }

    #[test]
    fn removes_multiple_and_bearer() {
        let s = "Authorization: Bearer sk-aaaa1111 and Bearer abcdef.ghi and sk-bbbb2222";
        let r = redact(s);
        assert!(!r.contains("aaaa1111"));
        assert!(!r.contains("bbbb2222"));
        assert!(!r.contains("abcdef.ghi"));
    }

    #[test]
    fn removes_json_fields() {
        let s = r#"{"api_key":"hunter2-secret","model":"godev1","Authorization":"xyz"}"#;
        let r = redact(s);
        assert!(!r.contains("hunter2"));
        assert!(!r.contains("\"xyz\""));
        assert!(r.contains("godev1"));
    }

    #[test]
    fn leaves_normal_text() {
        let s = "ต่อ Gateway ไม่ได้ กรุณาต่อ VPN บริษัทแล้วลองใหม่ (task-list ok)";
        assert_eq!(redact(s), s);
    }

    #[test]
    fn redacts_exact_secret() {
        let r = redact_secret("server echoed MYSECRETVALUE back", Some("MYSECRETVALUE"));
        assert!(!r.contains("MYSECRETVALUE"));
    }

    #[test]
    fn last4_works() {
        assert_eq!(last4("sk-abcdefx7Kp"), "x7Kp");
        assert_eq!(last4("abc"), "");
    }
}
