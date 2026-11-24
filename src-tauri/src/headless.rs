//! Headless WebView2 for "ค้นเว็บ": load a URL in a hidden window, let its JavaScript run, then run
//! a small script and read the result back from the native side via `ICoreWebView2::ExecuteScript`.
//! Two uses:
//!   * `render_text` — read `document.body.innerText` of a page (values a site renders with JS that
//!     a plain HTTP fetch would miss: live prices, scores).
//!   * `browser_search` — run a web search *in a real browser*, which passes the bot/anomaly
//!     challenge that blocks keyless HTTP scraping (DuckDuckGo returns HTTP 202 + a challenge page
//!     to bare requests). It renders the server-rendered result page and parses it with the tested
//!     parsers in `websearch`. Tries DuckDuckGo's HTML endpoint first, then Bing (a second, mostly
//!     unblocked engine) when DDG returns nothing — e.g. when DDG rate-limits. No API key.
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
/// JS that reads the rendered HTML of the page body (for parsing a search-results page in Rust).
const INNER_HTML_JS: &str = "document.body ? document.body.innerHTML : ''";

/// Load `url` in a hidden WebView2, wait for JS to render, and return `document.body.innerText`
/// trimmed to `cap` characters. Windows only; returns an error on other platforms.
pub async fn render_text(app: &AppHandle, url: &str, cap: usize) -> AppResult<String> {
    if !is_http(url) {
        return Err(AppError::new("web_render", "URL ไม่ถูกต้อง"));
    }
    let text = eval_on_page(app, url, INNER_TEXT_JS, PAGE_SETTLE).await?;
    Ok(cap_chars(&text, cap))
}

/// Search inside a real browser and return up to `max` results. The fallback for when the keyless
/// HTTP endpoints are bot-blocked (HTTP 202 challenge). Renders a server-rendered results page and
/// parses it with the tested parsers in `websearch`. DuckDuckGo's HTML endpoint first; Bing if DDG
/// comes back empty (rate-limited). Returns an error only when neither page could be rendered.
pub async fn browser_search(app: &AppHandle, query: &str, max: usize) -> AppResult<Vec<WebHit>> {
    let ddg = url::Url::parse_with_params(
        "https://html.duckduckgo.com/html/",
        &[("q", query), ("kl", "th-th")],
    )
    .map_err(|_| AppError::new("web_render", "คำค้นไม่ถูกต้อง"))?
    .to_string();

    // 1) DuckDuckGo HTML endpoint (same markup our tested parse_serp handles)
    let mut last_err: Option<AppError> = None;
    match eval_on_page(app, &ddg, INNER_HTML_JS, SEARCH_SETTLE).await {
        Ok(html) => {
            let hits = crate::websearch::parse_serp(&html, max);
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Err(e) => last_err = Some(e),
    }

    // 2) Bing (a second engine that is rarely blocked; direct links)
    let bing = url::Url::parse_with_params(
        "https://www.bing.com/search",
        &[("q", query), ("setlang", "th")],
    )
    .map_err(|_| AppError::new("web_render", "คำค้นไม่ถูกต้อง"))?
    .to_string();
    match eval_on_page(app, &bing, INNER_HTML_JS, SEARCH_SETTLE).await {
        Ok(html) => Ok(crate::websearch::parse_bing(&html, max)),
        Err(e) => Err(last_err.unwrap_or(e)),
    }
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
