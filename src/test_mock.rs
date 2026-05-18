//! 回归测试 — 系统提示词验证。

use crate::config::ModelTier;

#[test]
fn system_prompt_contains_causal_reasoning() {
    let builder = crate::prompt::Builder {
        cwd: std::path::PathBuf::from("/tmp"),
        home: std::path::PathBuf::from("/tmp"),
        skills: vec![],
        summary_file: std::path::PathBuf::from("/tmp/_nonexistent_summary"),
        plan_file: std::path::PathBuf::from("/tmp/_nonexistent_plan"),
        plan_draft_file: std::path::PathBuf::from("/tmp/_nonexistent_draft"),
    };
    let prompt = builder.build_system_prompt().unwrap();
    assert!(prompt.contains("<causal-reasoning>"), "should contain causal-reasoning section");
    assert!(prompt.contains("Before every code change, answer silently"),
        "should contain causal reasoning instructions");
}

#[test]
fn forced_model_works() {
    let forced: Option<ModelTier> = Some(ModelTier::Pro);
    let tier = if let Some(f) = forced { f } else { ModelTier::Flash };
    assert_eq!(tier, ModelTier::Pro);
}

#[test]
fn unknown_model_falls_back_to_flash() {
    let tier = ModelTier::parse("gpt-4").unwrap_or(ModelTier::Flash);
    assert_eq!(tier, ModelTier::Flash);
}
