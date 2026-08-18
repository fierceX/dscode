//! Pure routing logic ported from `pi-deepseek-route/core.ts`.
//!
//! All texts and thresholds originate from the 2026-08-15 official API probes
//! for `deepseek-v4-flash` (`reasoning_effort=max`, n=2).

use regex::Regex;
use serde_json::Value;
use std::sync::LazyLock;

pub const MODE_SPEC: i8 = 0;
pub const MODE_MIXED: f64 = 0.3;
pub const MODE_REACT: i8 = 1;
pub const MODE_WEAK: &str = "weak";

const SPEC_PERSONA: &str = "You are a helpful software engineer assistant.";

const MIXED_PERSONA: &str = "You are a helpful software engineer assistant.\n\
Work directly: prefer writing or editing code over describing plans. \
Verify your changes by reading and running them.";

const REACT_PERSONA: &str = "You are a hands-on software engineer who delivers working output fast.\n\
Work directly: write or edit code, then verify it by reading and running. \
Keep the loop tight — produce, verify, fix — and do not build test \
harnesses, scaffolding, or ceremony the user did not ask for. \
Finish with a usable deliverable and a short summary.";

const WEAK_FLASH: &str = "You are a helpful assistant.\n\
Before acting, decide the task type (build or fix) and adopt the matching \
style: build → hands-on production; fix → inspect-and-plan.\n\
Before acting, briefly review what you have already done in this session and continue from where you left off; do not repeat completed steps. Do not run environment checks (echo, whoami, uname, node --version, date) or exhaustive grep/glob scans.\n\
Think deeply first, then produce.";

pub const GUIDE_BASE: &str = "\n\nRouter: classify this task (build or fix) now, then adopt the matching style — build: direct production; fix: inspect-first.";

pub const GUIDE_BOOST: &str = "\n\nRouter: this is a NEW task, different from the previous ones. Classify it fresh (build or fix) and adopt the matching style — build: direct production; fix: inspect-first. Do not follow the previous task's style.";

pub const GUIDE_COMMIT: &str = " Think deeply first, then commit and act.";

pub const GUIDE_DEEP: &str = " Think deeply about the architecture, edge cases, and integration points. Do not spend reasoning on the environment or tooling. Produce when your information is complete.";

static COMPLEX_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(重构|架构|全面|详细|设计|系统|优化|分析|survey|overview|architecture|refactor|comprehensive|detailed|design|system|optimize|analyze)").unwrap()
});

static CHAT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"^(你好|您好|hello|hi|hey|嗨|哈喽|在吗|谢谢|感谢|thanks|thank you|早上好|下午好|晚上好|嗯|好|ok|okay|yes|no|嗯嗯|好的)[!。.!？?~～]*$").unwrap()
});

static REACT_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(开发|创建|写一个|写|生成|从零|做|做一个|做个|游戏|网页|网站|构建|新项目|搭建|实现|做出|上线|落地|脚本|工具|应用|build|create|develop|generate|implement|write a|write an|build a|make a|new project)").unwrap()
});

static SPEC_RE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(修复|修一下|调试|重构|维护|排查|报错|出错|崩溃|优化|审查|review|fix|debug|refactor|maintain|repair|broken|break|为什么|异常|故障|迁移|升级|兼容)").unwrap()
});

/// True when the task text is long or uses architecture-level wording.
pub fn is_complex_task(text: &str) -> bool {
    !text.is_empty() && (text.chars().count() > 120 || COMPLEX_RE.is_match(text))
}

/// True when the first message is a greeting / acknowledgement / no task.
pub fn is_chat_task(text: &str) -> bool {
    let t = text.trim();
    if t.is_empty() {
        return true;
    }
    if CHAT_RE.is_match(t) {
        return true;
    }
    if t.chars().count() > 24 {
        return false;
    }
    !REACT_RE.is_match(t) && !SPEC_RE.is_match(t)
}

/// True when the model id is in the Flash family.
pub fn is_flash_model(model_id: &str) -> bool {
    model_id.to_ascii_lowercase().contains("flash")
}

/// Quantize a mode to one of the four measured behavior bands.
pub fn band_of(mode: Mode) -> Band {
    match mode {
        Mode::Weak => Band::Weak,
        Mode::Spec => Band::Spec,
        Mode::Mixed => Band::Transition,
        Mode::React => Band::React,
        Mode::Number(m) => {
            let m = clamp01(m);
            if m < 0.2 {
                Band::Spec
            } else if m < 0.5 {
                Band::Transition
            } else {
                Band::React
            }
        }
    }
}

/// Human-readable band name.
pub fn band_for(mode: Mode) -> &'static str {
    match band_of(mode) {
        Band::Spec => "spec",
        Band::Transition => "mixed",
        Band::React => "react",
        Band::Weak => "weak",
    }
}

/// Persona for a mode. This crate is Flash-only, so weak always uses
/// `WEAK_FLASH`.
pub fn persona_for(mode: Mode) -> &'static str {
    match band_of(mode) {
        Band::Spec => SPEC_PERSONA,
        Band::Transition => MIXED_PERSONA,
        Band::Weak => WEAK_FLASH,
        Band::React => REACT_PERSONA,
    }
}

/// First-turn core tool candidates for Mink tool names.
pub fn core_for(mode: Mode) -> &'static [&'static str] {
    match band_of(mode) {
        Band::Spec => &["Read", "Edit", "Glob", "Grep"],
        Band::Transition => &["Read", "Edit", "Write", "Glob", "Grep"],
        Band::Weak => &["Bash", "Read"],
        Band::React => &["Read", "Write", "Edit"],
    }
}

/// Per-message near-field guidance (Flash-only dispatch).
pub fn guide_for(round: usize, text: &str) -> String {
    let base = if round >= 3 { GUIDE_BOOST } else { GUIDE_BASE };
    if is_complex_task(text) {
        format!("{base}{GUIDE_DEEP}")
    } else {
        format!("{base}{GUIDE_COMMIT}")
    }
}

/// Mode type accepted by router APIs.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Mode {
    Spec,
    Mixed,
    React,
    Weak,
    Number(f64),
}

/// Measured behavior band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Band {
    Spec,
    Transition,
    React,
    Weak,
}

impl Mode {
    pub fn as_number_or_weak(self) -> Result<f64, &'static str> {
        match self {
            Mode::Weak => Err("weak"),
            Mode::Number(n) => Ok(clamp01(n)),
            Mode::Spec => Ok(0.0),
            Mode::Mixed => Ok(MODE_MIXED),
            Mode::React => Ok(1.0),
        }
    }
}

/// Classify a task text into a mode.
pub fn classify_task(text: &str) -> Mode {
    let react = REACT_RE.find_iter(text).count();
    let spec = SPEC_RE.find_iter(text).count();
    if react > spec {
        Mode::Number(1.0)
    } else if spec > react {
        Mode::Number(0.0)
    } else {
        Mode::Weak
    }
}

/// Clamp a numeric mode into [0, 1].
pub fn clamp01(v: f64) -> f64 {
    v.clamp(0.0, 1.0)
}

/// Parse a user/agent-supplied mode token.
pub fn parse_mode(token: &str) -> Option<Mode> {
    let t = token.trim().to_ascii_lowercase();
    match t.as_str() {
        "auto" => None,
        "weak" | "router" => Some(Mode::Weak),
        "spec" | "spec-lean" => Some(Mode::Spec),
        "balanced" | "mixed" => Some(Mode::Mixed),
        "react" | "react-lean" => Some(Mode::React),
        _ => {
            let n: f64 = t.parse().ok()?;
            if t.contains('.') {
                Some(Mode::Number(clamp01(n)))
            } else {
                Some(Mode::Number(clamp01(n / 100.0)))
            }
        }
    }
}

/// Extract plain text from a message content value.
pub fn message_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .filter_map(|item| match item {
                Value::String(s) => Some(s.clone()),
                Value::Object(map) => map.get("text").and_then(Value::as_str).map(str::to_string),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" "),
        _ => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flash_persona_is_weak_flash() {
        assert!(persona_for(Mode::Weak).contains("Before acting"));
        assert!(persona_for(Mode::Weak).contains("Think deeply first"));
    }

    #[test]
    fn classify_build_is_react() {
        assert!(matches!(classify_task("写一个游戏"), Mode::Number(n) if n == 1.0));
        assert!(matches!(classify_task("build a website"), Mode::Number(n) if n == 1.0));
    }

    #[test]
    fn classify_fix_is_spec() {
        assert!(matches!(classify_task("修复这个 bug"), Mode::Number(n) if n == 0.0));
        assert!(matches!(classify_task("fix the crash"), Mode::Number(n) if n == 0.0));
    }

    #[test]
    fn classify_ambiguous_is_weak() {
        assert!(matches!(classify_task("帮我看看"), Mode::Weak));
    }

    #[test]
    fn chat_detection() {
        assert!(is_chat_task("你好"));
        assert!(is_chat_task("hello"));
        assert!(is_chat_task("ok"));
        assert!(!is_chat_task("修复这个 bug"));
        assert!(!is_chat_task("写一个网站"));
    }

    #[test]
    fn complex_detection() {
        assert!(is_complex_task("请全面分析这个系统的架构并给出优化方案"));
        assert!(!is_complex_task("修一下"));
    }

    #[test]
    fn guide_rounds_and_complexity() {
        let simple = guide_for(1, "修一下");
        assert!(simple.contains(GUIDE_COMMIT));
        let complex = guide_for(3, "全面优化架构");
        assert!(complex.contains(GUIDE_BOOST));
        assert!(complex.contains(GUIDE_DEEP));
    }

    #[test]
    fn parse_mode_accepts_bands_and_numbers() {
        assert!(matches!(parse_mode("weak"), Some(Mode::Weak)));
        assert!(matches!(parse_mode("spec"), Some(Mode::Spec)));
        assert!(matches!(parse_mode("react"), Some(Mode::React)));
        assert!(matches!(parse_mode("0.5"), Some(Mode::Number(n)) if n == 0.5));
        assert!(matches!(parse_mode("50"), Some(Mode::Number(n)) if n == 0.5));
        assert!(parse_mode("auto").is_none());
    }

    #[test]
    fn message_text_extracts_strings_and_blocks() {
        assert_eq!(message_text(&Value::String("hi".into())), "hi");
        let blocks =
            serde_json::json!([{"type":"text","text":"hello"},{"type":"text","text":"world"}]);
        assert_eq!(message_text(&blocks), "hello world");
    }
}
