//! Setup links: `docask://connect?url=<server>&token=<token>` — made on the server with
//! `server/embed/connect-link.sh` and sent to the team. Clicking one opens DocAsk, which asks
//! once ("connect to <host>?") and then saves the URL, keeps the token in the credential store
//! and turns the team database on. Nobody types or sees the token.
//!
//! Invite links: `docask://join?url=<server>&service=<id>&key=<share key>&name=<service>` —
//! made by a service's owner ("คัดลอกลิงก์เชิญ"). Accepting one adds that shared service.
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

/// An invite to one shared service.
#[derive(Clone)]
pub struct JoinLink {
    pub url: String,
    pub service: String,
    pub key: String,
    pub name: String,
}

#[derive(Clone)]
pub enum Pending {
    Connect(Link),
    Join(JoinLink),
}

/// What the UI is allowed to see about a waiting link (never the token or key).
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LinkInfo {
    /// "connect" (server setup) or "join" (a shared service)
    pub kind: &'static str,
    pub host: String,
    pub url: String,
    pub service_name: Option<String>,
}

static PENDING: Mutex<Option<Pending>> = Mutex::new(None);

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

fn bad() -> AppError {
    AppError::new("invalid_link", "ลิงก์ไม่ถูกต้อง — ขอลิงก์ใหม่จากผู้ดูแลเซิร์ฟเวอร์หรือเจ้าของ service")
}

/// `docask://<action>?a=b&…` → (action, params). Also `docask:action?…`, `docask://action/?…`.
fn split(link: &str) -> AppResult<(String, Vec<(String, String)>)> {
    let l = link.trim().trim_matches('"');
    let rest = l.strip_prefix(&format!("{SCHEME}:")).ok_or_else(bad)?.trim_start_matches('/');
    let (action, query) = rest.split_once('?').ok_or_else(bad)?;
    let params = query
        .split('&')
        .map(|pair| {
            let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
            (k.to_string(), decode(v))
        })
        .collect();
    Ok((action.trim_end_matches('/').to_ascii_lowercase(), params))
}

fn param(p: &[(String, String)], k: &str) -> Option<String> {
    p.iter().find(|(x, _)| x == k).map(|(_, v)| v.clone())
}

/// `docask://connect?url=…&token=…`
pub fn parse(link: &str) -> AppResult<Link> {
    let (action, p) = split(link)?;
    if action != "connect" {
        return Err(bad());
    }
    let url = validate_url(&param(&p, "url").ok_or_else(bad)?)?;
    let token = param(&p, "token").ok_or_else(bad)?;
    crate::secrets::validate_server_token(&token)?;
    Ok(Link { url, token: token.trim().to_string() })
}

/// `docask://join?url=…&service=…&key=…&name=…`
pub fn parse_join(link: &str) -> AppResult<JoinLink> {
    let (action, p) = split(link)?;
    if action != "join" {
        return Err(bad());
    }
    let url = validate_url(&param(&p, "url").ok_or_else(bad)?)?;
    let service = param(&p, "service").filter(|s| !s.is_empty() && s.len() <= 64 && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')).ok_or_else(bad)?;
    let key = param(&p, "key").ok_or_else(bad)?;
    crate::secrets::validate_server_token(&key)?;
    let name: String = param(&p, "name").unwrap_or_default().chars().filter(|c| !c.is_control()).take(80).collect();
    Ok(JoinLink { url, service, key: key.trim().to_string(), name })
}

fn host_of(url: &str) -> String {
    reqwest::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_string)).unwrap_or_else(|| url.to_string())
}

fn info(p: &Pending) -> LinkInfo {
    match p {
        Pending::Connect(l) => LinkInfo { kind: "connect", host: host_of(&l.url), url: l.url.clone(), service_name: None },
        Pending::Join(j) => LinkInfo { kind: "join", host: host_of(&j.url), url: j.url.clone(), service_name: Some(if j.name.is_empty() { j.service.clone() } else { j.name.clone() }) },
    }
}

/// A setup link among process arguments (Windows passes the clicked link as one).
pub fn from_args<I: IntoIterator<Item = String>>(args: I) -> Option<String> {
    args.into_iter().find(|a| a.trim_matches('"').to_ascii_lowercase().starts_with(&format!("{SCHEME}:")))
}

/// Keep a link until the user confirms; tell the UI which host it is for.
pub fn receive(app: &AppHandle, link: &str) -> AppResult<LinkInfo> {
    let p = match split(link)?.0.as_str() {
        "join" => Pending::Join(parse_join(link)?),
        _ => Pending::Connect(parse(link)?),
    };
    let i = info(&p);
    *PENDING.lock() = Some(p);
    let _ = app.emit("connect-link", i.clone());
    crate::diag::log("setup link received");
    Ok(i)
}

pub fn pending() -> Option<LinkInfo> {
    PENDING.lock().as_ref().map(info)
}

pub fn take() -> Option<Pending> {
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
        assert_eq!(info(&Pending::Connect(l)).host, "x.example.com");
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
    fn join_links() {
        let j = parse_join(&format!("docask://join?url=https%3A%2F%2Fa.trycloudflare.com&service=ab12&key={TOKEN}&name=HR%20%C2%B7%20x")).unwrap();
        assert_eq!((j.service.as_str(), j.name.as_str()), ("ab12", "HR · x"));
        assert!(parse_join(&format!("docask://join?url=https://a.example.com&service=../x&key={TOKEN}")).is_err(), "odd service id");
        assert!(parse_join("docask://join?url=https://a.example.com&service=ab&key=short").is_err());
        assert!(parse(&format!("docask://join?url=https://a.example.com&token={TOKEN}")).is_err(), "join is not connect");
        let info = info(&Pending::Join(j));
        assert_eq!((info.kind, info.service_name.as_deref()), ("join", Some("HR · x")));
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
