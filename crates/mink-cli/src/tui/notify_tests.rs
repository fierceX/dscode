#[test]
fn terminal_osc_component_removes_control_and_delimiters() {
    assert_eq!(super::terminal_osc_component("a;b\nc\x1b\x07d"), "a b c  d");
}

#[cfg(target_os = "macos")]
#[test]
fn apple_script_string_escapes_quotes_backslashes_and_newlines() {
    assert_eq!(
        super::apple_script_string("a\"b\\c\nd"),
        "\"a\\\"b\\\\c\\nd\""
    );
}
