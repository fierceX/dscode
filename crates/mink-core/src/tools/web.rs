use anyhow::{Result, anyhow, bail};
use regex::Regex;
use std::collections::HashSet;
use std::sync::LazyLock;
use std::time::Duration;

const DEFAULT_USER_AGENT: &str =
    "Mozilla/5.0 (Macintosh; Intel Mac OS X 10.15; rv:128.0) Gecko/20100101 Firefox/128.0";
const DEFAULT_ACCEPT: &str = "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8";
const DEFAULT_ACCEPT_LANGUAGE: &str = "en-US,en;q=0.8";
const DDG_LITE_URL: &str = "https://lite.duckduckgo.com/lite/";
const DDG_HTML_URL: &str = "https://html.duckduckgo.com/html/";
const MAX_SEARCH_RESULTS: usize = 8;

static ANCHOR_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<a\b(?P<attrs>[^>]*)>(?P<title>.*?)</a>"#).unwrap());
static HREF_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)\bhref\s*=\s*(['"])(?P<href>.*?)['"]"#).unwrap());
static SNIPPET_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<(?:td|a|div)\b[^>]*class\s*=\s*['"][^'"]*(?:result-snippet|result__snippet)[^'"]*['"][^>]*>(?P<snippet>.*?)</(?:td|a|div)>"#).unwrap()
});
static TITLE_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<title[^>]*>(?P<title>.*?)</title>"#).unwrap());
static SCRIPT_STYLE_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<(script|style|noscript|svg)\b[^>]*>.*?</(script|style|noscript|svg)>"#)
        .unwrap()
});
static COMMENT_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<!--.*?-->"#).unwrap());
static BLOCK_BREAK_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r#"(?is)<\s*/\s*(p|div|li|h[1-6]|tr|section|article)\s*>"#).unwrap()
});
static LINE_BREAK_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"(?is)<\s*br\s*/?\s*>"#).unwrap());
static TAG_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"(?is)<[^>]+>"#).unwrap());
static WHITESPACE_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"[ \t\r\x0c]+"#).unwrap());
static SPACE_BEFORE_PUNCT_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r#"\s+([.,;:!?])"#).unwrap());
static BLANK_LINES_RE: LazyLock<Regex> = LazyLock::new(|| Regex::new(r#"\n{3,}"#).unwrap());

#[derive(Debug, Clone, PartialEq, Eq)]
struct SearchResult {
    title: String,
    url: String,
    snippet: String,
}

pub fn web_search(query: &str) -> Result<String> {
    let query = query.trim();
    if query.is_empty() {
        bail!("Error: no query provided");
    }

    let client = build_web_client(Duration::from_secs(15), 5)?;
    let mut failures = Vec::new();

    match search_ddg_lite(&client, query) {
        Ok(results) if !results.is_empty() => {
            return Ok(format_search_results(query, "DuckDuckGo Lite", results));
        }
        Ok(_) => {}
        Err(err) => failures.push(err.to_string()),
    }

    match search_ddg_html(&client, query) {
        Ok(results) if !results.is_empty() => {
            return Ok(format_search_results(query, "DuckDuckGo HTML", results));
        }
        Ok(_) => {}
        Err(err) => failures.push(err.to_string()),
    }

    if !failures.is_empty() {
        bail!(
            "Error: DuckDuckGo search failed after Lite GET and HTML POST attempts: {}",
            failures.join("; ")
        );
    }

    Ok(format_search_results(query, "DuckDuckGo", Vec::new()))
}

fn search_ddg_lite(client: &reqwest::Client, query: &str) -> Result<Vec<SearchResult>> {
    let req = client
        .get(DDG_LITE_URL)
        .query(&[("q", query)])
        .header("Accept", DEFAULT_ACCEPT)
        .header("Accept-Language", DEFAULT_ACCEPT_LANGUAGE)
        .header("DNT", "1")
        .header("Sec-GPC", "1")
        .header("Upgrade-Insecure-Requests", "1")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "same-site")
        .header("Sec-Fetch-User", "?1")
        .header("Referer", "https://duckduckgo.com/");
    let html = send_text(req, "DuckDuckGo Lite search")?;
    ensure_not_ddg_challenge("DuckDuckGo Lite", &html)?;
    Ok(parse_ddg_results(&html))
}

fn search_ddg_html(client: &reqwest::Client, query: &str) -> Result<Vec<SearchResult>> {
    let req = client
        .post(DDG_HTML_URL)
        .form(&[("q", query), ("b", ""), ("l", "us-en")])
        .header("Accept", DEFAULT_ACCEPT)
        .header("Accept-Language", DEFAULT_ACCEPT_LANGUAGE)
        .header("DNT", "1")
        .header("Sec-GPC", "1")
        .header("Upgrade-Insecure-Requests", "1")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "same-site")
        .header("Sec-Fetch-User", "?1")
        .header("Origin", "https://duckduckgo.com")
        .header("Referer", "https://duckduckgo.com/");
    let html = send_text(req, "DuckDuckGo HTML search")?;
    ensure_not_ddg_challenge("DuckDuckGo HTML", &html)?;
    Ok(parse_ddg_results(&html))
}

pub fn web_fetch(url: &str) -> Result<String> {
    let url = normalize_fetch_url(url)?;
    let client = build_web_client(Duration::from_secs(60), 10)?;
    let req = client
        .get(&url)
        .header("Accept", DEFAULT_ACCEPT)
        .header("Accept-Language", DEFAULT_ACCEPT_LANGUAGE)
        .header("DNT", "1")
        .header("Sec-GPC", "1")
        .header("Upgrade-Insecure-Requests", "1")
        .header("Sec-Fetch-Dest", "document")
        .header("Sec-Fetch-Mode", "navigate")
        .header("Sec-Fetch-Site", "none")
        .header("Sec-Fetch-User", "?1");
    let handle = tokio::runtime::Handle::try_current().map_err(|_| anyhow!("no tokio runtime"))?;
    std::thread::spawn(move || {
        handle.block_on(async move {
            let resp = req.send().await?.error_for_status()?;
            let final_url = resp.url().to_string();
            let content_type = resp
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_ascii_lowercase();
            let text = resp.text().await?;
            if content_type.contains("html") || looks_like_html(&text) {
                Ok(html_to_text(&final_url, &text))
            } else {
                Ok(format!("Source: {final_url}\n\n{text}"))
            }
        })
    })
    .join()
    .map_err(|_| anyhow!("web_fetch thread panicked"))?
}

fn build_web_client(timeout: Duration, max_redirects: usize) -> Result<reqwest::Client> {
    let builder = reqwest::Client::builder()
        .timeout(timeout)
        .user_agent(web_user_agent())
        .redirect(reqwest::redirect::Policy::limited(max_redirects));
    Ok(apply_env_proxies(builder)?.build()?)
}

fn web_user_agent() -> String {
    std::env::var("MINK_WEB_USER_AGENT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| DEFAULT_USER_AGENT.to_string())
}

fn send_text(req: reqwest::RequestBuilder, label: &str) -> Result<String> {
    let handle = tokio::runtime::Handle::try_current().map_err(|_| anyhow!("no tokio runtime"))?;
    std::thread::spawn(move || {
        handle.block_on(async move { req.send().await?.error_for_status()?.text().await })
    })
    .join()
    .map_err(|_| anyhow!("{label} thread panicked"))?
    .map_err(|e| anyhow!("{label} failed: {e}"))
}

fn apply_env_proxies(mut builder: reqwest::ClientBuilder) -> Result<reqwest::ClientBuilder> {
    if let Some(proxy) = proxy_from_env("https") {
        builder = builder.proxy(reqwest::Proxy::https(proxy)?);
    }
    if let Some(proxy) = proxy_from_env("http") {
        builder = builder.proxy(reqwest::Proxy::http(proxy)?);
    }
    if proxy_from_env("https").is_none()
        && proxy_from_env("http").is_none()
        && let Some(proxy) = proxy_from_env("all")
    {
        builder = builder.proxy(reqwest::Proxy::all(proxy)?);
    }
    Ok(builder)
}

fn proxy_from_env(kind: &str) -> Option<String> {
    let keys = match kind {
        "https" => ["HTTPS_PROXY", "https_proxy"],
        "http" => ["HTTP_PROXY", "http_proxy"],
        "all" => ["ALL_PROXY", "all_proxy"],
        _ => return None,
    };
    for key in keys {
        if let Ok(value) = std::env::var(key) {
            let value = value.trim();
            if !value.is_empty() {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn ensure_not_ddg_challenge(provider: &str, html: &str) -> Result<()> {
    if is_ddg_challenge(html) {
        bail!(
            "{provider} returned an anti-bot challenge. Try a different proxy, reduce request frequency, or set MINK_WEB_USER_AGENT."
        );
    }
    Ok(())
}

fn is_ddg_challenge(html: &str) -> bool {
    let lower = html.to_ascii_lowercase();
    lower.contains("anomaly.js")
        || lower.contains("challenge-form")
        || lower.contains("anomaly-modal")
        || lower.contains("unfortunately, bots use duckduckgo too")
}

fn parse_ddg_results(html: &str) -> Vec<SearchResult> {
    let mut results = Vec::new();
    let mut seen = HashSet::new();

    for cap in ANCHOR_RE.captures_iter(html) {
        let attrs = cap.name("attrs").map(|m| m.as_str()).unwrap_or("");
        let Some(href) = extract_href(attrs) else {
            continue;
        };
        if !is_search_result_anchor(attrs, &href) {
            continue;
        }
        let Some(url) = normalize_ddg_result_url(&href) else {
            continue;
        };
        if !seen.insert(url.clone()) {
            continue;
        }
        let title = clean_html_text(cap.name("title").map(|m| m.as_str()).unwrap_or(""));
        if title.is_empty() {
            continue;
        }
        let snippet = cap
            .get(0)
            .and_then(|m| html.get(m.end()..))
            .map(extract_snippet_after_anchor)
            .unwrap_or_default();
        results.push(SearchResult {
            title,
            url,
            snippet,
        });
        if results.len() >= MAX_SEARCH_RESULTS {
            break;
        }
    }

    results
}

fn extract_href(attrs: &str) -> Option<String> {
    HREF_RE
        .captures(attrs)
        .and_then(|cap| cap.name("href"))
        .map(|m| decode_html_entities(m.as_str()))
}

fn is_search_result_anchor(attrs: &str, href: &str) -> bool {
    let attrs_l = attrs.to_ascii_lowercase();
    let href_l = href.to_ascii_lowercase();
    attrs_l.contains("result-link")
        || attrs_l.contains("result__a")
        || href_l.contains("/l/?")
        || href_l.contains("duckduckgo.com/l/?")
}

fn normalize_ddg_result_url(href: &str) -> Option<String> {
    let href = decode_html_entities(href).trim().to_string();
    if let Some(value) = query_param(&href, "uddg") {
        let decoded = percent_decode(&value);
        if decoded.starts_with("http://") || decoded.starts_with("https://") {
            return Some(decoded);
        }
    }
    if href.starts_with("//") {
        return Some(format!("https:{href}"));
    }
    if href.starts_with("http://") || href.starts_with("https://") {
        return Some(href);
    }
    None
}

fn query_param(url: &str, name: &str) -> Option<String> {
    let query = url.split_once('?')?.1;
    for part in query.split('&') {
        let (key, value) = part.split_once('=').unwrap_or((part, ""));
        if key == name {
            return Some(value.to_string());
        }
    }
    None
}

fn extract_snippet_after_anchor(after: &str) -> String {
    let next_link = ["result-link", "result__a"]
        .into_iter()
        .filter_map(|needle| after.find(needle))
        .min()
        .unwrap_or_else(|| after.len().min(4096));
    let segment = &after[..next_link.min(after.len())];
    SNIPPET_RE
        .captures(segment)
        .and_then(|cap| cap.name("snippet"))
        .map(|m| clean_html_text(m.as_str()))
        .unwrap_or_default()
}

fn format_search_results(query: &str, source: &str, results: Vec<SearchResult>) -> String {
    if results.is_empty() {
        return format!("No DuckDuckGo results found for: {query}");
    }
    let mut out = format!("# WebSearch results for: {query}\n\n");
    for (idx, item) in results.iter().enumerate() {
        out.push_str(&format!("{}. [{}]({})\n", idx + 1, item.title, item.url));
        if !item.snippet.is_empty() {
            out.push_str(&format!("   {}\n", item.snippet));
        }
        out.push('\n');
    }
    out.push_str(&format!("Source: {source}\n"));
    out
}

pub(crate) fn normalize_fetch_url(raw: &str) -> Result<String> {
    let raw = raw.trim();
    if raw.is_empty() {
        bail!("Error: no url provided");
    }
    if let Some(rest) = raw.strip_prefix("http://") {
        return Ok(format!("https://{rest}"));
    }
    if raw.starts_with("https://") {
        return Ok(raw.to_string());
    }
    bail!("Error: url must start with http:// or https://");
}

fn looks_like_html(text: &str) -> bool {
    let prefix = text
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    prefix.contains("<html") || prefix.contains("<!doctype html")
}

fn html_to_text(url: &str, html: &str) -> String {
    let title = TITLE_RE
        .captures(html)
        .and_then(|cap| cap.name("title"))
        .map(|m| clean_html_text(m.as_str()))
        .unwrap_or_default();
    let mut body = COMMENT_RE.replace_all(html, "\n").into_owned();
    body = SCRIPT_STYLE_RE.replace_all(&body, "\n").into_owned();
    body = LINE_BREAK_RE.replace_all(&body, "\n").into_owned();
    body = BLOCK_BREAK_RE.replace_all(&body, "\n").into_owned();
    body = TAG_RE.replace_all(&body, " ").into_owned();
    body = decode_html_entities(&body);
    body = normalize_text_block(&body);

    let mut out = String::new();
    if !title.is_empty() {
        out.push_str(&format!("# {title}\n\n"));
    }
    out.push_str(&format!("Source: {url}\n\n"));
    out.push_str(&body);
    out
}

fn clean_html_text(input: &str) -> String {
    normalize_text_block(&decode_html_entities(&TAG_RE.replace_all(input, " ")))
}

fn normalize_text_block(input: &str) -> String {
    let mut lines = Vec::new();
    for line in input.lines() {
        let line = WHITESPACE_RE.replace_all(line, " ");
        let line = SPACE_BEFORE_PUNCT_RE.replace_all(line.trim(), "$1");
        let line = line.trim();
        if !line.is_empty() {
            lines.push(line.to_string());
        }
    }
    BLANK_LINES_RE
        .replace_all(&lines.join("\n"), "\n\n")
        .trim()
        .to_string()
}

fn decode_html_entities(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    while let Some(idx) = rest.find('&') {
        out.push_str(&rest[..idx]);
        rest = &rest[idx..];
        if let Some(end) = rest.find(';') {
            let entity = &rest[1..end];
            if let Some(decoded) = decode_entity(entity) {
                out.push(decoded);
                rest = &rest[end + 1..];
                continue;
            }
        }
        out.push('&');
        rest = &rest[1..];
    }
    out.push_str(rest);
    out
}

fn decode_entity(entity: &str) -> Option<char> {
    match entity {
        "amp" => Some('&'),
        "lt" => Some('<'),
        "gt" => Some('>'),
        "quot" => Some('"'),
        "apos" | "#39" => Some('\''),
        "nbsp" => Some(' '),
        _ if entity.starts_with("#x") || entity.starts_with("#X") => {
            u32::from_str_radix(&entity[2..], 16)
                .ok()
                .and_then(char::from_u32)
        }
        _ if entity.starts_with('#') => entity[1..].parse::<u32>().ok().and_then(char::from_u32),
        _ => None,
    }
}

fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut idx = 0;
    while idx < bytes.len() {
        match bytes[idx] {
            b'%' if idx + 2 < bytes.len() => {
                if let (Some(h), Some(l)) = (hex_value(bytes[idx + 1]), hex_value(bytes[idx + 2])) {
                    out.push((h << 4) | l);
                    idx += 3;
                    continue;
                }
                out.push(bytes[idx]);
                idx += 1;
            }
            b'+' => {
                out.push(b' ');
                idx += 1;
            }
            byte => {
                out.push(byte);
                idx += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub struct WebSearchTool;
pub struct WebFetchTool;

impl super::runner::ToolExec for WebSearchTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "WebSearch",
            "Search the web.",
            super::metadata::ApprovalTier::Read,
            super::metadata::ToolResultKind::Web,
        )
        .storm_exempt()
        .discoverable()
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        if ctx.tool_config.tool_disable.disable_web {
            return Ok(super::runner::ToolOutcome::text(
                "Error: WebSearch is disabled by configuration.".into(),
            ));
        }
        #[derive(serde::Deserialize)]
        struct Args {
            query: String,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        web_search(&args.query).map(super::runner::ToolOutcome::text)
    }
}

impl super::runner::ToolExec for WebFetchTool {
    fn metadata(&self) -> super::metadata::ToolMetadata {
        super::metadata::ToolMetadata::new(
            "WebFetch",
            "Fetch a web page as readable text.",
            super::metadata::ApprovalTier::Read,
            super::metadata::ToolResultKind::Web,
        )
        .storm_exempt()
        .discoverable()
    }

    fn execute(
        &self,
        input: &serde_json::Value,
        ctx: &crate::context::ToolContext,
    ) -> anyhow::Result<super::runner::ToolOutcome> {
        if ctx.tool_config.tool_disable.disable_web {
            return Ok(super::runner::ToolOutcome::text(
                "Error: WebFetch is disabled by configuration.".into(),
            ));
        }
        #[derive(serde::Deserialize)]
        struct Args {
            url: String,
        }
        let args: Args = serde_json::from_value(input.clone())?;
        web_fetch(&args.url).map(super::runner::ToolOutcome::text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_duckduckgo_lite_results() {
        let html = r#"
        <html><body>
          <a rel="nofollow" class="result-link" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fexample.com%2Fa%3Fx%3D1&amp;rut=abc">Example &amp; Result</a>
          <td class="result-snippet">A <b>small</b> snippet &amp; more.</td>
          <a class="result-link" href="https://example.org/">Second</a>
          <td class="result-snippet">Another snippet.</td>
        </body></html>
        "#;

        let results = parse_ddg_results(html);

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "Example & Result");
        assert_eq!(results[0].url, "https://example.com/a?x=1");
        assert_eq!(results[0].snippet, "A small snippet & more.");
        assert_eq!(results[1].url, "https://example.org/");
    }

    #[test]
    fn formats_empty_search_results() {
        let output = format_search_results("nope", "DuckDuckGo", Vec::new());
        assert!(output.contains("No DuckDuckGo results found"));
    }

    #[test]
    fn parses_duckduckgo_html_results() {
        let html = r#"
        <html><body>
          <div class="result results_links">
            <h2 class="result__title">
              <a rel="nofollow" class="result__a" href="/l/?kh=-1&amp;uddg=https%3A%2F%2Fexample.net%2Fdoc">HTML Result</a>
            </h2>
            <a class="result__snippet" href="/l/?kh=-1&amp;uddg=https%3A%2F%2Fexample.net%2Fdoc">HTML <b>snippet</b>.</a>
          </div>
        </body></html>
        "#;

        let results = parse_ddg_results(html);

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].title, "HTML Result");
        assert_eq!(results[0].url, "https://example.net/doc");
        assert_eq!(results[0].snippet, "HTML snippet.");
    }

    #[test]
    fn detects_duckduckgo_challenge_page() {
        let html = r#"
        <form id="challenge-form" action="//duckduckgo.com/anomaly.js?sv=lite&cc=botnet">
          <div class="anomaly-modal__title">Unfortunately, bots use DuckDuckGo too.</div>
        </form>
        "#;

        assert!(is_ddg_challenge(html));
        assert!(ensure_not_ddg_challenge("DuckDuckGo Lite", html).is_err());
    }

    #[test]
    fn fetch_url_upgrades_http_to_https() {
        assert_eq!(
            normalize_fetch_url("http://example.com/a").unwrap(),
            "https://example.com/a"
        );
    }

    #[test]
    fn fetch_url_rejects_non_http() {
        assert!(normalize_fetch_url("file:///tmp/a").is_err());
    }

    #[test]
    fn html_to_text_removes_scripts_and_tags() {
        let html = r#"
        <!doctype html>
        <html>
          <head><title>Test &amp; Page</title><style>.x{}</style></head>
          <body>
            <h1>Hello</h1>
            <script>alert("bad")</script>
            <p>World&nbsp;Text<br>Next line</p>
          </body>
        </html>
        "#;
        let output = html_to_text("https://example.com", html);

        assert!(output.starts_with("# Test & Page"));
        assert!(output.contains("Source: https://example.com"));
        assert!(output.contains("Hello"));
        assert!(output.contains("World Text"));
        assert!(output.contains("Next line"));
        assert!(!output.contains("alert"));
        assert!(!output.contains("<p>"));
    }

    #[test]
    fn percent_decodes_utf8() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
    }
}
