use super::*;

#[test]
fn empty_script_errors() {
    assert!(execute_script("", None).is_err());
    assert!(execute_script("   ", None).is_err());
}

#[test]
fn simple_print_works() {
    let (stdout, stderr, code) = execute_script("print('hello')", None).unwrap();
    assert!(stdout.contains("hello"));
    assert_eq!(stderr, "");
    assert_eq!(code, Some(0));
}

#[test]
fn unrestricted_scripts_execute() {
    // 限制已移除：这些脚本现在应该正常执行
    let (out, _, code) = execute_script("import subprocess; print('ok')", None).unwrap();
    assert_eq!(code, Some(0));
    assert!(out.contains("ok"));
}

#[test]
fn timeout_kills_long_script() {
    let (stdout, _, code) = execute_script("import time; time.sleep(10)", Some(1)).unwrap();
    assert!(stdout.contains("timed out"));
    assert_eq!(code, Some(124));
}

#[test]
fn signal_killed_script_fails() {
    let (_, _, code) = execute_script(
        "import os, signal; os.kill(os.getpid(), signal.SIGKILL)",
        None,
    )
    .unwrap();
    assert_eq!(code, None);
}

#[test]
fn large_stdout_is_drained_and_bounded() {
    let script = "import sys\nsys.stdout.write('x' * 1200000)\n";
    let (stdout, stderr, code) = execute_script(script, Some(10)).unwrap();
    assert_eq!(code, Some(0));
    assert!(stderr.is_empty());
    assert!(stdout.contains("truncated stdout"));
}

#[test]
fn interrupt_kills_long_script() {
    let interrupt = AtomicBool::new(true);
    let (stdout, _, code) = execute_script_with_interrupt(
        "import time; time.sleep(10); print('done')",
        Some(30),
        Some(&interrupt),
    )
    .unwrap();
    assert_eq!(code, Some(130));
    assert!(stdout.contains("interrupted"));
    assert!(!stdout.contains("done"));
}

#[test]
fn json_processing_works() {
    let script = r#"
import json
data = {"concrete": "C45", "volume": 1500}
print(json.dumps(data, indent=2))
"#;
    let (stdout, _, code) = execute_script(script, None).unwrap();
    assert!(stdout.contains("C45"));
    assert!(stdout.contains("1500"));
    assert_eq!(code, Some(0));
}
