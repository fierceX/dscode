use std::process::Command;
#[cfg(any(feature = "sdk-bin", feature = "sdk"))]
use std::process::Stdio;

#[test]
fn mink_help_uses_mink_binary_name() {
    let output = Command::new(env!("CARGO_BIN_EXE_mink"))
        .arg("--help")
        .output()
        .expect("run mink --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: mink [options] [prompt]"));
    assert!(stdout.contains("--agent-jsonl"));
    #[cfg(feature = "tui")]
    assert!(stdout.contains("--tui"));
}

#[test]
fn mink_version_reports_cargo_package_version() {
    let output = Command::new(env!("CARGO_BIN_EXE_mink"))
        .arg("--version")
        .output()
        .expect("run mink --version");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(env!("CARGO_PKG_VERSION")),
        "mink --version must report the crate version, got: {stdout}"
    );
}

#[test]
fn mink_short_version_flag_matches_long_flag() {
    let long = Command::new(env!("CARGO_BIN_EXE_mink"))
        .arg("--version")
        .output()
        .expect("run mink --version");
    let short = Command::new(env!("CARGO_BIN_EXE_mink"))
        .arg("-V")
        .output()
        .expect("run mink -V");
    assert!(short.status.success());
    assert_eq!(
        String::from_utf8_lossy(&long.stdout),
        String::from_utf8_lossy(&short.stdout)
    );
}

#[cfg(any(feature = "sdk-bin", feature = "sdk"))]
#[test]
fn mink_core_help_uses_mink_core_binary_name() {
    let output = Command::new(env!("CARGO_BIN_EXE_mink-core"))
        .arg("--help")
        .output()
        .expect("run mink-core --help");

    assert!(output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("Usage: mink-core [options] [prompt]"));
    assert!(stdout.contains("--agent-jsonl"));
}

#[cfg(any(feature = "sdk-bin", feature = "sdk"))]
#[test]
fn mink_core_agent_jsonl_parse_failure_keeps_final_schema() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mink-core"))
        .arg("--agent-jsonl")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DEEPSEEK_BASE_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mink-core --agent-jsonl");

    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(br#"{"version":2}"#)
            .expect("write sdk request");
    }

    let output = child.wait_with_output().expect("wait mink-core");
    assert!(!output.status.success());
    let stdout = String::from_utf8_lossy(&output.stdout);
    let final_line: serde_json::Value =
        serde_json::from_str(stdout.trim()).expect("parse final json");

    assert_eq!(final_line["type"], "final");
    assert_eq!(final_line["version"], 2);
    assert_eq!(final_line["status"], "failed");
    assert_eq!(final_line["session_id"], "");
    assert_eq!(final_line["session_ref"], "");
    assert_eq!(final_line["home"], "");
    assert_eq!(final_line["cwd"], "");
    assert_eq!(final_line["events_path"], "");
    assert_eq!(final_line["conversation_path"], "");
    assert_eq!(final_line["artifacts_dir"], "");
    assert_eq!(final_line["summary_path"], "");
    assert_eq!(final_line["tool_call_count"], 0);
    assert_eq!(final_line["tool_error_count"], 0);
    assert!(
        final_line["error"]
            .as_str()
            .unwrap()
            .contains("missing required field prompt")
    );
}

#[cfg(any(feature = "sdk-bin", feature = "sdk"))]
#[test]
fn mink_core_agent_jsonl_valid_request_without_api_key_fails_before_runtime() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_mink-core"))
        .arg("--agent-jsonl")
        .env_remove("DEEPSEEK_API_KEY")
        .env_remove("DEEPSEEK_BASE_URL")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mink-core --agent-jsonl");

    {
        use std::io::Write;
        child
            .stdin
            .as_mut()
            .expect("stdin")
            .write_all(br#"{"version":2,"prompt":"hello"}"#)
            .expect("write sdk request");
    }

    let output = child.wait_with_output().expect("wait mink-core");
    assert!(!output.status.success());
    assert!(
        String::from_utf8_lossy(&output.stdout).trim().is_empty(),
        "valid config failure should not emit parse final JSON"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("no API key"),
        "stderr should contain the provider defaults error"
    );
}
