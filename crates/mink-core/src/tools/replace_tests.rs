use super::*;

#[test]
fn replace_with_identical_text_is_idempotent() {
    let result = replace_text("a\nb\n", "b", "b", false, false, 0.95, "x.md").unwrap();
    assert_eq!(result.strategy, "idempotent");
    assert_eq!(result.content, "a\nb\n");
    assert_eq!(result.count, 0);
}

#[test]
fn replace_all_with_identical_text_is_idempotent() {
    let result = replace_text("a\nb\nb\n", "b", "b", true, false, 0.95, "x.md").unwrap();
    assert_eq!(result.strategy, "idempotent");
    assert_eq!(result.content, "a\nb\nb\n");
}

#[test]
fn exact_ambiguity_is_rejected_and_all_replaces_every_match() {
    let error = replace_text("x x", "x", "y", false, true, 0.95, "a").unwrap_err();
    assert!(error.to_string().contains("2 occurrences"));
    let result = replace_text("x x", "x", "y", true, true, 0.95, "a").unwrap();
    assert_eq!(result.content, "y y");
}

#[test]
fn threshold_applies_to_every_fuzzy_candidate() {
    assert!(
        replace_text(
            "alpha beta gamma\n",
            "alpha beta\n",
            "changed\n",
            false,
            true,
            1.0,
            "a"
        )
        .is_err()
    );
}

#[test]
fn overlapping_all_candidates_do_not_panic() {
    assert!(replace_text("x\nx\nx", "x \nx ", "", true, true, 0.95, "a").is_err());
}

#[test]
fn indentation_only_rewrite_is_preserved() {
    let result = replace_text("    x\n", "x \n", "  x\n", false, true, 0.95, "a").unwrap();
    assert_eq!(result.content, "  x\n");
}

#[test]
fn identical_text_when_target_missing_still_fails_closed() {
    // 目标文本完全不存在时，`old == new` 不得伪装成成功更新：
    // 保持 fail-closed（match_error），与文档声明的保守行为一致。
    let error = replace_text("hello\n", "absent", "absent", false, false, 0.95, "x.md")
        .expect_err("missing target with old==new must fail closed");
    assert!(error.to_string().contains("Could not find"), "{error:#}");
}

#[test]
fn identical_text_fuzzy_candidate_is_idempotent_without_rewriting() {
    // old 与 actual 仅差一个字符：无 exact 命中，但有高相似 fuzzy 候选。
    // old == new 时幂等成功且不得把候选改写/规范化。
    let content = "alpha betta gamma\n";
    let result = replace_text(
        content,
        "alpha beta gamma",
        "alpha beta gamma",
        false,
        true,
        0.9,
        "x.md",
    )
    .unwrap();
    assert_eq!(result.strategy, "idempotent");
    assert_eq!(result.content, content);
}

#[test]
fn indentation_profile_converts_tabs_to_target_spaces() {
    let result = replace_text(
        "    foo\n        child\n",
        "\tfoo\n\t\tchild",
        "\tbar\n\t\tchild2",
        false,
        true,
        0.95,
        "a",
    )
    .unwrap();
    assert_eq!(result.content, "    bar\n        child2\n");
}
