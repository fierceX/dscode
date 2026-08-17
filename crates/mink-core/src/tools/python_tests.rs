use super::*;
use std::time::Duration;

#[test]
fn empty_script_errors() {
    assert!(execute_script("", None).is_err());
    assert!(execute_script("   ", None).is_err());
}

#[test]
fn python_timeout_uses_configured_default_and_configured_ceiling() {
    let resolve = crate::tools::process::resolve_tool_timeout;
    assert_eq!(resolve(None, 45, 600).unwrap(), Duration::from_secs(45));
    assert_eq!(resolve(Some(0), 45, 600).unwrap(), Duration::from_secs(45));
    assert_eq!(resolve(Some(1), 45, 600).unwrap(), Duration::from_secs(1));
    assert_eq!(resolve(None, 9_999, 600).unwrap(), Duration::from_secs(600));
    for bad in [601u64, 86_400, u64::MAX] {
        let err = resolve(Some(bad), 30, 600).unwrap_err();
        assert!(
            err.to_string().contains("must not exceed 600"),
            "timeout {bad} should be rejected: {err}"
        );
    }

    // A raised configured ceiling admits larger explicit values and clamps
    // the configured default to the new ceiling.
    assert_eq!(
        resolve(Some(1_200), 30, 1_800).unwrap(),
        Duration::from_secs(1_200)
    );
    assert_eq!(
        resolve(None, 3_600, 1_800).unwrap(),
        Duration::from_secs(1_800)
    );
    let err = resolve(Some(1_801), 30, 1_800).unwrap_err();
    assert!(err.to_string().contains("must not exceed 1800"), "{err}");

    // A ceiling below the 5s default floor fails closed instead of panicking
    // inside Duration::clamp.
    let err = resolve(None, 30, 4).unwrap_err();
    assert!(err.to_string().contains("at least 5"), "{err}");
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
