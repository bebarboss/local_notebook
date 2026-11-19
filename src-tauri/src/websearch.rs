//! Keyless web search for the "ค้นเว็บ" toggle in a general chat. Uses DuckDuckGo's HTML
//! endpoint (no API key) and a plain HTML→text pass for pages. Unofficial endpoint, so treat as
//! best-effort: a failed search/fetch returns an error the caller turns into "ค้นเว็บไม่ได้"
//! rather than failing the whole question. Never logs the query, the page text, or the answer —
//! only hosts, counts, and error codes.

use crate::errors::{AppError, AppResult};
use std::time::Duration;

/// A generic browser-like UA — never carries user/account details (privacy rule).
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";
const REQ_TIMEOUT: Duration = Duration::from_secs(12);
/// Cap a fetched page's raw HTML before parsing, so a giant page can't blow up memory.
const BODY_CAP: usize = 2_000_000;

/// One search result.
#[derive(Debug, Clone, PartialEq)]
pub struct WebHit {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

/// The host of a URL for step/activity display (never the full URL with its query).
pub fn host_of(u: &str) -> String {
    url::Url::parse(u).ok().and_then(|p| p.host_str().map(|h| h.to_string())).unwrap_or_else(|| "เว็บ".to_string())
}

/// Search DuckDuckGo and return up to `max` results. Error = couldn't reach/parse the endpoint.
pub async fn search(query: &str, max: usize) -> AppResult<Vec<WebHit>> {
    let resp = crate::llm::CLIENT
        .get(SEARCH_URL)
        .query(&[("q", query), ("kl", "th-th")])
        .header(reqwest::header::USER_AGENT, UA)
        .timeout(REQ_TIMEOUT)
        .send()
        .await
        .map_err(AppError::from)?;
    if !resp.status().is_success() {
        return Err(AppError::new("web_search", format!("ค้นเว็บไม่สำเร็จ (HTTP {})", resp.status().as_u16())));
    }
    let html = resp.text().await.map_err(AppError::from)?;
    Ok(parse_serp(&html, max))
}

/// Fetch a page and return its readable text, trimmed to `cap` characters.
pub async fn fetch_text(page_url: &str, cap: usize) -> AppResult<String> {
    let resp = crate::llm::CLIENT
        .get(page_url)
        .header(reqwest::header::USER_AGENT, UA)
        .timeout(REQ_TIMEOUT)
        .send()
        .await
        .map_err(AppError::from)?;
    if !resp.status().is_success() {
        return Err(AppError::new("web_fetch", format!("เปิดหน้าไม่สำเร็จ (HTTP {})", resp.status().as_u16())));
    }
    let mut html = resp.text().await.map_err(AppError::from)?;
    if html.len() > BODY_CAP {
        // truncate on a char boundary before parsing
        let mut end = BODY_CAP;
        while end > 0 && !html.is_char_boundary(end) {
            end -= 1;
        }
        html.truncate(end);
    }
    let text = html_to_text(&html, cap);
    if text.is_empty() {
        return Err(AppError::new("web_empty", "หน้านี้ไม่มีข้อความให้อ่าน"));
    }
    Ok(text)
}

/// Pull result blocks out of the DuckDuckGo HTML SERP.
fn parse_serp(html: &str, max: usize) -> Vec<WebHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let (Ok(result_sel), Ok(a_sel), Ok(snip_sel)) = (
        Selector::parse("div.result, div.web-result"),
        Selector::parse("a.result__a"),
        Selector::parse(".result__snippet"),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for res in doc.select(&result_sel) {
        let Some(a) = res.select(&a_sel).next() else { continue };
        let title = collapse_ws(&a.text().collect::<String>());
        let Some(real) = a.value().attr("href").and_then(clean_ddg_url) else { continue };
        if title.is_empty() {
            continue;
        }
        let snippet = res.select(&snip_sel).next().map(|s| collapse_ws(&s.text().collect::<String>())).unwrap_or_default();
        out.push(WebHit { title, url: real, snippet });
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Turn a DuckDuckGo result href into the real destination URL. Handles both the `/l/?uddg=…`
/// redirect (protocol-relative too) and a direct link. Returns `None` for anything not http(s).
fn clean_ddg_url(href: &str) -> Option<String> {
    let abs = if href.starts_with("//") { format!("https:{href}") } else { href.to_string() };
    let parsed = url::Url::parse(&abs).ok()?;
    if let Some((_, v)) = parsed.query_pairs().find(|(k, _)| k == "uddg") {
        let dest = url::Url::parse(&v).ok()?;
        return matches!(dest.scheme(), "http" | "https").then(|| dest.to_string());
    }
    if matches!(parsed.scheme(), "http" | "https") && parsed.host_str() != Some("duckduckgo.com") {
        return Some(abs);
    }
    None
}

/// A rough readable-text pass: the text of the usual content blocks, de-scripted by construction
/// (script/style are not among the selected tags). Short/boilerplate lines are dropped.
fn html_to_text(html: &str, cap: usize) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let Ok(sel) = Selector::parse("p, h1, h2, h3, h4, li, td, blockquote") else {
        return String::new();
    };
    let mut parts: Vec<String> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for el in doc.select(&sel) {
        let t = collapse_ws(&el.text().collect::<String>());
        if t.chars().count() >= 20 && seen.insert(t.clone()) {
            parts.push(t);
        }
    }
    cap_text(&parts.join("\n"), cap)
}

fn collapse_ws(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn cap_text(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push_str("\n…(ตัดเนื้อหาบางส่วน)");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SERP: &str = r#"
      <html><body>
        <div class="result results_links">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fen.wikipedia.org%2Fwiki%2FThailand&rut=abc">ไทยแลนด์ - Wikipedia</a>
          <a class="result__snippet">ประเทศไทย เป็นประเทศในเอเชียตะวันออกเฉียงใต้</a>
        </div>
        <div class="result results_links">
          <a class="result__a" href="https://example.com/page">ตัวอย่าง โดยตรง</a>
          <a class="result__snippet">สรุปสั้น ๆ</a>
        </div>
        <div class="result results_links">
          <a class="result__a" href="//duckduckgo.com/l/?uddg=notaurl">ลิงก์เสีย</a>
        </div>
      </body></html>"#;

    #[test]
    fn parses_serp_and_decodes_uddg() {
        let hits = parse_serp(SERP, 10);
        assert_eq!(hits.len(), 2, "the broken link is dropped");
        assert_eq!(hits[0].url, "https://en.wikipedia.org/wiki/Thailand");
        assert!(hits[0].title.contains("Wikipedia"));
        assert!(hits[0].snippet.contains("เอเชีย"));
        assert_eq!(hits[1].url, "https://example.com/page");
    }

    #[test]
    fn serp_respects_max() {
        assert_eq!(parse_serp(SERP, 1).len(), 1);
    }

    #[test]
    fn clean_url_handles_redirect_and_direct() {
        assert_eq!(
            clean_ddg_url("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2Fx&rut=1").as_deref(),
            Some("https://a.com/x")
        );
        assert_eq!(clean_ddg_url("https://b.com/y").as_deref(), Some("https://b.com/y"));
        assert_eq!(clean_ddg_url("javascript:void(0)"), None);
        assert_eq!(clean_ddg_url("//duckduckgo.com/l/?uddg=ftp%3A%2F%2Fx"), None);
    }

    #[test]
    fn html_to_text_drops_script_and_short_bits() {
        let html = r#"<html><head><style>.x{color:red}</style></head>
            <body>
              <script>var secret = 'leak'; alert(secret);</script>
              <nav>เมนู</nav>
              <p>นี่คือย่อหน้าที่ยาวพอจะถูกเก็บไว้เป็นเนื้อหาจริงของหน้า</p>
              <p>สั้น</p>
              <h2>หัวข้อที่ยาวพอสมควรจะถูกเก็บด้วยเช่นกัน</h2>
            </body></html>"#;
        let text = html_to_text(html, 10_000);
        assert!(text.contains("ย่อหน้าที่ยาวพอ"));
        assert!(text.contains("หัวข้อที่ยาวพอ"));
        assert!(!text.contains("secret"), "script contents must not appear");
        assert!(!text.contains("color:red"), "style contents must not appear");
        assert!(!text.contains("สั้น"), "too-short lines are dropped");
    }

    #[test]
    fn cap_text_trims_on_char_boundary() {
        let s = "กขคง".repeat(100);
        let capped = cap_text(&s, 50);
        // 50 kept chars + a short "…(ตัดเนื้อหาบางส่วน)" suffix
        assert!(capped.chars().count() <= 50 + 30);
        assert!(capped.chars().take(50).eq(s.chars().take(50)));
        assert!(capped.ends_with("บางส่วน)"));
    }
}
