//! Headless WebView2 for "ค้นเว็บ": load a URL in a hidden window, let its JavaScript run, then run
//! a small script and read the result back from the native side via `ICoreWebView2::ExecuteScript`.
//! Two uses:
//!   * `render_text` — read `document.body.innerText` of a page (values a site renders with JS that
//!     a plain HTTP fetch would miss: live prices, scores).
//!   * `ddg_search` — run the DuckDuckGo search *in a real browser*, which passes the bot/anomaly
//!     challenge that blocks keyless HTTP scraping (DDG returns HTTP 202 + a challenge page to bare
//!     requests). This keeps web search working without any API key.
//!
//! Safety: the hidden window uses a one-off label that no capability targets, so the external page
//! it loads cannot invoke any app/plugin/core command — we only pull data out from Rust, never
//! expose the IPC to it. Only http/https URLs are accepted. Never logs page content — only the host
//! and error codes.

use crate::errors::{AppError, AppResult};
use crate::websearch::WebHit;
use std::time::Duration;
use tauri::{AppHandle, Manager, WebviewUrl, WebviewWindowBuilder};

/// Let an arbitrary page load + run its scripts this long before reading.
const PAGE_SETTLE: Duration = Duration::from_millis(3200);
/// A search-results page renders faster than an arbitrary site.
const SEARCH_SETTLE: Duration = Duration::from_millis(1900);
/// Hard cap on a whole render (build + settle + read), so a hung page can't stall the loop.
const RENDER_TIMEOUT: Duration = Duration::from_secs(16);
/// How long to wait for ExecuteScript's result to come back.
const READ_TIMEOUT: Duration = Duration::from_secs(6);

/// JS that reads the visible text of the page.
const INNER_TEXT_JS: &str = "document.body ? document.body.innerText : ''";

/// Load `url` in a hidden WebView2, wait for JS to render, and return `document.body.innerText`
/// trimmed to `cap` characters. Windows only; returns an error on other platforms.
pub async fn render_text(app: &AppHandle, url: &str, cap: usize) -> AppResult<String> {
    if !is_http(url) {
        return Err(AppError::new("web_render", "URL ไม่ถูกต้อง"));
    }
    let text = eval_on_page(app, url, INNER_TEXT_JS, PAGE_SETTLE).await?;
    Ok(cap_chars(&text, cap))
}

/// Search DuckDuckGo inside a real browser and return up to `max` results. This is the fallback for
/// when the keyless HTTP endpoints are bot-blocked (HTTP 202 challenge). Snippets are left empty —
/// the research loop reads the promising links for their content.
pub async fn ddg_search(app: &AppHandle, query: &str, max: usize) -> AppResult<Vec<WebHit>> {
    let search_url = url::Url::parse_with_params(
        "https://duckduckgo.com/",
        &[("q", query), ("kl", "th-th"), ("ia", "web")],
    )
    .map_err(|_| AppError::new("web_render", "คำค้นไม่ถูกต้อง"))?
    .to_string();

    // collect result links from whichever DuckDuckGo result markup rendered, as a JSON array
    let script = format!(
        r#"(function(){{
            var sels=['a[data-testid="result-title-a"]','a.result__a','.result__title a','h2 a[href^="http"]'];
            var out=[],seen={{}};
            for(var i=0;i<sels.length;i++){{
                document.querySelectorAll(sels[i]).forEach(function(a){{
                    var u=a.href||'';
                    if(!u||seen[u]) return;
                    if(u.indexOf('duckduckgo.com')>-1) return;
                    if(u.indexOf('http')!==0) return;
                    seen[u]=1;
                    out.push({{title:(a.innerText||'').replace(/\s+/g,' ').trim(),url:u}});
                }});
                if(out.length) break;
            }}
            return JSON.stringify(out.slice(0,{max}));
        }})()"#
    );

    let json = eval_on_page(app, &search_url, &script, SEARCH_SETTLE).await?;
    let raw: Vec<RawHit> = serde_json::from_str(&json).unwrap_or_default();
    Ok(raw
        .into_iter()
        .filter(|h| !h.url.is_empty())
        .map(|h| WebHit { title: h.title, url: h.url, snippet: String::new() })
        .collect())
}

#[derive(serde::Deserialize)]
struct RawHit {
    #[serde(default)]
    title: String,
    #[serde(default)]
    url: String,
}

/// Build a hidden window at `url`, let it settle, run `script`, and return its (decoded) string
/// result. The whole thing is bounded by `RENDER_TIMEOUT`.
async fn eval_on_page(app: &AppHandle, url: &str, script: &str, settle: Duration) -> AppResult<String> {
    match tokio::time::timeout(RENDER_TIMEOUT, eval_inner(app, url, script, settle)).await {
        Ok(r) => r,
        Err(_) => Err(AppError::new("web_render", "เรนเดอร์หน้าไม่ทันเวลา")),
    }
}

async fn eval_inner(app: &AppHandle, url: &str, script: &str, settle: Duration) -> AppResult<String> {
    let label = format!("hw-{}", uuid::Uuid::new_v4().simple());
    let parsed = url::Url::parse(url).map_err(|_| AppError::new("web_render", "URL ไม่ถูกต้อง"))?;

    // window creation must happen on the main (UI) thread on Windows
    {
        let (app2, label2) = (app.clone(), label.clone());
        let (tx, rx) = tokio::sync::oneshot::channel::<bool>();
        app.run_on_main_thread(move || {
            let ok = WebviewWindowBuilder::new(&app2, &label2, WebviewUrl::External(parsed))
                .visible(false)
                .focused(false)
                .skip_taskbar(true)
                .inner_size(1024.0, 768.0)
                .build()
                .is_ok();
            let _ = tx.send(ok);
        })
        .map_err(|e| AppError::new("web_render", format!("เปิดหน้าซ่อนไม่ได้ ({e})")))?;
        if !rx.await.unwrap_or(false) {
            return Err(AppError::new("web_render", "เปิดหน้าซ่อนไม่ได้"));
        }
    }

    // always close the hidden window, even on an early return below
    let _guard = CloseGuard { app: app.clone(), label: label.clone() };

    // let the page load and run its scripts
    tokio::time::sleep(settle).await;

    run_script(app, &label, script).await
}

#[cfg(windows)]
async fn run_script(app: &AppHandle, label: &str, script: &str) -> AppResult<String> {
    use webview2_com::ExecuteScriptCompletedHandler;
    use windows::core::{HSTRING, PCWSTR};

    let win = app.get_webview_window(label).ok_or_else(|| AppError::new("web_render", "หน้าซ่อนหาย"))?;
    let (tx, rx) = tokio::sync::oneshot::channel::<String>();
    let script = HSTRING::from(script);

    win.with_webview(move |pw| {
        // SAFETY: runs on the webview's own (main) thread, where the controller is valid.
        unsafe {
            let controller = pw.controller();
            if let Ok(core) = controller.CoreWebView2() {
                // webview2-com hands the result back already decoded to a String (JSON-encoded).
                let handler = ExecuteScriptCompletedHandler::create(Box::new(move |_err, result: String| {
                    let _ = tx.send(result);
                    Ok(())
                }));
                let _ = core.ExecuteScript(PCWSTR(script.as_ptr()), &handler);
            }
        }
    })
    .map_err(|e| AppError::new("web_render", format!("อ่านหน้าไม่ได้ ({e})")))?;

    // ExecuteScript returns the JS value as a JSON-encoded string; decode one level back to the
    // natural string (plain innerText, or the search JSON array as a string).
    match tokio::time::timeout(READ_TIMEOUT, rx).await {
        Ok(Ok(json)) => Ok(serde_json::from_str::<String>(&json).unwrap_or(json)),
        _ => Err(AppError::new("web_render", "อ่านเนื้อหาไม่ได้")),
    }
}

#[cfg(not(windows))]
async fn run_script(_app: &AppHandle, _label: &str, _script: &str) -> AppResult<String> {
    Err(AppError::new("web_render", "รองรับเฉพาะ Windows"))
}

fn is_http(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

/// Closes the hidden window when the render finishes or bails out.
struct CloseGuard {
    app: AppHandle,
    label: String,
}
impl Drop for CloseGuard {
    fn drop(&mut self) {
        if let Some(w) = self.app.get_webview_window(&self.label) {
            let _ = w.close();
        }
    }
}

fn cap_chars(s: &str, max: usize) -> String {
    let s = s.trim();
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect()
}
