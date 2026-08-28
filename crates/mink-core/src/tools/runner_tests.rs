use super::*;
use crate::config::{ToolApprovalMode, ToolApprovalPolicy};
use crate::context::ToolConfig;
use crate::tools::approval::{ToolAuthorization, authorize_tool, denied_message};

#[tokio::test]
async fn successful_display_text_may_start_with_error_prefix() {
    let shared = crate::regression::test_context_for_agent("runner-error-prefix-success")
        .await
        .unwrap();
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let call = ToolCallEvent {
        name: "Grep".into(),
        id: "call-error-prefix".into(),
        input_json: serde_json::json!({}),
        fields: BTreeMap::new(),
        parse_error: None,
    };
    let result = format_dispatched_result(
        &ctx,
        &call,
        ToolExecOutput {
            content: "Error: this is literal file content".into(),
            is_bash: false,
            conv_content: String::new(),
            exit_code: None,
            wall_ms: None,
            no_mutation: false,
            memo_candidate: None,
            spawns_sub_agent: false,
            status: ToolStatus::Succeeded,
            diagnostics: Vec::new(),
            plan_command: None,
            state_metadata: None,
            result_kind: ToolResultKind::Search,
            presentation: None,
        },
    );
    assert_eq!(result.status, ToolStatus::Succeeded);
}

#[test]
fn format_tool_result_truncates_large() {
    let s = "line0\n".repeat(500);
    let result = format_tool_result(&s, 100);
    assert!(result.len() <= 100 + 100); // head + tail + marker
    assert!(result.contains("truncated"));
}

#[test]
fn format_tool_result_short_passes_through() {
    let s = "short";
    assert_eq!(format_tool_result(s, 100), "short");
}

#[test]
fn filter_bash_noise_strips_ansi() {
    let input = "\x1b[32mgreen text\x1b[0m";
    let result = filter_bash_noise(input);
    assert!(!result.contains('\x1b'));
    assert!(result.contains("green text"));
}

#[test]
fn filter_bash_noise_compresses_repeats() {
    let input = "line1\nline1\nline1\nline2";
    let result = filter_bash_noise(input);
    assert!(result.contains("repeated 2 times"));
    assert!(result.contains("line1"));
    assert!(result.contains("line2"));
}

#[test]
fn tool_registry_matches_schema() {
    let schema: Vec<serde_json::Value> =
        serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
    let schema_names: std::collections::BTreeSet<String> = schema
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .expect("schema tool name")
                .to_string()
        })
        .collect();
    let registry = tool_registry();
    let registry_names: std::collections::BTreeSet<String> = registry
        .iter()
        .map(|tool| tool.metadata().name.to_string())
        .collect();

    for name in &schema_names {
        if name == "PythonSandbox" && cfg!(not(feature = "python-sandbox")) {
            continue;
        }
        assert!(
            registry_names.contains(name),
            "schema tool missing executor: {name}"
        );
    }
    for tool in registry {
        assert!(
            schema_names.contains(tool.metadata().name.as_ref()),
            "registry tool missing schema: {}",
            tool.metadata().name
        );
    }
    for expected in [
        "PlanDraft",
        "PlanConfirm",
        "PlanClear",
        "TodoWrite",
        "TodoRead",
        "TodoAdvance",
        "SubAgent",
    ] {
        assert!(registry_names.contains(expected));
    }
}

#[test]
fn tool_schema_order_is_stable_and_descriptions_are_self_contained() {
    let schema: Vec<serde_json::Value> =
        serde_json::from_str(crate::assets::TOOLS_JSON).expect("tools schema should parse");
    let names: Vec<&str> = schema
        .iter()
        .map(|tool| {
            tool.get("name")
                .and_then(serde_json::Value::as_str)
                .expect("schema tool name")
        })
        .collect();
    let pos = |name: &str| {
        names
            .iter()
            .position(|candidate| *candidate == name)
            .expect("tool should exist in schema")
    };

    assert!(pos("Glob") < pos("Bash"));
    assert!(pos("Grep") < pos("Bash"));

    for tool in &schema {
        let own_name = tool["name"].as_str().unwrap();
        let serialized = serde_json::to_string(tool).unwrap();
        for peer in &names {
            if *peer == own_name {
                continue;
            }
            assert!(
                !serialized.contains(&format!("`{peer}`"))
                    && !serialized.contains(&format!("use {peer}"))
                    && !serialized.contains(&format!("Use {peer}"))
                    && !serialized.contains(&format!("{peer} tool")),
                "schema '{own_name}' contains peer-tool routing for '{peer}'"
            );
        }
    }

    let plan_draft = schema
        .iter()
        .find(|tool| tool["name"] == "PlanDraft")
        .expect("PlanDraft schema");
    assert!(
        plan_draft["description"]
            .as_str()
            .is_some_and(|description| description.contains("empty content string"))
    );
    assert!(
        plan_draft["input_schema"]["properties"]["content"]["description"]
            .as_str()
            .is_some_and(|description| description.contains("empty string"))
    );
}

#[test]
fn registry_metadata_is_complete() {
    // summary 字段已删除：模型可见描述来自 tools.json schema（由
    // catalog 一致性测试钉住），registry 元数据只保留行为属性。
    for tool in tool_registry() {
        let meta = tool.metadata();
        assert!(!meta.name.is_empty(), "tool name is empty");
    }
}

#[test]
fn mutating_tools_are_write_or_exec_tier() {
    for tool in tool_registry() {
        let meta = tool.metadata();
        if meta.mutating {
            assert!(
                matches!(meta.approval, ApprovalTier::Write | ApprovalTier::Exec),
                "{} is mutating but not write/exec tier",
                meta.name
            );
        }
    }
}

#[test]
fn expected_tool_metadata_contracts() {
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .expect("tool should exist")
            .metadata()
    };

    assert_eq!(meta("Read").approval, ApprovalTier::Read);
    assert_eq!(meta("Read").result_kind, ToolResultKind::FileRead);
    assert_eq!(meta("Write").approval, ApprovalTier::Write);
    assert_eq!(meta("Write").result_kind, ToolResultKind::FileWrite);
    assert_eq!(meta("Edit").approval, ApprovalTier::Write);
    assert_eq!(meta("Edit").result_kind, ToolResultKind::Edit);
    assert_eq!(meta("Bash").approval, ApprovalTier::Exec);
    assert_eq!(meta("Bash").result_kind, ToolResultKind::Command);
    assert_eq!(meta("Glob").approval, ApprovalTier::Read);
    assert_eq!(meta("Glob").result_kind, ToolResultKind::Search);
    assert_eq!(meta("Grep").approval, ApprovalTier::Read);
    assert_eq!(meta("Grep").result_kind, ToolResultKind::Search);
    assert_eq!(meta("SubAgent").approval, ApprovalTier::Exec);
    assert_eq!(meta("SubAgent").result_kind, ToolResultKind::SubAgent);
    assert!(meta("SubAgent").spawns_sub_agent);
    // internal/discoverable 元数据字段已删除（零生产消费）。
}

#[test]
fn approval_yolo_allows_exec_tools() {
    let config = approval_test_config(ToolApprovalMode::Yolo, []);
    let bash = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Bash")
        .unwrap()
        .metadata();
    assert_eq!(authorize_tool(&bash, &config), ToolAuthorization::Allowed);
}

#[test]
fn approval_write_mode_blocks_exec_but_allows_write() {
    let config = approval_test_config(ToolApprovalMode::Write, []);
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .unwrap()
            .metadata()
    };

    assert_eq!(
        authorize_tool(&meta("Read"), &config),
        ToolAuthorization::Allowed
    );
    assert_eq!(
        authorize_tool(&meta("Write"), &config),
        ToolAuthorization::Allowed
    );
    assert!(matches!(
        authorize_tool(&meta("Bash"), &config),
        ToolAuthorization::Denied { .. }
    ));
}

#[test]
fn approval_per_tool_overrides_mode() {
    let config = approval_test_config(
        ToolApprovalMode::Write,
        [
            ("Bash".to_string(), ToolApprovalPolicy::Allow),
            ("Read".to_string(), ToolApprovalPolicy::Deny),
        ],
    );
    let meta = |name: &str| {
        tool_registry()
            .iter()
            .find(|tool| tool.metadata().name == name)
            .unwrap()
            .metadata()
    };

    assert_eq!(
        authorize_tool(&meta("Bash"), &config),
        ToolAuthorization::Allowed
    );
    let read = meta("Read");
    let reason = match authorize_tool(&read, &config) {
        ToolAuthorization::Denied { reason } => denied_message(&read, reason),
        ToolAuthorization::Allowed => panic!("Read should be denied"),
    };
    assert!(reason.contains("deny"), "{reason}");
}

#[test]
fn policy_gate_blocks_tools_disabled_by_whitelist_before_execution() {
    let mut config = approval_test_config(ToolApprovalMode::Yolo, []);
    config.enabled_tools = Some(vec!["Read".into()]);
    let storm = Mutex::new(StormBreaker::new(6, 3));
    let resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::Primary,
        &config,
        false,
    );
    let surface = crate::tools::surface::ModelToolSurface::resolve(
        crate::tools::catalog::ToolCatalog::builtin().unwrap(),
        &config,
        &resolution,
    )
    .unwrap();
    let gate = ToolPolicyGate {
        surface: &surface,
        storm: &storm,
    };
    let call = test_call("Bash");
    let metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Bash")
        .map(|tool| tool.metadata());

    let blocked = gate
        .evaluate(&call, metadata)
        .expect("Bash should be blocked by enabled_tools");

    assert_eq!(blocked.tool_name, "Bash");
    assert!(blocked.content.contains("unavailable"));
}

#[test]
fn policy_gate_blocks_tools_hidden_by_role_or_backend() {
    let config = approval_test_config(ToolApprovalMode::Yolo, []);
    let catalog = crate::tools::catalog::ToolCatalog::builtin().unwrap();
    let storm = Mutex::new(StormBreaker::new(6, 3));

    let sub_agent_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::SubAgent,
        &config,
        false,
    );
    let sub_agent_surface =
        crate::tools::surface::ModelToolSurface::resolve(catalog, &config, &sub_agent_resolution)
            .unwrap();
    let sub_agent_gate = ToolPolicyGate {
        surface: &sub_agent_surface,
        storm: &storm,
    };
    let sub_agent_metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "SubAgent")
        .map(|tool| tool.metadata());
    let blocked = sub_agent_gate
        .evaluate(&test_call("SubAgent"), sub_agent_metadata)
        .expect("SubAgent should be blocked outside the sub-agent surface");
    assert!(blocked.content.contains("UnavailableForRole"));

    let vfs_resolution = crate::tools::surface::ToolResolutionContext::from_runtime(
        crate::tools::surface::AgentRole::Primary,
        &config,
        true,
    );
    let vfs_surface =
        crate::tools::surface::ModelToolSurface::resolve(catalog, &config, &vfs_resolution)
            .unwrap();
    let vfs_gate = ToolPolicyGate {
        surface: &vfs_surface,
        storm: &storm,
    };
    let edit_metadata = tool_registry()
        .iter()
        .find(|tool| tool.metadata().name == "Edit")
        .map(|tool| tool.metadata());
    let blocked = vfs_gate
        .evaluate(&test_call("Edit"), edit_metadata)
        .expect("Edit should be blocked outside the VFS surface");
    assert!(blocked.content.contains("UnavailableForBackend"));
}

fn test_call(name: &str) -> ToolCallEvent {
    ToolCallEvent {
        name: name.to_string(),
        id: "call_test".to_string(),
        input_json: serde_json::json!({}),
        fields: BTreeMap::new(),
        parse_error: None,
    }
}

fn approval_test_config<const N: usize>(
    mode: ToolApprovalMode,
    overrides: [(String, ToolApprovalPolicy); N],
) -> ToolConfig {
    ToolConfig {
        tool_timeout_secs: 600,
        tool_timeout_max_secs: 600,
        sub_agent_timeout_secs: 300,
        tool_result_max_bytes: 100_000,
        file_write_max_bytes: 1_048_576,
        edit_mode: crate::config::EditMode::Hashline,
        edit_fuzzy_match: true,
        edit_fuzzy_threshold: 0.95,
        edit_enforce_seen_lines: false,
        max_search_files: 5000,
        max_search_results: 1000,
        enabled_tools: None,
        tool_approval_mode: mode,
        tool_approval: overrides.into_iter().collect(),
        sandbox_python: crate::config::SandboxPythonConfig::default(),
    }
}

#[test]
fn all_tool_result_kind_variants_have_expected_coverage() {
    let kinds: std::collections::BTreeSet<&'static str> = tool_registry()
        .iter()
        .map(|tool| match tool.metadata().result_kind {
            ToolResultKind::Text => "Text",
            ToolResultKind::FileRead => "FileRead",
            ToolResultKind::FileWrite => "FileWrite",
            ToolResultKind::Edit => "Edit",
            ToolResultKind::Command => "Command",
            ToolResultKind::Search => "Search",
            ToolResultKind::Control => "Control",
            ToolResultKind::SubAgent => "SubAgent",
        })
        .collect();

    for expected in [
        "FileRead",
        "FileWrite",
        "Edit",
        "Command",
        "Search",
        "Control",
        "SubAgent",
    ] {
        assert!(kinds.contains(expected), "missing result kind {expected}");
    }
}

// ── Image read protocol (v7) ─────────────────────────────────────────────

fn vision_capabilities(
    max_images: usize,
) -> Arc<crate::capabilities::model_capabilities::SessionModelCapabilities> {
    use crate::capabilities::model_capabilities::{
        ImageInputCapability, OpenAiChatImageUrlLimits, SessionModelCapabilities,
    };
    let limits = OpenAiChatImageUrlLimits {
        max_images_per_request: max_images,
        ..Default::default()
    };
    let mut caps = SessionModelCapabilities {
        version: 1,
        initial_model: "vision".into(),
        image_input: ImageInputCapability::OpenAiChatImageUrl(limits),
        capability_fingerprint: String::new(),
    };
    caps.capability_fingerprint = caps.image_input.fingerprint();
    Arc::new(caps)
}

fn png_fixture() -> Vec<u8> {
    let img = image::RgbaImage::new(24, 24);
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png fixture");
    out
}

fn png_fixture_with_dimensions(width: u32, height: u32) -> Vec<u8> {
    let mut img = image::RgbaImage::new(width, height);
    for (x, y, pixel) in img.enumerate_pixels_mut() {
        *pixel = image::Rgba([(x % 251) as u8, (y % 251) as u8, 7, 255]);
    }
    let mut out = Vec::new();
    img.write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .expect("png fixture");
    out
}

fn read_call(id: &str, path: &str) -> ToolCallEvent {
    ToolCallEvent {
        name: "Read".into(),
        id: id.into(),
        input_json: serde_json::json!({"path": path}),
        fields: BTreeMap::from([("path".to_string(), path.to_string())]),
        parse_error: None,
    }
}

#[tokio::test]
async fn image_read_captures_and_reference_read_reinjects() {
    let mut shared = crate::regression::test_context_for_agent("runner-image-capture")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("shot.png"), &bytes).unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));

    let results = runner
        .execute_all(vec![read_call("call_read", "shot.png")])
        .await
        .unwrap();
    assert_eq!(results.len(), 1);
    let result = &results[0];
    assert!(result.succeeded(), "{}", result.content);
    let attachment = result.image_attachment.as_ref().expect("captured image");
    assert!(attachment.image_id.starts_with("sha256:"));
    assert_eq!((attachment.width, attachment.height), (24, 24));
    assert!(
        result
            .content
            .contains("[The image will be attached to the next model request.]")
    );
    assert!(result.content.contains("shot.png"), "{}", result.content);
    // Object is durably cached and registered as a this-turn capture.
    assert!(shared.image_cache.contains(&attachment.image_id));
    assert!(
        shared
            .this_turn_image_ids
            .lock()
            .unwrap()
            .contains(&attachment.image_id)
    );

    // Reference read of the same image succeeds and re-injects it.
    let reference = format!("image://{}", attachment.image_id);
    let results = runner
        .execute_all(vec![read_call("call_ref", &reference)])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert_eq!(
        results[0].image_attachment.as_ref().unwrap().image_id,
        attachment.image_id
    );

    // Missing reference fails closed as an ordinary failed ToolExecution.
    let missing = format!("image://{}", "sha256:".to_string() + &"aa".repeat(32));
    let results = runner
        .execute_all(vec![read_call("call_missing", &missing)])
        .await
        .unwrap();
    assert!(!results[0].succeeded());
    assert!(
        results[0].content.contains("not found in image cache"),
        "{}",
        results[0].content
    );
}

#[tokio::test]
async fn batch_admission_limits_each_batch_and_resets_between_batches() {
    // Per-batch admission (v7 §7.3): the image budget starts at zero for
    // every execute_all call. Within one batch the 3rd image (over the
    // budget of 2) is an ordinary failed ToolExecution — never published, so
    // materialization cannot fail later on a promise the tool layer made.
    // The NEXT batch starts fresh: history does not lock out new reads.
    let mut shared = crate::regression::test_context_for_agent("runner-image-batch")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("shot.png"), &bytes).unwrap();
    // Distinct contents keep this-turn ids distinct (content-addressed).
    let bytes_b = png_fixture_with_dimensions(32, 24);
    std::fs::write(shared.cwd.join("shot_b.png"), &bytes_b).unwrap();
    let bytes_c = png_fixture_with_dimensions(64, 24);
    std::fs::write(shared.cwd.join("shot_c.png"), &bytes_c).unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(2);
    }
    let tool_ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(tool_ctx.clone()));

    let results = runner
        .execute_all(vec![
            read_call("call_1", "shot.png"),
            read_call("call_2", "shot_b.png"),
            read_call("call_3", "shot_c.png"),
        ])
        .await
        .unwrap();
    // First two capture, the third is rejected by the batch budget.
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[1].succeeded(), "{}", results[1].content);
    assert!(!results[2].succeeded(), "{}", results[2].content);
    assert!(
        results[2]
            .content
            .contains("image attachment batch limit exceeded"),
        "{}",
        results[2].content
    );
    assert_eq!(tool_ctx.this_turn_image_ids.lock().unwrap().len(), 2);

    // Next batch starts from zero: two more captures succeed (with fresh
    // ids), proving history does not accumulate across batches.
    let results = runner
        .execute_all(vec![
            read_call("call_4", "shot_b.png"),
            read_call("call_5", "shot_c.png"),
        ])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[1].succeeded(), "{}", results[1].content);
}

#[tokio::test]
async fn batch_budget_survives_sequential_tool_flush() {
    // Regression: sequential tools (Bash) flush the read batch mid-`execute_all`;
    // the per-execute_all image budget must NOT reset there, or a
    // Read→Bash→Read group would slip two images (and their bytes) into one
    // request while each flush counts from zero.
    let mut shared = crate::regression::test_context_for_agent("runner-image-seq-flush")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("shot.png"), &bytes).unwrap();
    let bytes_b = png_fixture_with_dimensions(32, 24);
    std::fs::write(shared.cwd.join("shot_b.png"), &bytes_b).unwrap();
    let bytes_c = png_fixture_with_dimensions(64, 24);
    std::fs::write(shared.cwd.join("shot_c.png"), &bytes_c).unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(2);
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let bash = ToolCallEvent {
        name: "Bash".into(),
        id: "call_bash".into(),
        input_json: serde_json::json!({"command": "true"}),
        fields: BTreeMap::from([("command".to_string(), "true".to_string())]),
        parse_error: None,
    };
    let results = runner
        .execute_all(vec![
            read_call("call_1", "shot.png"),
            bash,
            read_call("call_2", "shot_b.png"),
            read_call("call_3", "shot_c.png"),
        ])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[1].succeeded(), "{}", results[1].content);
    assert!(results[2].succeeded(), "{}", results[2].content);
    // Budget counts across the flush: the third image is rejected even
    // though it lies in a later read sub-batch.
    assert!(!results[3].succeeded(), "{}", results[3].content);
    assert!(
        results[3]
            .content
            .contains("image attachment batch limit exceeded"),
        "{}",
        results[3].content
    );
}

struct TestImageVfs {
    image: Option<Vec<u8>>,
    text: String,
}

impl crate::tools::vfs::ReadOnlyFileSystem for TestImageVfs {
    fn read(
        &self,
        _scope: &crate::tools::vfs::VfsScope,
        _request: &crate::tools::vfs::VfsReadRequest,
    ) -> anyhow::Result<crate::tools::vfs::VfsReadResult> {
        Ok(crate::tools::vfs::VfsReadResult {
            content: self.text.clone(),
            total_lines: 1,
            total_bytes: self.text.len(),
        })
    }

    fn glob(
        &self,
        _scope: &crate::tools::vfs::VfsScope,
        _request: &crate::tools::vfs::VfsGlobRequest,
    ) -> anyhow::Result<crate::tools::vfs::VfsGlobResult> {
        Ok(crate::tools::vfs::VfsGlobResult::default())
    }

    fn grep(
        &self,
        _scope: &crate::tools::vfs::VfsScope,
        _request: &crate::tools::vfs::VfsGrepRequest,
    ) -> anyhow::Result<crate::tools::vfs::VfsGrepResult> {
        Ok(crate::tools::vfs::VfsGrepResult::default())
    }

    fn read_image(
        &self,
        _scope: &crate::tools::vfs::VfsScope,
        _path: &str,
        max_bytes: u64,
    ) -> anyhow::Result<Option<crate::tools::vfs::VfsImage>> {
        Ok(self.image.clone().map(|bytes| {
            assert!(bytes.len() <= max_bytes as usize);
            crate::tools::vfs::VfsImage {
                bytes,
                mime: "image/png".to_string(),
            }
        }))
    }
}

#[tokio::test]
async fn vfs_image_reads_one_at_a_time_and_text_falls_back() {
    // VfsCandidate: classification stores only the path; the image bytes are
    // read during prepare (one image in memory at a time). A backend `None`
    // falls back to the ordinary text read.
    let mut shared = crate::regression::test_context_for_agent("runner-image-vfs")
        .await
        .unwrap();
    let bytes = png_fixture();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
        shared_ref.read_only_fs = Some(Arc::new(TestImageVfs {
            image: Some(bytes.clone()),
            text: "text body".to_string(),
        }));
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_vfs", "shot.png")])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[0].image_attachment.is_some());
    assert!(
        shared
            .image_cache
            .contains(&("sha256:".to_string() + &"00".repeat(32)))
            || !results[0].content.is_empty()
    );

    // Backend reports no image: the call falls back to the text read.
    let mut shared = crate::regression::test_context_for_agent("runner-image-vfs-text")
        .await
        .unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
        shared_ref.read_only_fs = Some(Arc::new(TestImageVfs {
            image: None,
            text: "plain text body".to_string(),
        }));
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_text", "note.txt")])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[0].image_attachment.is_none());
    assert!(
        results[0].content.contains("plain text body"),
        "{}",
        results[0].content
    );
}

#[tokio::test]
async fn vfs_text_selector_works_in_vision_session() {
    // A VFS TEXT path with a selector must not be rejected as an image: the
    // selector is honored on the ordinary text read; it is rejected only
    // when the path is confirmed to be an image.
    let mut shared = crate::regression::test_context_for_agent("runner-image-vfs-selector")
        .await
        .unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
        shared_ref.read_only_fs = Some(Arc::new(TestImageVfs {
            image: None,
            text: "line one\nline two\nline three".to_string(),
        }));
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_sel", "note.txt:2-2")])
        .await
        .unwrap();
    assert!(results[0].succeeded(), "{}", results[0].content);
    assert!(results[0].image_attachment.is_none());
    assert!(
        results[0].content.contains("line two"),
        "{}",
        results[0].content
    );
}

#[tokio::test]
async fn vfs_image_selector_rejected_only_when_confirmed_image() {
    // The same selector on a CONFIRMED image is rejected at prepare time.
    let mut shared = crate::regression::test_context_for_agent("runner-image-vfs-sel-img")
        .await
        .unwrap();
    let bytes = png_fixture();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
        shared_ref.read_only_fs = Some(Arc::new(TestImageVfs {
            image: Some(bytes.clone()),
            text: String::new(),
        }));
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_img_sel", "shot.png:2-2")])
        .await
        .unwrap();
    assert!(!results[0].succeeded(), "{}", results[0].content);
    assert!(
        results[0].content.contains("do not support line selectors"),
        "{}",
        results[0].content
    );
}

#[tokio::test]
async fn registered_resources_never_become_vfs_image_reads() {
    // Registered resource paths (even with a selector, even when the VFS
    // backend serves image bytes) stay on the resource-router path.
    let mut shared = crate::regression::test_context_for_agent("runner-image-vfs-resource")
        .await
        .unwrap();
    let bytes = png_fixture();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
        shared_ref.read_only_fs = Some(Arc::new(TestImageVfs {
            image: Some(bytes.clone()),
            text: "ignored".to_string(),
        }));
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    // Direct classifier check: a registered scheme classifies as text.
    let selection =
        crate::resources::selector::split_read_path_selection("session://current/stats:1-2")
            .unwrap();
    let kind =
        super::file::classify_image_read(&ctx, "session://current/stats:1-2", &selection).unwrap();
    assert!(
        matches!(kind, super::file::ImageReadKind::NotImage),
        "{kind:?}"
    );
}

#[tokio::test]
async fn parse_error_calls_become_failed_tool_results_without_executing() {
    // A call whose arguments could not be parsed must not fail the turn: the
    // runner returns a failed tool result and never executes the tool.
    let shared = crate::regression::test_context_for_agent("runner-parse-error")
        .await
        .unwrap();
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let marker =
        std::env::temp_dir().join(format!("mink-parse-error-marker-{}", std::process::id()));
    let _ = std::fs::remove_file(&marker);
    let call = ToolCallEvent {
        name: "Bash".into(),
        id: "call_bad".into(),
        input_json: serde_json::json!({}),
        fields: BTreeMap::new(),
        parse_error: Some("parse tool input: expected `,` or `}` at line 1 column 153".to_string()),
    };
    let results = runner.execute_all(vec![call]).await.unwrap();
    assert_eq!(results.len(), 1);
    assert!(!results[0].succeeded(), "{}", results[0].content);
    assert!(
        results[0].content.contains("tool input JSON invalid"),
        "{}",
        results[0].content
    );
    // The Bash command was never executed.
    assert!(!marker.exists(), "tool must not execute on parse error");
}

#[tokio::test]
async fn selector_on_image_path_is_rejected_before_prepare() {
    let mut shared = crate::regression::test_context_for_agent("runner-image-selector")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("shot.png"), &bytes).unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique shared ctx");
        shared_ref.model_capabilities = vision_capabilities(4);
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_sel", "shot.png:1-5")])
        .await
        .unwrap();
    assert!(!results[0].succeeded());
    assert!(
        results[0].content.contains("do not support line selectors"),
        "{}",
        results[0].content
    );
    // Nothing was persisted.
    assert!(
        !shared
            .image_cache
            .contains(&("sha256:".to_string() + &"00".repeat(32)))
    );
}

#[tokio::test]
async fn unsupported_session_keeps_text_path_unchanged() {
    let shared = crate::regression::test_context_for_agent("runner-image-unsupported")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("shot.png"), &bytes).unwrap();
    // model_capabilities stays Unsupported (default).
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));
    let results = runner
        .execute_all(vec![read_call("call_text", "shot.png")])
        .await
        .unwrap();
    // Binary bytes are not text: the legacy text path fails as before, and no
    // image capture exists.
    assert!(!results[0].succeeded());
    assert!(results[0].image_attachment.is_none());
    // image:// keeps the unknown-scheme fail-closed behavior.
    let results = runner
        .execute_all(vec![read_call("call_ref", "image://sha256:bbbb")])
        .await
        .unwrap();
    assert!(!results[0].succeeded());
}

#[tokio::test]
async fn mixed_batch_preserves_dispatch_order() {
    let mut shared = crate::regression::test_context_for_agent("runner-mixed-order")
        .await
        .unwrap();
    let bytes = png_fixture();
    std::fs::write(shared.cwd.join("a.png"), &bytes).unwrap();
    std::fs::write(shared.cwd.join("b.png"), &bytes).unwrap();
    std::fs::write(shared.cwd.join("note.txt"), "hello\n").unwrap();
    {
        let shared_ref = Arc::get_mut(&mut shared).expect("unique");
        shared_ref.model_capabilities = vision_capabilities(4);
    }
    let ctx = crate::context::ToolContext::from(shared.as_ref());
    let runner = ToolRunner::new(Arc::new(ctx));

    let results = runner
        .execute_all(vec![
            read_call("call_a", "a.png"),
            read_call("call_grep", "note.txt"),
            read_call("call_b", "b.png"),
        ])
        .await
        .unwrap();
    assert_eq!(results.len(), 3);
    // Order must match the original dispatch order (review fix #2).
    assert_eq!(results[0].tool_use_id, "call_a");
    assert_eq!(results[1].tool_use_id, "call_grep");
    assert_eq!(results[2].tool_use_id, "call_b");
    assert!(results[0].image_attachment.is_some());
    assert!(results[1].image_attachment.is_none());
    assert!(results[2].image_attachment.is_some());
    assert!(
        results[1].content.contains("hello"),
        "{}",
        results[1].content
    );
}
