use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::io::Write;
use std::sync::Mutex;

/// A single signal detected by a sensor script.
#[derive(Debug, Clone)]
pub struct SensorSignal {
    pub kind: String,
    pub weight: f64,
    pub detail: String,
}

/// Output parsed from a sensor script's stdout.
#[derive(Debug, Clone, serde::Deserialize)]
struct SensorOutput {
    #[serde(default)]
    signals: Vec<SensorSignalRaw>,
    #[serde(default)]
    actions: Vec<String>,
}

#[derive(Debug, Clone, serde::Deserialize)]
struct SensorSignalRaw {
    kind: String,
    weight: f64,
    detail: String,
}

// Embedded sensor script — compiled into the binary.
const ERROR_SENSOR: &str = include_str!("../../assets/sensors/error.sh");

/// Lazy-initialized temporary directory holding extracted sensor scripts.
static SENSOR_INIT: Mutex<Option<PathBuf>> = Mutex::new(None);

/// Ensure sensor scripts are extracted to a temp directory.
/// Returns the directory path.
fn ensure_sensor_dir() -> std::io::Result<PathBuf> {
    let mut guard = SENSOR_INIT.lock().unwrap();
    if let Some(ref dir) = *guard {
        return Ok(dir.clone());
    }
    let tmp = std::env::temp_dir().join(format!("dscode-sensors-{}", std::process::id()));
    std::fs::create_dir_all(&tmp)?;
    // Write error.sh
    let script_path = tmp.join("error.sh");
    if !script_path.exists() {
        std::fs::write(&script_path, ERROR_SENSOR)?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }
    *guard = Some(tmp.clone());
    Ok(tmp)
}

/// Run a sensor script by name.
///
/// `name` — sensor name (e.g. "error"), mapped to `assets/sensors/<name>.sh`.
/// `tool_name` — the tool that was executed (argv[1]).
/// `elapsed_ms` — execution duration in ms (argv[2]).
/// `output_len` — output byte count (argv[3]).
/// `output` — full tool output (stdin).
///
/// Returns `Ok(signals)` on success, or `Err` if the sensor script failed
/// (non-zero exit, missing, or unparseable output).
pub fn run_sensor(
    name: &str,
    tool_name: &str,
    elapsed_ms: u64,
    output_len: usize,
    output: &str,
) -> anyhow::Result<Vec<SensorSignal>> {
    let dir = ensure_sensor_dir()?;
    let script_path = dir.join(format!("{name}.sh"));
    if !script_path.exists() {
        anyhow::bail!("sensor script not found: {name}.sh");
    }

    let mut child = Command::new(&script_path)
        .arg(tool_name)
        .arg(elapsed_ms.to_string())
        .arg(output_len.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;

    // Pipe tool output to script's stdin
    if let Some(ref mut stdin) = child.stdin {
        stdin.write_all(output.as_bytes())?;
    }
    drop(child.stdin.take());

    let output = child.wait_with_output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("sensor {name}.sh exited with {}: {stderr}", output.status);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    if stdout.trim().is_empty() || stdout.trim() == "{}" {
        return Ok(Vec::new());
    }

    let parsed: SensorOutput = serde_json::from_str(stdout.trim())
        .map_err(|e| anyhow::anyhow!("sensor {name}.sh: failed to parse output: {e}\noutput: {stdout}"))?;

    Ok(parsed.signals.into_iter().map(|s| SensorSignal {
        kind: s.kind,
        weight: s.weight,
        detail: s.detail,
    }).collect())
}

/// Find the path to a sensor script, if it exists.
pub fn find_sensor(name: &str) -> Option<PathBuf> {
    let dir = ensure_sensor_dir().ok()?;
    let path = dir.join(format!("{name}.sh"));
    if path.exists() { Some(path) } else { None }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn run_sensor_returns_empty_for_success() {
        let signals = run_sensor("error", "Read", 10, 50, "everything is fine")
            .expect("sensor should run");
        assert!(signals.is_empty(), "no signals for clean output");
    }

    #[test]
    fn run_sensor_returns_signals_for_rust_error() {
        let output = "error[E0425]: cannot find value `x` in this scope";
        let signals = run_sensor("error", "Bash", 100, output.len() as _, output)
            .expect("sensor should run");
        assert!(!signals.is_empty(), "should detect rust error");
        assert!(signals.iter().any(|s| s.kind == "tool_error"));
    }

    #[test]
    fn run_sensor_returns_none_when_script_not_found() {
        let result = run_sensor("nonexistent", "Read", 0, 0, "");
        assert!(result.is_err(), "nonexistent sensor should fail");
    }

    #[test]
    fn error_sensor_detects_rust_error() {
        let output = "error[E0425]: cannot find value `x` in this scope\n";
        let signals = run_sensor("error", "Bash", 100, output.len(), output)
            .expect("sensor should run");
        assert!(signals.iter().any(|s| s.detail.contains("Rust compilation error")));
    }

    #[test]
    fn error_sensor_detects_python_test_failure() {
        let output = "FAILED tests/test_main.py::test_foo - AssertionError: assert 1 == 2\n";
        let signals = run_sensor("error", "Bash", 100, output.len(), output)
            .expect("sensor should run");
        assert!(signals.iter().any(|s| s.detail.contains("Test failure")));
    }

    #[test]
    fn find_sensor_returns_path_for_existing() {
        let path = find_sensor("error");
        assert!(path.is_some(), "error sensor should exist");
        assert!(path.unwrap().ends_with("error.sh"));
    }

    #[test]
    fn find_sensor_returns_none_for_missing() {
        assert!(find_sensor("nonexistent").is_none());
    }
}
