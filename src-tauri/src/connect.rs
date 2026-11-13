//! Setup links: `docask://connect?url=<server>&token=<token>` — made on the server with
//! `server/embed/connect-link.sh` and sent to the team. Clicking one opens DocAsk, which asks
//! once ("connect to <host>?") and then saves the URL, keeps the token in the credential store
//! and turns the team database on. Nobody types or sees the token.
//!
//! The token never reaches the frontend: a link waits here (`PENDING`) and the UI only gets
//! the host to confirm. Asking first matters: a forged link could otherwise point DocAsk at
//! someone else's server and send it the documents.

use crate::embed_server::validate_url;
use crate::errors::{AppError, AppResult};
use parking_lot::Mutex;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

pub const SCHEME: &str = "docask";

#[derive(Clone)]
pub struct Link {
    pub url: String,
    pub token: String,
}

/// What the UI is allowed to see about a waiting link.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    pub host: String,
    pub url: String,
}

static PENDING: Mutex<Option<Link>> = Mutex::new(None);

fn decode(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' if i + 2 < b.len() => {
                let hex = |c: u8| (c as char).to_digit(16);
                match (hex(b[i + 1]), hex(b[i + 2])) {
                    (Some(h), Some(l)) => {
                        out.push((h * 16 + l) as u8);
                        i += 3;
                        continue;
                    }
                    _ => out.push(b'%'),
                }
            }
            b'+' => out.push(b' '),
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `docask://connect?url=…&token=…` (also `docask:connect?…`, `docask://connect/?…`).
pub fn parse(link: &str) -> AppResult<Link> {
    let bad = || AppError::new("invalid_link", "ลิงก์ตั้งค่าไม่ถูกต้อง — ขอลิงก์ใหม่จากผู้ดูแลเซิร์ฟเวอร์");
    let l = link.trim().trim_matches('"');
    let rest = l.strip_prefix(&format!("{SCHEME}:")).ok_or_else(bad)?;
    let rest = rest.trim_start_matches('/');
    let (action, query) = rest.split_once('?').ok_or_else(bad)?;
    if action.trim_end_matches('/') != "connect" {
        return Err(bad());
    }
    let (mut url, mut token) = (None, None);
    for pair in query.split('&') {
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        match k {
            "url" => url = Some(decode(v)),
            "token" => token = Some(decode(v)),
            _ => {}
        }
    }
    let url = validate_url(&url.ok_or_else(bad)?)?;
    let token = token.ok_or_else(bad)?;
    crate::secrets::validate_server_token(&token)?;
    Ok(Link { url, token: token.trim().to_string() })
}

fn info(l: &Link) -> LinkInfo {
    let host = reqwest::Url::parse(&l.url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| l.url.clone());
    LinkInfo { host, url: l.url.clone() }
}

/// A setup link among process arguments (Windows passes the clicked link as one).
pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().find(|a| a.trim_matches('"').to_ascii_lowercase().starts_with(&format!("{SCHEME}:")))
}

/// Keep a link until the user confirms; tell the UI which host it is for.
pub fn receive(app: &AppHandle, link: &str) -> AppResult<LinkInfo> {
    let l = parse(link)?;
    let i = info(&l);
    *PENDING.lock() = Some(l);
    let _ = app.emit("connect-link", i.clone());
    crate::diag::log("setup link received");
    Ok(i)
}

pub fn pending() -> Option<LinkInfo> {
    PENDING.lock().as_ref().map(info)
}

pub fn take() -> Option<Link> {
    PENDING.lock().take()
}

pub fn discard() {
    PENDING.lock().take();
}

/// Make Windows open `docask://` links with this program (per user, no admin rights).
#[cfg(windows)]
pub fn register_scheme() {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;
    let Ok(exe) = std::env::current_exe() else { return };
    let command = format!("\"{}\" \"%1\"", exe.display());
    let hk = RegKey::predef(HKEY_CURRENT_USER);
    let path = format!("Software\\Classes\\{SCHEME}");
    // already pointing at this exe → nothing to write
    if let Ok(k) = hk.open_subkey(format!("{path}\\shell\\open\\command")) {
        if k.get_value::<String, _>("").ok().as_deref() == Some(command.as_str()) {
            return;
        }
    }
    let r = (|| -> std::io::Result<()> {
        let (root, _) = hk.create_subkey(&path)?;
        root.set_value("", &"URL:DocAsk setup link")?;
        root.set_value("URL Protocol", &"")?;
        let (cmd, _) = hk.create_subkey(format!("{path}\\shell\\open\\command"))?;
        cmd.set_value("", &command)?;
        Ok(())
    })();
    if let Err(e) = r {
        log::warn!("could not register docask:// links: {e}");
    }
}

#[cfg(not(windows))]
pub fn register_scheme() {}

#[cfg(test)]
mod tests {
    use super::*;

    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[test]
    fn parses_links() {
        let l = parse(&format!("docask://connect?url=https%3A%2F%2Fabc-def.trycloudflare.com&token={TOKEN}")).unwrap();
        assert_eq!(l.url, "https://abc-def.trycloudflare.com");
        assert_eq!(l.token, TOKEN);
        let l = parse(&format!("\"docask://connect/?token={TOKEN}&url=https://x.example.com/\"")).unwrap();
        assert_eq!(l.url, "https://x.example.com");
        assert_eq!(info(&l).host, "x.example.com");
    }

    #[test]
    fn rejects_bad_links() {
        assert!(parse("https://evil.example.com").is_err());
        assert!(parse(&format!("docask://other?url=https://x.example.com&token={TOKEN}")).is_err());
        assert!(parse("docask://connect?url=https://x.example.com&token=short").is_err());
        assert!(parse(&format!("docask://connect?url=http://evil.example.com&token={TOKEN}")).is_err(), "plain http on the internet");
        assert!(parse(&format!("docask://connect?token={TOKEN}")).is_err());
    }

    #[test]
    fn finds_the_link_among_arguments() {
        let args = vec!["C:\\DocAsk.exe".to_string(), "--autostart".into(), "docask://connect?url=x&token=y".into()];
        assert_eq!(from_args(args).as_deref(), Some("docask://connect?url=x&token=y"));
        assert_eq!(from_args(vec!["DocAsk.exe".to_string()]), None);
    }

    #[test]
    fn percent_decoding() {
        assert_eq!(decode("a%20b+c%2F%zz"), "a b c/%zz");
        assert_eq!(decode("%E0%B8%81"), "ก");
        assert_eq!(decode("50%"), "50%");
    }
}
