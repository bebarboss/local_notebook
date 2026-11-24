//! Keyless web search for the "ค้นเว็บ" toggle in a general chat. Uses DuckDuckGo's HTML
//! endpoint (no API key), falling back to the Lite endpoint when the HTML one rate-limits, plus a
//! plain HTML→text pass for pages (structured data + tables + article text). Unofficial endpoints,
//! so treat as best-effort: a failed search/fetch returns an error the caller turns into
//! "ค้นเว็บไม่ได้" rather than failing the whole question. Never logs the query, the page text, or
//! the answer — only hosts, counts, and error codes.

use crate::errors::{AppError, AppResult};
use std::collections::HashSet;
use std::time::Duration;

/// A generic browser-like UA — never carries user/account details (privacy rule).
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0 Safari/537.36";
/// Primary SERP (rich HTML). When it rate-limits we fall back to the Lite one below.
const SEARCH_URL: &str = "https://html.duckduckgo.com/html/";
/// Lite SERP: a plain table of results that survives when the HTML endpoint throttles us.
const LITE_URL: &str = "https://lite.duckduckgo.com/lite/";
const REQ_TIMEOUT: Duration = Duration::from_secs(12);
/// Cap a fetched page's raw HTML before parsing, so a giant page can't blow up memory.
const BODY_CAP: usize = 2_000_000;
/// JSON-LD keys worth surfacing as readable text (news/articles, products/prices, places).
const LD_KEYS: &[&str] = &[
    "name", "headline", "alternativeheadline", "description", "articlebody", "text",
    "datepublished", "datemodified", "price", "pricecurrency", "lowprice", "highprice",
    "value", "unittext", "availability", "address", "telephone", "author", "publisher",
];

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

/// Search DuckDuckGo and return up to `max` results. Tries the HTML endpoint, then the Lite one.
/// Error = couldn't reach/parse either endpoint; `Ok(empty)` = reached one but it had no results.
pub async fn search(query: &str, max: usize) -> AppResult<Vec<WebHit>> {
    let mut last_err: Option<AppError> = None;
    // 1) rich HTML endpoint
    match fetch_serp(SEARCH_URL, query).await {
        Ok(html) => {
            let hits = parse_serp(&html, max);
            if !hits.is_empty() {
                return Ok(hits);
            }
        }
        Err(e) => last_err = Some(e),
    }
    // 2) Lite endpoint (different markup; holds up when the HTML one throttles)
    match fetch_serp(LITE_URL, query).await {
        Ok(html) => Ok(parse_lite(&html, max)),
        Err(e) => Err(last_err.unwrap_or(e)),
    }
}

/// GET a SERP endpoint and return its HTML, or an error with just the status code.
async fn fetch_serp(endpoint: &str, query: &str) -> AppResult<String> {
    let resp = crate::llm::CLIENT
        .get(endpoint)
        .query(&[("q", query), ("kl", "th-th")])
        .header(reqwest::header::USER_AGENT, UA)
        .timeout(REQ_TIMEOUT)
        .send()
        .await
        .map_err(AppError::from)?;
    if !resp.status().is_success() {
        return Err(AppError::new("web_search", format!("ค้นเว็บไม่สำเร็จ (HTTP {})", resp.status().as_u16())));
    }
    resp.text().await.map_err(AppError::from)
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

/// Pull result blocks out of the DuckDuckGo HTML SERP. `pub(crate)` so the headless browser
/// search can reuse it on the HTML endpoint it renders.
pub(crate) fn parse_serp(html: &str, max: usize) -> Vec<WebHit> {
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

/// Pull results out of the DuckDuckGo Lite SERP (a flat table: links then snippets in order).
fn parse_lite(html: &str, max: usize) -> Vec<WebHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let (Ok(a_sel), Ok(snip_sel)) =
        (Selector::parse("a.result-link"), Selector::parse("td.result-snippet")) else {
        return Vec::new();
    };
    let snippets: Vec<String> = doc.select(&snip_sel).map(|s| collapse_ws(&s.text().collect::<String>())).collect();
    let mut out = Vec::new();
    for (i, a) in doc.select(&a_sel).enumerate() {
        let title = collapse_ws(&a.text().collect::<String>());
        let Some(url) = a.value().attr("href").and_then(clean_ddg_url) else { continue };
        if title.is_empty() {
            continue;
        }
        let snippet = snippets.get(i).cloned().unwrap_or_default();
        out.push(WebHit { title, url, snippet });
        if out.len() >= max {
            break;
        }
    }
    out
}

/// Pull organic results out of a Bing SERP (`<li class="b_algo"><h2><a href=…>` with a
/// `.b_caption` snippet). Bing links are direct URLs, no redirect. Used as the second engine the
/// headless browser renders when DuckDuckGo returns nothing (e.g. rate-limited). `pub(crate)`.
pub(crate) fn parse_bing(html: &str, max: usize) -> Vec<WebHit> {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let (Ok(item_sel), Ok(a_sel), Ok(cap_sel)) = (
        Selector::parse("li.b_algo"),
        Selector::parse("h2 a"),
        Selector::parse(".b_caption p, .b_caption, p"),
    ) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for item in doc.select(&item_sel) {
        let Some(a) = item.select(&a_sel).next() else { continue };
        let Some(href) = a.value().attr("href") else { continue };
        if !matches!(url::Url::parse(href).ok().as_ref().map(|u| u.scheme()), Some("http") | Some("https")) {
            continue;
        }
        let host = host_of(href);
        if host == "www.bing.com" || host == "bing.com" {
            continue; // skip Bing's own "see more"/translate links
        }
        let title = collapse_ws(&a.text().collect::<String>());
        if title.is_empty() || !seen.insert(href.to_string()) {
            continue;
        }
        let snippet = item.select(&cap_sel).next().map(|s| collapse_ws(&s.text().collect::<String>())).unwrap_or_default();
        out.push(WebHit { title, url: href.to_string(), snippet });
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

/// A readable-text pass over a page: structured data (JSON-LD, meta) first, then table rows joined
/// cell-by-cell (so price tables keep their numbers), then a block-aware walk of the rest of the
/// page. The walk emits one line per block element and keeps inline text (so modern `<div>`/`<span>`
/// card layouts — price boxes, scores, Tailwind sites — survive, which selecting only `<p>`/`<li>`
/// would miss). Script/style/nav/footer and the already-handled tables are skipped. Short lines drop.
fn html_to_text(html: &str, cap: usize) -> String {
    use scraper::{Html, Selector};
    let doc = Html::parse_document(html);
    let mut parts: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut add = |t: String, min: usize| {
        if t.chars().count() >= min && seen.insert(t.clone()) {
            parts.push(t);
        }
    };

    // 1) structured data — the most reliable place to find a headline, a price, a published date
    for bit in structured_bits(&doc) {
        add(bit, 3);
    }

    // 2) table rows: join a row's cells so "ทองแท่ง | 42,100 | 42,200" survives as one line
    if let Ok(tr_sel) = Selector::parse("tr") {
        if let Ok(cell_sel) = Selector::parse("td, th") {
            for tr in doc.select(&tr_sel) {
                let cells: Vec<String> = tr
                    .select(&cell_sel)
                    .map(|c| collapse_ws(&c.text().collect::<String>()))
                    .filter(|c| !c.is_empty())
                    .collect();
                if cells.len() >= 2 {
                    add(cells.join(" | "), 8);
                }
            }
        }
    }

    // 3) the rest of the page, one line per block element (keeps div/span card text)
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    walk_text(doc.tree.root(), 0, &mut cur, &mut lines);
    flush_line(&mut cur, &mut lines);
    for line in lines {
        add(line, 8);
    }

    cap_text(&parts.join("\n"), cap)
}

/// Tags whose whole subtree is skipped: non-content, boilerplate, or handled elsewhere (tables).
fn is_skip(name: &str) -> bool {
    matches!(
        name,
        "script" | "style" | "noscript" | "template" | "svg" | "head" | "iframe" | "nav"
            | "footer" | "aside" | "table"
    )
}

/// Block-level tags: a line break starts before and after them so their text doesn't glue to siblings.
fn is_block(name: &str) -> bool {
    matches!(
        name,
        "p" | "div" | "section" | "article" | "main" | "header" | "li" | "ul" | "ol" | "dl" | "dt"
            | "dd" | "tr" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "br" | "blockquote" | "figure"
            | "figcaption" | "caption" | "hr" | "pre" | "address" | "details" | "summary"
    )
}

/// Recursively collect visible text: append inline text into `cur`, and flush `cur` as a line at
/// each block boundary. Bounded depth so a pathological document can't overflow the stack.
fn walk_text(node: ego_tree::NodeRef<'_, scraper::Node>, depth: usize, cur: &mut String, out: &mut Vec<String>) {
    if depth > 500 {
        return;
    }
    for child in node.children() {
        match child.value() {
            scraper::Node::Text(t) => cur.push_str(t),
            scraper::Node::Element(el) => {
                let name = el.name();
                if is_skip(name) {
                    continue;
                }
                if is_block(name) {
                    flush_line(cur, out);
                    walk_text(child, depth + 1, cur, out);
                    flush_line(cur, out);
                } else {
                    walk_text(child, depth + 1, cur, out);
                }
            }
            _ => {}
        }
    }
}

/// Collapse the buffered text into one line and push it (if non-empty), then clear the buffer.
fn flush_line(cur: &mut String, out: &mut Vec<String>) {
    let line = collapse_ws(cur);
    cur.clear();
    if !line.is_empty() {
        out.push(line);
    }
}

/// Readable values from `<meta>` description/OpenGraph tags and JSON-LD blocks.
fn structured_bits(doc: &scraper::Html) -> Vec<String> {
    use scraper::Selector;
    let mut out = Vec::new();
    if let Ok(meta_sel) = Selector::parse(
        r#"meta[name="description"], meta[property="og:title"], meta[property="og:description"]"#,
    ) {
        for m in doc.select(&meta_sel) {
            if let Some(c) = m.value().attr("content") {
                let c = collapse_ws(c);
                if !c.is_empty() {
                    out.push(c);
                }
            }
        }
    }
    if let Ok(ld_sel) = Selector::parse(r#"script[type="application/ld+json"]"#) {
        for s in doc.select(&ld_sel) {
            let raw = s.text().collect::<String>();
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                let mut vals = Vec::new();
                collect_ld(&v, &mut vals);
                out.extend(vals);
            }
        }
    }
    out
}

/// Walk a JSON-LD value and collect the string/number values under the keys we care about.
/// Bounded so a pathological document can't dump unbounded text.
fn collect_ld(v: &serde_json::Value, out: &mut Vec<String>) {
    use serde_json::Value;
    const MAX_LD: usize = 40;
    match v {
        Value::Object(map) => {
            for (k, val) in map {
                if out.len() >= MAX_LD {
                    return;
                }
                if LD_KEYS.contains(&k.to_lowercase().as_str()) {
                    match val {
                        Value::String(s) => {
                            let s = collapse_ws(s);
                            if !s.is_empty() {
                                out.push(format!("{k}: {s}"));
                            }
                        }
                        Value::Number(n) => out.push(format!("{k}: {n}")),
                        _ => collect_ld(val, out),
                    }
                } else {
                    collect_ld(val, out);
                }
            }
        }
        Value::Array(arr) => {
            for item in arr {
                if out.len() >= MAX_LD {
                    return;
                }
                collect_ld(item, out);
            }
        }
        _ => {}
    }
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

    // Lite SERP: links and snippets live in separate rows, matched positionally.
    const LITE: &str = r#"
      <html><body><table>
        <tr><td>1.&nbsp;</td><td><a rel="nofollow" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.com%2Fx&rut=1" class="result-link">ผลแรก A</a></td></tr>
        <tr><td></td><td class="result-snippet">ตัวอย่างของผลแรกที่ยาวพอ</td></tr>
        <tr><td>2.&nbsp;</td><td><a rel="nofollow" href="https://b.com/y" class="result-link">ผลสอง B</a></td></tr>
        <tr><td></td><td class="result-snippet">ตัวอย่างของผลสอง</td></tr>
      </table></body></html>"#;

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
    fn parses_lite_serp() {
        let hits = parse_lite(LITE, 10);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://a.com/x");
        assert!(hits[0].title.contains("ผลแรก"));
        assert!(hits[0].snippet.contains("ยาวพอ"), "snippet matched to its link by position");
        assert_eq!(hits[1].url, "https://b.com/y");
    }

    // Bing SERP: organic results are <li class="b_algo"> with a direct link and a caption.
    const BING: &str = r#"
      <html><body><ol id="b_results">
        <li class="b_algo"><h2><a href="https://example.com/ai">10 โปรเจค AI น่าสร้าง</a></h2>
          <div class="b_caption"><p>รวมไอเดียโปรเจค AI ที่ลงมือทำได้จริง</p></div></li>
        <li class="b_algo"><h2><a href="https://www.bing.com/translator">ตัวแปลภาษา Bing</a></h2></li>
        <li class="b_algo"><h2><a href="https://foo.org/rag">สร้างระบบ RAG</a></h2>
          <div class="b_caption"><p>คู่มือทำ RAG chatbot</p></div></li>
      </ol></body></html>"#;

    #[test]
    fn parses_bing_serp() {
        let hits = parse_bing(BING, 10);
        assert_eq!(hits.len(), 2, "two organic results; Bing's own translate link dropped");
        assert_eq!(hits[0].url, "https://example.com/ai");
        assert!(hits[0].title.contains("โปรเจค AI"));
        assert!(hits[0].snippet.contains("ลงมือทำได้จริง"));
        assert_eq!(hits[1].url, "https://foo.org/rag");
    }

    #[test]
    fn bing_respects_max() {
        assert_eq!(parse_bing(BING, 1).len(), 1);
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
    fn html_to_text_keeps_table_rows_and_structured_data() {
        let html = r#"<html><head>
              <meta name="description" content="ราคาทองประจำวัน อัปเดตล่าสุด">
              <script type="application/ld+json">
                {"@type":"Article","headline":"ราคาทองวันนี้ปรับขึ้น","datePublished":"2026-11-20","price":42200}
              </script>
            </head><body>
              <table>
                <tr><th>ประเภท</th><th>รับซื้อ</th><th>ขายออก</th></tr>
                <tr><td>ทองคำแท่ง</td><td>42,100</td><td>42,200</td></tr>
              </table>
            </body></html>"#;
        let text = html_to_text(html, 10_000);
        assert!(text.contains("ราคาทองประจำวัน"), "meta description surfaced");
        assert!(text.contains("ราคาทองวันนี้ปรับขึ้น"), "JSON-LD headline surfaced");
        assert!(text.contains("42200") || text.contains("price: 42200"), "JSON-LD price surfaced");
        assert!(text.contains("ทองคำแท่ง | 42,100 | 42,200"), "table row joined cell-by-cell");
    }

    #[test]
    fn html_to_text_keeps_div_span_cards() {
        // modern layout (e.g. goldtraders.or.th): price lives in div/span cards, no <table>/<p>
        let html = r#"<html><body>
              <nav><a href="/menu">เมนูหลักของเว็บไซต์ที่ยาวพอจะถูกตัด</a></nav>
              <main>
                <div class="flex"><span>รับซื้อ</span><span>58,150.00</span></div>
                <div class="flex"><span>ขายออก</span><span>58,350.00</span></div>
              </main>
            </body></html>"#;
        let text = html_to_text(html, 10_000);
        assert!(
            text.contains("รับซื้อ 58,150.00") || text.contains("รับซื้อ58,150.00"),
            "price card kept as one line: {text}"
        );
        assert!(text.contains("ขายออก"), "second card kept");
        assert!(!text.contains("เมนูหลัก"), "nav boilerplate is skipped");
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
