use anyhow::{Result, anyhow, bail};
use std::process::{Command, Stdio};

pub fn glob(pattern: &str, path: &str) -> Result<String> {
    if pattern.is_empty() { bail!("Error: no pattern provided"); }
    let _ = Command::new("rg").arg("--version").output()
        .map_err(|_| anyhow!("Error: rg is required for glob"))?;
    let output = Command::new("rg")
        .args(["--files", path, "-g", pattern])
        .stdout(Stdio::piped()).stderr(Stdio::null())
        .output()?;
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

pub fn grep(pattern: &str, path: &str, file_glob: &str, context: Option<usize>) -> Result<String> {
    if pattern.is_empty() { bail!("Error: no pattern provided"); }
    let _ = Command::new("rg").arg("--version").output()
        .map_err(|_| anyhow!("Error: rg is required for grep"))?;
    let mut cmd = Command::new("rg");
    cmd.args(["-n", "--color", "never", "--heading"]);
    if let Some(c) = context && c > 0 { cmd.args(["-C", &c.to_string()]); }
    if !file_glob.is_empty() { cmd.args(["--glob", file_glob]); }
    if pattern.starts_with('-') { cmd.args(["-e", pattern]); } else { cmd.args(["--", pattern]); }
    cmd.arg(path);
    let output = cmd.stdout(Stdio::piped()).stderr(Stdio::null()).output()?;
    Ok(String::from_utf8_lossy(&output.stdout).trim_end_matches('\n').to_string())
}

pub struct GlobTool;
pub struct GrepTool;

impl super::runner::ToolExec for GlobTool {
    fn name(&self) -> &'static str { "Glob" }
    fn execute(&self, input: &serde_json::Value, _ctx: &crate::context::ToolContext) -> anyhow::Result<(String, bool, String, Option<i32>)> {
        #[derive(serde::Deserialize)]
        struct Args { pattern: String, #[serde(default)] path: Option<String> }
        let args: Args = serde_json::from_value(input.clone())?;
        glob(&args.pattern, args.path.as_deref().unwrap_or(".")).map(|s| (s, false, String::new(), None))
    }
}

impl super::runner::ToolExec for GrepTool {
    fn name(&self) -> &'static str { "Grep" }
    fn execute(&self, input: &serde_json::Value, _ctx: &crate::context::ToolContext) -> anyhow::Result<(String, bool, String, Option<i32>)> {
        #[derive(serde::Deserialize)]
        struct Args { pattern: String, #[serde(default)] path: Option<String>, #[serde(default)] glob: Option<String>, #[serde(default)] context: Option<usize> }
        let args: Args = serde_json::from_value(input.clone())?;
        grep(&args.pattern, args.path.as_deref().unwrap_or("."), args.glob.as_deref().unwrap_or(""), args.context)
            .map(|s| (s, false, String::new(), None))
    }
}
