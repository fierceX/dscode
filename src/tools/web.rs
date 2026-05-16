use anyhow::{Result, bail};

pub fn web_search(query: &str) -> Result<String> {
    if query.is_empty() { bail!("Error: no query provided"); }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .build()?;
    let mut req = client
        .get("https://s.jina.ai/")
        .query(&[("q", query)])
        .header("X-Respond-With", "no-content");
    if let Ok(key) = std::env::var("JINA_API_KEY") {
        if !key.is_empty() { req = req.header("Authorization", format!("Bearer {key}")); }
    }
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("no tokio runtime"))?;
    Ok(std::thread::spawn(move || handle.block_on(async { req.send().await?.text().await }))
        .join()
        .map_err(|_| anyhow::anyhow!("web_search thread panicked"))??)
}

pub fn web_fetch(url: &str) -> Result<String> {
    if url.is_empty() { bail!("Error: no url provided"); }
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(60))
        .build()?;
    let mut req = client
        .get("https://r.jina.ai/")
        .query(&[("url", url)]);
    if let Ok(key) = std::env::var("JINA_API_KEY") {
        if !key.is_empty() { req = req.header("Authorization", format!("Bearer {key}")); }
    }
    let handle = tokio::runtime::Handle::try_current()
        .map_err(|_| anyhow::anyhow!("no tokio runtime"))?;
    Ok(std::thread::spawn(move || handle.block_on(async { req.send().await?.text().await }))
        .join()
        .map_err(|_| anyhow::anyhow!("web_fetch thread panicked"))??)
}

pub struct WebSearchTool;
pub struct WebFetchTool;

impl super::runner::ToolExec for WebSearchTool {
    fn name(&self) -> &'static str { "WebSearch" }
    fn execute(&self, input: &serde_json::Value, _ctx: &crate::context::AgentSharedContext) -> anyhow::Result<(String, bool, String)> {
        #[derive(serde::Deserialize)]
        struct Args { query: String }
        let args: Args = serde_json::from_value(input.clone())?;
        web_search(&args.query).map(|s| (s, false, String::new()))
    }
}

impl super::runner::ToolExec for WebFetchTool {
    fn name(&self) -> &'static str { "WebFetch" }
    fn execute(&self, input: &serde_json::Value, _ctx: &crate::context::AgentSharedContext) -> anyhow::Result<(String, bool, String)> {
        #[derive(serde::Deserialize)]
        struct Args { url: String }
        let args: Args = serde_json::from_value(input.clone())?;
        web_fetch(&args.url).map(|s| (s, false, String::new()))
    }
}
