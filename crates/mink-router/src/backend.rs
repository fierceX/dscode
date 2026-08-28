//! `LlmBackend` decorator that applies pi-deepseek-route style routing on the
//! fly. This is the external, non-invasive integration point: Mink only sees a
//! normal LLM backend.

use std::sync::Arc;

use mink::runtime::{LlmBackend, LlmPurpose, LlmRequest, LlmResponseStream};

use crate::config::RouterConfig;
use crate::core::{
    band_for, classify_task, core_for, guide_for, is_chat_task, is_flash_model, persona_for,
};
use crate::prefab::{
    count_real_user_rounds, extract_real_user_messages, filter_core_tools, has_flash_persona,
    has_tool_use_after_real_user, last_is_real_user,
};

/// Router decorator around any [`LlmBackend`].
pub struct RouterLlmBackend {
    inner: Arc<dyn LlmBackend>,
    config: RouterConfig,
}

impl RouterLlmBackend {
    pub fn new(inner: Arc<dyn LlmBackend>, config: RouterConfig) -> Self {
        Self { inner, config }
    }
}

/// Output of a stateless routing transform.
#[derive(Debug, Clone)]
pub struct TransformOutput {
    pub system_prompt: String,
    pub messages: Vec<serde_json::Value>,
    pub tools: Vec<serde_json::Value>,
}

/// Apply the pi-deepseek-route style transform without touching a real
/// [`LlmRequest`]. This is kept separate so the routing logic is unit-testable
/// without constructing an `LlmBackend` or `Display`.
pub fn transform_request(
    config: &RouterConfig,
    model: &str,
    system_prompt: &str,
    messages: &[serde_json::Value],
    tools: &[serde_json::Value],
) -> TransformOutput {
    let mut system_prompt = system_prompt.to_string();
    let mut messages = messages.to_vec();
    let mut tools = tools.to_vec();

    // Flash-only gate.
    if config.flash_only && !is_flash_model(model) {
        return TransformOutput {
            system_prompt,
            messages,
            tools,
        };
    }

    // Extract real user messages (Prefab-aware).
    let real_messages = extract_real_user_messages(&messages, config.prefab_aware);
    let Some(first_real) = real_messages.first() else {
        return TransformOutput {
            system_prompt,
            messages,
            tools,
        };
    };

    // Conversational first message: stand down entirely.
    if is_chat_task(first_real) {
        return TransformOutput {
            system_prompt,
            messages,
            tools,
        };
    }

    let mode = classify_task(first_real);

    // Inject persona only when Prefab/template did not already provide it.
    if !has_flash_persona(&system_prompt) {
        system_prompt.push_str(&format!(
            "\n\n# Reasoning-mode persona (mink-router)\n{}",
            persona_for(mode)
        ));
    }

    // Optional first-turn tool narrowing.
    if config.narrow_first_turn_tools
        && !has_tool_use_after_real_user(&messages, config.prefab_aware)
    {
        tools = filter_core_tools(&tools, core_for(mode));
    }

    // Near-field guidance for weak mode.
    if band_for(mode) == "weak"
        && last_is_real_user(&messages, config.prefab_aware)
        && let Some(last_text) = real_messages.last()
    {
        let round = count_real_user_rounds(&messages, config.prefab_aware);
        let guide = guide_for(round, last_text);
        messages.push(serde_json::json!({
            "role": "user",
            "content": guide,
        }));
    }

    TransformOutput {
        system_prompt,
        messages,
        tools,
    }
}

#[async_trait::async_trait]
impl LlmBackend for RouterLlmBackend {
    fn name(&self) -> &str {
        "router-llm-backend"
    }

    fn image_input_capability(&self, model: &str) -> mink::runtime::ImageInputCapability {
        // The router is a transparent transport decorator: capability
        // declarations must pass through to the inner backend so sessions
        // created through `--router` resolve the same image capability as
        // plain sessions (v7 §3.1).
        self.inner.image_input_capability(model)
    }

    async fn stream(&self, mut request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
        // Only main-agent requests are routed. Compaction and sub-agent calls
        // should stay untouched for now.
        if !matches!(request.purpose, LlmPurpose::Agent) {
            return self.inner.stream(request).await;
        }

        let transformed = transform_request(
            &self.config,
            &request.model,
            &request.system_prompt,
            &request.messages,
            &request.tools,
        );
        request.system_prompt = transformed.system_prompt;
        request.messages = transformed.messages;
        request.tools = transformed.tools;

        self.inner.stream(request).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn user(text: &str) -> serde_json::Value {
        json!({ "role": "user", "content": text })
    }

    fn tool_use() -> serde_json::Value {
        json!({
            "role": "assistant",
            "content": [{"type": "tool_use", "name": "Bash"}]
        })
    }

    #[test]
    fn non_flash_passes_through() {
        let config = RouterConfig::flash_only();
        let out = transform_request(&config, "deepseek-v4-pro", "sys", &[user("修复 bug")], &[]);
        assert_eq!(out.system_prompt, "sys");
        assert_eq!(out.messages.len(), 1);
    }

    #[test]
    fn chat_stands_down() {
        let config = RouterConfig::flash_only();
        let out = transform_request(&config, "deepseek-v4-flash", "sys", &[user("你好")], &[]);
        assert_eq!(out.system_prompt, "sys");
        assert_eq!(out.messages.len(), 1);
    }

    #[test]
    fn flash_weak_adds_persona_and_guidance() {
        let config = RouterConfig::flash_only().with_prefab_aware(true);
        let tools = vec![json!({ "name": "Bash" }), json!({ "name": "Edit" })];
        let out = transform_request(
            &config,
            "deepseek-v4-flash",
            "sys",
            &[user("请帮我看看当前项目里都有哪些文件以及它们之间的关系")],
            &tools,
        );
        assert!(out.system_prompt.contains("Reasoning-mode persona"));
        assert!(out.system_prompt.contains("Before acting"));
        assert_eq!(out.messages.len(), 2); // original + guide
        assert!(
            out.messages[1]["content"]
                .as_str()
                .unwrap()
                .contains("Router:")
        );
        assert_eq!(out.tools.len(), 2); // tool narrowing disabled by default
    }

    #[test]
    fn prefab_warmup_does_not_trigger_guidance() {
        let config = RouterConfig::flash_only().with_prefab_aware(true);
        let messages = vec![
            user(
                "Read the workspace-root AGENTS.md completely before any future maintenance work.",
            ),
            user("Ready."),
        ];
        let out = transform_request(&config, "deepseek-v4-flash", "sys", &messages, &[]);
        // No real user message yet -> no persona/guidance.
        assert_eq!(out.system_prompt, "sys");
        assert_eq!(out.messages.len(), 2);
    }

    #[test]
    fn persona_is_not_duplicated() {
        let config = RouterConfig::flash_only();
        let sys =
            "You are a helpful assistant.\nBefore acting, decide the task type (build or fix)";
        let out = transform_request(&config, "deepseek-v4-flash", sys, &[user("修复 bug")], &[]);
        assert!(!out.system_prompt.contains("Reasoning-mode persona"));
    }

    #[test]
    fn first_turn_tools_can_be_narrowed() {
        let config = RouterConfig::flash_only()
            .with_prefab_aware(true)
            .with_narrow_first_turn_tools(true);
        let tools = vec![
            json!({ "name": "Bash" }),
            json!({ "name": "Read" }),
            json!({ "name": "Edit" }),
            json!({ "name": "Write" }),
        ];
        // Ambiguous task -> weak -> core Bash+Read.
        let out = transform_request(
            &config,
            "deepseek-v4-flash",
            "sys",
            &[user("请帮我看看当前项目里都有哪些文件以及它们之间的关系")],
            &tools,
        );
        let names: Vec<&str> = out
            .tools
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        assert_eq!(names, vec!["Bash", "Read"]);
    }

    #[test]
    fn after_tool_use_full_tools_restored() {
        let config = RouterConfig::flash_only()
            .with_prefab_aware(true)
            .with_narrow_first_turn_tools(true);
        let tools = vec![
            json!({ "name": "Bash" }),
            json!({ "name": "Read" }),
            json!({ "name": "Edit" }),
        ];
        let messages = vec![user("帮我看看"), tool_use()];
        let out = transform_request(&config, "deepseek-v4-flash", "sys", &messages, &tools);
        assert_eq!(out.tools.len(), 3);
    }
}

#[cfg(test)]
mod image_capability_tests {
    use super::*;
    use mink::runtime::{
        ImageInputCapability, LlmBackend, LlmRequest, LlmResponseStream, OpenAiChatImageUrlLimits,
    };

    struct VisionInner;

    #[async_trait::async_trait]
    impl LlmBackend for VisionInner {
        fn name(&self) -> &str {
            "vision-inner"
        }

        fn image_input_capability(&self, _model: &str) -> ImageInputCapability {
            ImageInputCapability::OpenAiChatImageUrl(OpenAiChatImageUrlLimits::default())
        }

        async fn stream(&self, _request: LlmRequest) -> anyhow::Result<LlmResponseStream> {
            anyhow::bail!("not called in this test")
        }
    }

    #[test]
    fn router_forwards_image_input_capability_to_inner() {
        let router = RouterLlmBackend::new(Arc::new(VisionInner), RouterConfig::flash_only());
        assert!(
            router
                .image_input_capability("deepseek-v4-flash-vision-exp")
                .limits()
                .is_some(),
            "router must forward the inner backend's image capability (v7 §3.1)"
        );
    }
}
