use super::*;

#[test]
fn anchor_locator_cut_deletes_between_anchors() {
    // 'start'..'end' 锚点：范围由行文本定位（含两端），模型不给行号。
    let patch = parse("[a.rs#1A2B]\nCUT 'alpha'..'omega':").unwrap();
    let result = apply(
        "alpha\nbeta\ngamma\nomega\nzeta\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap();
    assert_eq!(result.text, "zeta\n");
}

#[test]
fn anchor_locator_put_replaces_between_anchors() {
    let patch = parse("[a.rs#1A2B]\nPUT 'fn foo('..'}':\n+new body").unwrap();
    let text = "fn foo(\n    old();\n}\nfn bar() {}\n";
    let result = apply(text, &patch.sections[0], &mut Clipboard::default()).unwrap();
    assert_eq!(result.text, "new body\nfn bar() {}\n");
}

#[test]
fn anchor_locator_single_line_and_trim_matching() {
    // start == end 锚点行 → 单行操作；行首空白被 trim 后匹配。
    let patch = parse("[a.rs#1A2B]\nPUT 'beta'..'beta':\n+x").unwrap();
    let result = apply(
        "alpha\n  beta\ngamma\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap();
    assert_eq!(result.text, "alpha\nx\ngamma\n");
}

#[test]
fn anchor_locator_double_quotes_and_register() {
    let patch = parse("[a.rs#1A2B]\nCUT \"alpha\"..\"omega\" @saved\nPUT >$ @saved").unwrap();
    let result = apply(
        "alpha\nbeta\nomega\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap();
    assert_eq!(result.text, "alpha\nbeta\nomega\n");
}

#[test]
fn anchor_locator_missing_or_ambiguous_anchor_fails_visibly() {
    // 0 匹配：可诊断错误（不是静默 ±1）。
    let patch = parse("[a.rs#1A2B]\nCUT 'nope'..'omega':").unwrap();
    let err = apply(
        "alpha\nomega\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("anchor line not found"), "{err}");
    // 多匹配（`}` 不唯一）：要求更长锚点。
    let patch = parse("[a.rs#1A2B]\nCUT '}'..'omega':").unwrap();
    let err = apply(
        "fn a() {\n    a_body();\n}\nfn b() {\n    b_body();\n}\nomega\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("not unique"), "{err}");
}

#[test]
fn anchor_locator_reversed_range_fails() {
    let patch = parse("[a.rs#1A2B]\nCUT 'omega'..'alpha':").unwrap();
    let err = apply(
        "alpha\nomega\n",
        &patch.sections[0],
        &mut Clipboard::default(),
    )
    .unwrap_err();
    assert!(err.to_string().contains("after end"), "{err}");
}

#[test]
fn anchor_text_with_star_is_not_block_locator() {
    // 锚点文本里的 `*`（如乘法表达式）是普通内容，不是 Block locator。
    let patch = parse("[a.rs#1A2B]\nPUT 'exprs = [\"3 + 4 * 2\"]'..'next':\n+replaced").unwrap();
    let text = "exprs = [\"3 + 4 * 2\"]\nnext\ntail\n";
    let result = apply(text, &patch.sections[0], &mut Clipboard::default()).unwrap();
    assert_eq!(result.text, "replaced\ntail\n");
}

#[test]
fn line_number_block_locator_still_rejected_naturally() {
    // `5*`（Block locator）不是合法语法，被 invalid-range 自然拒绝。
    let err = parse("[a.rs#1A2B]\nPUT 5*:\n+x").unwrap_err();
    assert!(
        err.to_string().contains("invalid range"),
        "expected natural rejection, got: {err}"
    );
}

#[test]
fn anchor_locator_idempotent_already_applied() {
    let patch = parse("[a.rs#1A2B]\nPUT 'beta'..'beta':\n+beta").unwrap();
    let text = "alpha\nbeta\ngamma\n";
    assert!(already_applied(text, &patch.sections[0].operations));
}

#[test]
fn normalizes_missing_space_locators() {
    // 模型常写 `PUT18.=18:`（缺空格）——按 `PUT 18.=18:` 归一化并给 warning。
    let patch = parse("[a.rs#1A2B]\nPUT2.=2:\n+new").unwrap();
    assert_eq!(patch.sections[0].operations.len(), 1);
    assert_eq!(patch.sections[0].warnings.len(), 1);
    assert!(
        patch.sections[0].warnings[0].contains("Normalized missing space"),
        "{}",
        patch.sections[0].warnings[0]
    );
    let result = apply("a\nb\nc", &patch.sections[0], &mut Clipboard::default()).unwrap();
    assert_eq!(result.text, "a\nnew\nc");

    // gap 形态同样归一化：`PUT>40:` / `PUT<1:` / `CUT5.=8`。
    let patch = parse("[a.rs#1A2B]\nPUT>40:\n+tail").unwrap();
    assert_eq!(patch.sections[0].operations.len(), 1);
    let patch = parse("[a.rs#1A2B]\nCUT5.=8").unwrap();
    assert_eq!(patch.sections[0].operations.len(), 1);
    assert!(patch.sections[0].warnings[0].contains("Normalized missing space"));

    // 普通单词不会误归一化（PUT 后非定位符开头）。
    let err = parse("[a.rs#1A2B]\nPUTme 5.=5:\n+x").unwrap_err();
    assert!(err.to_string().contains("unknown hashline operation"));
}

#[test]
fn parses_current_put_protocol() {
    let patch = parse("[a.rs#1A2B]\nPUT 2.=2:\n+B\nPUT >$:\n+c").unwrap();
    let result = apply("a\nb\n", &patch.sections[0], &mut Clipboard::default()).unwrap();
    assert_eq!(result.text, "a\nB\nc\n");
}

#[test]
fn anonymous_and_named_registers_work() {
    let patch = parse("[a#AAAA]\nCUT 2.=2 @saved\nPUT <1 @saved").unwrap();
    let result = apply("a\nb\nc", &patch.sections[0], &mut Clipboard::default()).unwrap();
    assert_eq!(result.text, "b\na\nc");
}

#[test]
fn rejects_block_locators() {
    // Block locators are not supported and need no dedicated guard: the
    // `*` is not a valid range separator, so parsing rejects them as
    // invalid ranges (and quoted anchor text with `*` still works).
    assert!(
        parse("[a#AAAA]\nPUT 1*:\n+x")
            .unwrap_err()
            .to_string()
            .contains("invalid range")
    );
    assert!(
        parse("[a#AAAA]\nCUT 1*")
            .unwrap_err()
            .to_string()
            .contains("invalid range")
    );
    // `>1*` 是 gap locator，`*` 不是合法行号——按行号解析自然拒绝。
    assert!(
        parse("[a#AAAA]\nPUT >1*:\n+x")
            .unwrap_err()
            .to_string()
            .contains("positive line number")
    );
    assert!(
        parse("[a#AAAA]\nPUT >1* @saved")
            .unwrap_err()
            .to_string()
            .contains("positive line number")
    );
}
