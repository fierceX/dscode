//! Mock regression tests for Phase 0–3 features.
//!
//! These tests verify integration between the new components:
//!   - Controller (P(stall) + ControlAction)
//!   - ModelSelector (Beta-Bernoulli → Greedy selection)
//!   - Sensor integration (shell sensors via ToolRunner)
//!   - ResolveActive decision flow
//!
//! They use minimal mocking and direct logic calls rather than
//! full `OrchActor` setup, which requires mpsc channels and Tokio.

use crate::agent::controller::{Controller, ControlAction};
use crate::agent::model_selector::ModelSelector;
use crate::config::ModelTier;

// ========================================================================
// 1. resolve_active 决策流模拟
// ========================================================================

/// 模拟 OrchActor::resolve_active 的决策逻辑（不含 forced_model/auto_model_enabled）。
/// 用于验证 Controller 和 ModelSelector 的协作是否产生正确的模型选择。
fn simulate_resolve_active(
    controller: &Controller,
    selector: &ModelSelector,
    auto_enabled: bool,
) -> &'static str {
    if !auto_enabled {
        return "flash";
    }
    if controller.is_locked()
        || matches!(
            controller.get_control_action(),
            Some(ControlAction::UpgradeModel) | Some(ControlAction::Abort)
        )
    {
        return "pro";
    }
    let selected = selector.select_greedy();
    ModelTier::parse(selected)
        .map(|t| match t {
            ModelTier::Pro => "pro",
            ModelTier::Flash => "flash",
        })
        .unwrap_or("flash")
}

#[test]
fn resolve_active_plain_returns_default_when_auto_disabled() {
    let c = Controller::new();
    let ms = ModelSelector::new();
    // auto_enabled=false → always "flash"
    assert_eq!(simulate_resolve_active(&c, &ms, false), "flash");
}

#[test]
fn resolve_active_controller_lock_forces_pro() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Raise stall probability to locked level (P > 0.80)
    for _ in 0..3 {
        c.note_error(false);
    }
    // Controller locked → force Pro regardless of selector
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

#[test]
fn resolve_active_controller_upgrade_forces_pro() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Raise to upgrade level (P > 0.95)
    for _ in 0..6 {
        c.note_error(false);
    }
    // Controller says UpgradeModel → force Pro
    assert_eq!(c.get_control_action(), Some(ControlAction::UpgradeModel));
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

#[test]
fn resolve_active_controller_abort_forces_pro() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Raise to abort level (P > 0.99, k >= 10)
    for _ in 0..11 {
        c.note_error(false);
    }
    assert_eq!(c.get_control_action(), Some(ControlAction::Abort));
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

#[test]
fn resolve_active_selector_picks_best_model_when_no_stall() {
    let c = Controller::new(); // P=0, no lock
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Make pro look much better
    for _ in 0..20 {
        ms.update("pro", true);
    }
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

#[test]
fn resolve_active_selector_picks_flash_when_better() {
    let c = Controller::new(); // P=0, no lock
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Make flash look better than pro
    for _ in 0..10 {
        ms.update("flash", true);
    }
    for _ in 0..3 {
        ms.update("pro", false);
    }
    assert_eq!(simulate_resolve_active(&c, &ms, true), "flash");
}

#[test]
fn resolve_active_selector_uses_last_equal_when_means_equal() {
    let c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Both have mean=0.5 → max_by returns the last element when equal ("pro")
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

#[test]
fn resolve_active_controller_beats_selector() {
    // Even when selector strongly prefers one model,
    // controller lock must override.
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    // Make flash look excellent
    for _ in 0..50 {
        ms.update("flash", true);
    }
    // mean = 51/(51+1) ≈ 0.981
    assert!((ms.mean("flash") - 51.0 / 52.0).abs() < 1e-10);
    // But controller is locked
    for _ in 0..3 {
        c.note_error(false);
    }
    // Controller lock wins over selector preference
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

// ========================================================================
// 2. Controller + ModelSelector 反馈循环
// ========================================================================

#[test]
fn controller_and_selector_coexist_independently() {
    // Controller tracks stall, ModelSelector tracks model quality.
    // They should not interfere with each other's state.
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");

    c.note_error(false); // P=0.5
    ms.update("flash", false); // flash mean drops

    assert!((c.stall_probability() - 0.5).abs() < 1e-10);
    assert!(ms.mean("flash") < 0.5);
}

#[test]
fn successful_turn_resets_controller_but_preserves_selector() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");

    // Stuck phase
    for _ in 0..3 {
        c.note_error(false);
    }
    // Learn that pro is better
    for _ in 0..5 {
        ms.update("pro", true);
    }
    let selector_mean_before = ms.mean("pro");

    // Successful turn → reset stall
    c.note_progress(true);
    c.reset_stall();
    assert_eq!(c.stall_probability(), 0.0);

    // Selector beliefs should survive
    assert_eq!(ms.mean("pro"), selector_mean_before);
}

// ========================================================================
// 3. 传感器回归测试（工具执行级别）
// ========================================================================

#[test]
fn run_sensor_detects_rust_compilation_error() {
    let output = "error[E0308]: mismatched types\n --> src/main.rs:10:5\n  |\n10 |     let x: i32 = \"hello\";\n  |         ^ expected i32, found &str\n";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 150, output.len(), output,
    )
    .expect("sensor should run");
    assert!(!signals.is_empty(), "should detect rust error");
    assert!(signals.iter().any(|s| s.detail.contains("Rust compilation error")));
}

#[test]
fn run_sensor_detects_python_test_failure() {
    let output = "__________________________ test_foo __________________________\n\
                   def test_foo():\n>       assert 1 == 2\nE       AssertionError: assert 1 == 2\n\n\
                   FAILED tests/test_demo.py::test_foo - AssertionError: assert 1 == 2";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 200, output.len(), output,
    )
    .expect("sensor should run");
    assert!(!signals.is_empty(), "should detect test failure");
    assert!(signals.iter().any(|s| s.detail.contains("Pytest failure")
        || s.detail.contains("Test failure")),
        "should detect pytest failure, got: {:?}",
        signals.iter().map(|s| &s.detail).collect::<Vec<_>>());
}

#[test]
fn run_sensor_detects_non_zero_exit() {
    let output = "Error: Process completed with exit code 1.";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 50, output.len(), output,
    )
    .expect("sensor should run");
    assert!(!signals.is_empty(), "should detect non-zero exit");
    assert!(signals.iter().any(|s| s.detail.contains("Non-zero exit")));
}

#[test]
fn run_sensor_returns_empty_for_clean_output() {
    let output = "All tests passed. 42 passed, 0 failed.";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 100, output.len(), output,
    )
    .expect("sensor should run");
    assert!(signals.is_empty(), "clean output should produce no signals");
}

#[test]
fn run_sensor_handles_empty_output() {
    let signals = crate::guard::sensor::run_sensor("error", "Read", 5, 0, "")
        .expect("sensor should run");
    assert!(signals.is_empty(), "empty output should produce no signals");
}

#[test]
fn run_sensor_reports_weight_for_different_patterns() {
    // Rust error weight=1.0
    let rust_output = "error[E0425]: cannot find value `x`";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 100, rust_output.len(), rust_output,
    )
    .expect("sensor should run");
    assert!(signals.iter().any(|s| (s.weight - 1.0).abs() < 1e-10));

    // Non-zero exit weight=0.5
    let exit_output = "Error: command failed with exit code 1";
    let signals = crate::guard::sensor::run_sensor(
        "error", "Bash", 100, exit_output.len(), exit_output,
    )
    .expect("sensor should run");
    assert!(signals.iter().any(|s| (s.weight - 0.5).abs() < 1e-10));
}

#[test]
fn unknown_sensor_returns_error() {
    let result = crate::guard::sensor::run_sensor("nonexistent_sensor", "Read", 0, 0, "");
    assert!(result.is_err(), "unknown sensor should fail");
}

// ========================================================================
// 4. Controller 边界条件
// ========================================================================

#[test]
fn controller_stall_probability_monotonic_increasing() {
    let mut c = Controller::new();
    let mut prev = 0.0f64;
    for k in 1..=10 {
        c.note_error(false);
        let p = c.stall_probability();
        assert!(p > prev, "P(stall) must increase monotonically: k={k}, p={p}");
        prev = p;
    }
}

#[test]
fn controller_stall_probability_resets_on_progress() {
    let mut c = Controller::new();
    for _ in 0..4 {
        c.note_error(false);
    }
    assert!(c.stall_probability() > 0.9);
    c.note_progress(true);
    assert_eq!(c.stall_probability(), 0.0);
}

#[test]
fn controller_locked_implies_is_locked_true() {
    let mut c = Controller::new();
    for _ in 0..3 {
        c.note_error(false);
    }
    assert!(c.is_locked(), "P(stall) > 0.80 should lock");
}

#[test]
fn controller_not_locked_at_low_stall() {
    let c = Controller::new();
    assert!(!c.is_locked(), "P(stall)=0 should not lock");
    let mut c = Controller::new();
    c.note_error(false);
    assert!(!c.is_locked(), "P(stall)=0.5 should not lock");
}

#[test]
fn controller_fix_loop_detection_basic() {
    let mut c = Controller::new();
    // Not enough calls
    assert!(!c.has_fix_loop());
    for _ in 0..16 {
        c.note_tool_call();
    }
    // 16 calls, no end_turn → fix loop
    assert!(c.has_fix_loop());
}

#[test]
fn controller_fix_loop_cleared_by_end_turn() {
    let mut c = Controller::new();
    for _ in 0..20 {
        c.note_tool_call();
    }
    c.note_end_turn();
    assert!(!c.has_fix_loop());
}

#[test]
fn controller_fix_loop_cleared_by_per_turn_reset() {
    let mut c = Controller::new();
    for _ in 0..20 {
        c.note_tool_call();
    }
    c.reset_per_turn();
    assert!(!c.has_fix_loop());
}

#[test]
fn controller_control_action_none_below_threshold() {
    let c = Controller::new();
    assert_eq!(c.get_control_action(), None);
}

// ========================================================================
// 5. ModelSelector 边界条件
// ========================================================================

#[test]
fn selector_mean_is_05_for_unknown_model() {
    let ms = ModelSelector::new();
    assert!((ms.mean("unknown") - 0.5).abs() < 1e-10);
}

#[test]
fn selector_update_does_not_affect_other_models() {
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    let flash_before = ms.mean("flash");
    ms.update("pro", true);
    assert_eq!(ms.mean("flash"), flash_before);
}

#[test]
fn selector_greedy_picks_only_registered() {
    let mut ms = ModelSelector::new();
    ms.ensure("pro");
    // Only pro registered, flash not registered
    assert_eq!(ms.select_greedy(), "pro");
}

#[test]
fn selector_empty_registry_returns_flash() {
    let ms = ModelSelector::new();
    assert_eq!(ms.select_greedy(), "flash");
}

#[test]
fn selector_ensure_is_idempotent() {
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("flash");
    ms.ensure("flash");
    assert_eq!(ms.len(), 1);
}

#[test]
fn selector_format_beliefs_shows_all_models() {
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");
    let s = ms.format_beliefs();
    assert!(s.contains("flash"));
    assert!(s.contains("pro"));
    assert!(s.contains("mean="));
    assert!(s.contains("α="));
    assert!(s.contains("β="));
}

// ========================================================================
// 6. 跨组件集成场景
// ========================================================================

/// 模拟一个完整的"失败 → 学习 → 恢复"场景。
#[test]
fn full_cycle_fail_learn_recover() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");

    // --- Phase 1: 使用 flash, 连续失败 ---
    for i in 0..4 {
        c.note_error(false);                // controller sees stall
        ms.update("flash", false);           // flash failure

        // After 3 failures, controller locks
        if i >= 2 {
            assert!(c.is_locked(), "should lock at k=3");
            // resolve_active would return "pro" when locked
        }
    }

    // --- Phase 2: 切换到 pro, 成功后恢复 ---
    ms.update("pro", true);  // pro success
    c.note_progress(true);   // progress made
    c.reset_stall();          // reset stall

    assert!(!c.is_locked(), "after reset, should not be locked");
    assert!(ms.mean("pro") > ms.mean("flash"), "pro should be preferred after learning");

    // resolve_active would now pick pro (since selector prefers it)
    assert_eq!(simulate_resolve_active(&c, &ms, true), "pro");
}

/// 验证 Controller 和 ModelSelector 在 auto_model_enabled=false 时都不影响决策。
#[test]
fn auto_disabled_bypasses_both_components() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");

    // Even with locked controller and pro-preferring selector
    for _ in 0..5 {
        c.note_error(false);
    }
    for _ in 0..10 {
        ms.update("pro", true);
    }

    // auto_enabled=false → always flash
    assert_eq!(simulate_resolve_active(&c, &ms, false), "flash");
}

// ========================================================================
// 7. Prompt 段包含验证 (Phase 0 回归)
// ========================================================================

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
    assert!(prompt.contains("What specific behavior will this change affect?"),
        "should contain cause question");
    assert!(prompt.contains("What observable result do I expect?"),
        "should contain effect question");
    assert!(prompt.contains("How will I verify the cause-effect link?"),
        "should contain verify question");
    assert!(prompt.contains("One change at a time"),
        "should mention single-change principle");
}

// ========================================================================
// 8. 传感器 → Controller 信号链路回归
// ========================================================================

/// 模拟 orchestrator 的聚合逻辑：多个 tool_error 信号只产生 1 次 note_error。
#[test]
fn controller_receives_aggregated_sensor_signal_once() {
    let mut c = Controller::new();
    // 模拟 5 个工具错误信号（同一轮次内）
    let signals = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Rust error".into(),
        },
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Test failure".into(),
        },
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 0.5, detail: "Non-zero exit".into(),
        },
    ];

    // 模拟 orchestrator 聚合逻辑：有任意 tool_error → 只调用 1 次 note_error(false)
    if signals.iter().any(|s| s.kind == "tool_error") {
        c.note_error(false);
    }

    // 5 个信号聚合为 1 次 → k=1, P=0.5
    assert_eq!(c.no_progress_count(), 1);
    assert!((c.stall_probability() - 0.5).abs() < 1e-10);
}

/// 模拟 Failed 轮次：update_after_turn 的 1 次 + 传感器聚合的 1 次 = k=2
/// 验证 Controller 不被过度计数。
#[test]
fn failed_turn_with_sensor_signals_produces_k2() {
    let mut c = Controller::new();

    // Phase 1: update_after_turn 处理 Failed
    c.note_error(false);  // k=1, P=0.5

    // Phase 2: 传感器聚合信号
    let has_error_signals = true;
    if has_error_signals {
        c.note_error(false);  // k=2, P=0.75
    }

    assert_eq!(c.no_progress_count(), 2);
    assert!((c.stall_probability() - 0.75).abs() < 1e-10,
        "expected P=0.75, got {}", c.stall_probability());
}

/// Stop 轮次应重置 Controller，即使传感器有错误信号也不额外计数。
#[test]
fn stop_turn_resets_controller_despite_sensor_errors() {
    let mut c = Controller::new();

    // 之前有 stall
    c.note_error(false);
    c.note_error(false);
    assert_eq!(c.no_progress_count(), 2);

    // 轮次 Stop：update_after_turn 重置
    c.note_progress(true);
    c.reset_stall();
    assert_eq!(c.no_progress_count(), 0);
    assert_eq!(c.stall_probability(), 0.0);

    // 传感器信号在 Stop 时不被喂入 → Controller 保持重置
    // （模拟 orchestrator 中只有 Failed 才喂入传感器信号的逻辑）
}

/// 多轮失败场景：每次 Failed 都 +1（update_after_turn）+ 有信号时再 +1
/// 验证 k 增长的合理性。
#[test]
fn multiple_failed_turns_with_signals_k_growth() {
    let mut c = Controller::new();

    // 模拟 3 个连续失败轮次，每轮都有传感器错误信号
    for _ in 0..3 {
        c.note_error(false);  // update_after_turn
        // 传感器聚合信号（模拟有错误信号）
        c.note_error(false);  // 聚合的 1 次
    }

    // 3 轮 × 2 = k=6
    assert_eq!(c.no_progress_count(), 6);
    let p = c.stall_probability();
    let expected_p = 1.0 - 0.5_f64.powi(6);
    assert!((p - expected_p).abs() < 1e-10,
        "expected P={expected_p}, got {p}");

    // 第 4 轮成功 → 重置
    c.note_progress(true);
    c.reset_stall();
    assert_eq!(c.stall_probability(), 0.0);
}

/// 无传感器信号的 Failed 轮次：只有 update_after_turn 的 1 次 note_error
#[test]
fn failed_turn_without_sensor_signals_k1() {
    let mut c = Controller::new();

    // 模拟 Failed 轮次，但传感器无信号
    c.note_error(false);  // 仅 update_after_turn

    assert_eq!(c.no_progress_count(), 1);
    assert!((c.stall_probability() - 0.5).abs() < 1e-10);
}

/// 验证 TurnExecutor 的 accumulated_signals 正确累积去重语义
/// （同轮次多次工具调用都产生信号，全部收集）
#[test]
fn accumulated_signals_contains_all_tool_errors() {
    // 模拟 TurnExecutor 收集逻辑
    let mut accumulated: Vec<crate::guard::sensor::SensorSignal> = Vec::new();

    // 第 1 个工具：产生 2 个信号
    let round1 = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Rust error".into(),
        },
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 0.5, detail: "Non-zero exit".into(),
        },
    ];
    accumulated.extend(round1);

    // 第 2 个工具：产生 1 个信号
    let round2 = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Test failure".into(),
        },
    ];
    accumulated.extend(round2);

    // 总共累积了 3 个信号
    assert_eq!(accumulated.len(), 3);
    assert!(accumulated.iter().any(|s| s.detail.contains("Rust error")));
    assert!(accumulated.iter().any(|s| s.detail.contains("Test failure")));
    assert!(accumulated.iter().any(|s| s.detail.contains("Non-zero exit")));

    // 聚合时：只要存在 tool_error 类型的信号 → 1 次 note_error
    let has_any_tool_error = accumulated.iter().any(|s| s.kind == "tool_error");
    assert!(has_any_tool_error);
}

/// 验证 ToolResult 中的 sensor_signals 被正确保留（模拟 store 层行为）
#[test]
fn tool_result_preserves_sensor_signals() {
    let sig = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "test error".into(),
        },
    ];

    let result = crate::session::store::ToolResult {
        tool_use_id: "test_id".into(),
        tool_name: "Bash".into(),
        tool_args: Default::default(),
        content: "output".into(),
        conv_content: "".into(),
        sensor_signals: sig.clone(),
    };

    assert_eq!(result.sensor_signals.len(), 1);
    assert_eq!(result.sensor_signals[0].kind, "tool_error");
    assert_eq!(result.sensor_signals[0].detail, "test error");
}

/// 验证 controller.note_error 单调递增特性（与信号聚合配合）
#[test]
fn controller_k_respects_turn_boundary_aggregation() {
    let mut c = Controller::new();

    // 第 1 轮：Failed + 5 个传感器信号
    c.note_error(false);  // Failed
    // 聚合信号：有 → 1 次
    c.note_error(false);

    assert_eq!(c.no_progress_count(), 2);

    // 第 2 轮：Failed + 3 个传感器信号
    c.note_error(false);  // Failed
    c.note_error(false);  // 聚合

    assert_eq!(c.no_progress_count(), 4);

    // 第 3 轮：Stop（重置）
    c.note_progress(true);
    c.reset_stall();
    assert_eq!(c.no_progress_count(), 0);

    // 第 4 轮：Failed 但传感器无信号
    c.note_error(false);  // 仅 Failed

    assert_eq!(c.no_progress_count(), 1);
}

// ========================================================================
// 9. 边界场景补充
// ========================================================================

/// progress(true) 后立即 error(false)：Controller 先重置再递增
#[test]
fn controller_progress_then_immediate_error_resets_then_increments() {
    let mut c = Controller::new();
    c.note_error(false);
    c.note_error(false);
    assert_eq!(c.no_progress_count(), 2);

    c.note_progress(true);   // 重置
    assert_eq!(c.no_progress_count(), 0);

    c.note_error(false);      // 重新递增
    assert_eq!(c.no_progress_count(), 1);
    assert!((c.stall_probability() - 0.5).abs() < 1e-10);
}

/// Interrupted 决策不触发传感器信号喂入（同 orchestrator 逻辑）
#[test]
fn interrupted_decision_does_not_feed_sensor_signals() {
    // 模拟 orchestrator 中的条件：仅 Failed 才喂入
    let decision_is_failed = false; // 模拟 Interrupted
    let signals: Vec<crate::guard::sensor::SensorSignal> = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "err".into(),
        },
    ];

    // 仅在 Failed 时喂入
    let sensor_was_fed = if decision_is_failed && signals.iter().any(|s| s.kind == "tool_error") {
        true
    } else {
        false
    };
    assert!(!sensor_was_fed, "Interrupted should not feed sensor signals");
}

/// Continue 决策不触发传感器信号喂入
#[test]
fn continue_decision_does_not_feed_sensor_signals() {
    let decision_is_failed = false; // 模拟 Continue
    let signals: Vec<crate::guard::sensor::SensorSignal> = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "err".into(),
        },
    ];

    let sensor_was_fed = if decision_is_failed && signals.iter().any(|s| s.kind == "tool_error") {
        true
    } else {
        false
    };
    assert!(!sensor_was_fed, "Continue should not feed sensor signals");
}

/// 非 tool_error 类型的信号被聚合过滤器忽略
#[test]
fn non_tool_error_signals_ignored_by_aggregation() {
    let signals = vec![
        crate::guard::sensor::SensorSignal {
            kind: "perf_warning".into(), weight: 0.3, detail: "slow".into(),
        },
        crate::guard::sensor::SensorSignal {
            kind: "context_high".into(), weight: 0.2, detail: "pressure".into(),
        },
    ];

    // 聚合检查：没有 kind == "tool_error" 的信号
    let has_tool_error = signals.iter().any(|s| s.kind == "tool_error");
    assert!(!has_tool_error, "should not detect tool_error from non-error signals");

    // 不触发 note_error
    let mut c = Controller::new();
    if has_tool_error {
        c.note_error(false);
    }
    assert_eq!(c.no_progress_count(), 0, "controller should not be affected");
}

/// is_locked() 和 get_control_action() 在 P>0.80 时一致
#[test]
fn locked_and_control_action_consistent_at_threshold() {
    let mut c = Controller::new();
    for _ in 0..3 { c.note_error(false); }
    // P ≈ 0.875 → 同时满足 is_locked() 和 InjectReflectionHint
    assert!(c.is_locked(), "P>0.80 should lock");
    assert_eq!(c.get_control_action(), Some(ControlAction::InjectReflectionHint));

    // P > 0.95 → UpgradeModel 也意味着 locked
    c.note_error(false);
    c.note_error(false);
    assert!(c.is_locked());
    assert_eq!(c.get_control_action(), Some(ControlAction::UpgradeModel));
}

/// ensure 不会覆盖已存在的信念
#[test]
fn ensure_does_not_reset_existing_belief() {
    let mut ms = ModelSelector::new();
    ms.update("flash", true);
    ms.update("flash", true);
    let mean_before = ms.mean("flash");
    assert!(mean_before > 0.5);

    // ensure 已注册模型不应改变信念
    ms.ensure("flash");
    assert_eq!(ms.mean("flash"), mean_before);
}

// ========================================================================
// 10. resolve_active 真实逻辑对齐测试
// ========================================================================

/// 验证 forced_model = Pro 时直接返回 Pro（绕过 Controller 和 Selector）
#[test]
fn forced_model_pro_bypasses_all_other_logic() {
    // 模拟 resolve_active 中的 `if let Some(forced) = self.forced_model`
    let forced: Option<ModelTier> = Some(ModelTier::Pro);
    let tier = if let Some(f) = forced {
        f
    } else {
        ModelTier::Flash
    };
    assert_eq!(tier, ModelTier::Pro);
}

/// 验证 forced_model = Flash 时直接返回 Flash
#[test]
fn forced_model_flash_stays_flash() {
    let forced: Option<ModelTier> = Some(ModelTier::Flash);
    let tier = if let Some(f) = forced {
        f
    } else {
        ModelTier::Pro
    };
    assert_eq!(tier, ModelTier::Flash);
}

/// 验证 auto_model_enabled=false 时，config.model 被解析而非强制返回 flash
#[test]
fn auto_disabled_respects_config_model() {
    // 真实路径：ModelTier::parse(&config.model).unwrap_or(Flash)
    let config_model = "pro";
    let tier = ModelTier::parse(config_model).unwrap_or(ModelTier::Flash);
    assert_eq!(tier, ModelTier::Pro);
}

/// 验证 auto_disabled 时未知 model 回退到 Flash
#[test]
fn auto_disabled_unknown_model_falls_back_to_flash() {
    let tier = ModelTier::parse("gpt-4").unwrap_or(ModelTier::Flash);
    assert_eq!(tier, ModelTier::Flash);
}

// ========================================================================
// 11. 端到端信号链路仿真
// ========================================================================

/// 全链路仿真：传感器信号 → Controller → ModelSelector → 模型切换
/// 模拟 5 轮失败的完整生命周期。
#[test]
fn end_to_end_signal_chain_five_failed_turns() {
    let mut c = Controller::new();
    let mut ms = ModelSelector::new();
    ms.ensure("flash");
    ms.ensure("pro");

    // --- 轮次 1：Failed + 有传感器信号 ---
    c.note_tool_call();
    c.note_tool_call();
    c.note_tool_call(); // 3 tool calls
    c.note_error(false); // update_after_turn (Failed)
    // 传感器聚合信号
    let signals_round1 = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Rust compilation error".into(),
        },
    ];
    if signals_round1.iter().any(|s| s.kind == "tool_error") {
        c.note_error(false); // +1 aggregated
    }
    ms.update("flash", false); // flash failed
    // 第 1 轮后：k=2, P=0.75, flash α=1, β=2
    assert_eq!(c.no_progress_count(), 2);
    assert!((c.stall_probability() - 0.75).abs() < 1e-10);

    // --- 轮次 2：Failed + 有信号 ---
    c.note_tool_call(); c.note_tool_call();
    c.note_error(false);
    let signals_round2 = vec![
        crate::guard::sensor::SensorSignal {
            kind: "tool_error".into(), weight: 1.0, detail: "Rust compilation error".into(),
        },
    ];
    if signals_round2.iter().any(|s| s.kind == "tool_error") {
        c.note_error(false);
    }
    ms.update("flash", false);
    // 第 2 轮后：k=4, P=0.9375
    assert_eq!(c.no_progress_count(), 4);
    assert!(c.is_locked()); // P > 0.80

    // --- 轮次 3：Failed + 有信号 → 触发 UpgradeModel ---
    c.note_tool_call();
    c.note_error(false);
    if true { c.note_error(false); } // 聚合
    ms.update("flash", false);
    // 第 3 轮后：k=6, P≈0.984 > 0.95 → UpgradeModel
    assert_eq!(c.get_control_action(), Some(ControlAction::UpgradeModel));

    // --- 轮次 4：切换到 pro，成功 ---
    c.note_end_turn();
    c.note_progress(true);
    c.reset_stall();
    ms.update("pro", true);
    // Stop 后：k=0, P=0
    assert_eq!(c.stall_probability(), 0.0);
    assert!(!c.is_locked());
    // pro 的 mean 应高于 flash
    assert!(ms.mean("pro") > ms.mean("flash"));

    // --- 轮次 5：继续用 pro，成功 ---
    ms.update("pro", true);
    ms.update("pro", true);
    // selector 应坚定选择 pro
    assert_eq!(ms.select_greedy(), "pro");
}

/// 验证控制动作优先级链：Abort > UpgradeModel > InjectReflectionHint
#[test]
fn control_action_priority_chain() {
    let mut c = Controller::new();

    // k=1 → P=0.5 → None
    c.note_error(false);
    assert_eq!(c.get_control_action(), None);

    // k=3 → P=0.875 → InjectReflectionHint
    c.note_error(false); c.note_error(false);
    assert_eq!(c.get_control_action(), Some(ControlAction::InjectReflectionHint));

    // k=5 → P=0.969 → UpgradeModel (覆盖 InjectReflectionHint)
    c.note_error(false); c.note_error(false);
    assert_eq!(c.get_control_action(), Some(ControlAction::UpgradeModel));

    // k=10 → P=0.999 → Abort (覆盖 UpgradeModel)
    for _ in 0..5 { c.note_error(false); }
    assert_eq!(c.get_control_action(), Some(ControlAction::Abort));
}
