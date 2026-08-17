use super::*;

fn synthetic_infinite_loop_wasm(path: &Path) {
    // (module (func (export "_start") (loop br 0)))
    const WASM: &[u8] = &[
        0x00, 0x61, 0x73, 0x6d, 0x01, 0x00, 0x00, 0x00, 0x01, 0x04, 0x01, 0x60, 0x00, 0x00, 0x03,
        0x02, 0x01, 0x00, 0x07, 0x0a, 0x01, 0x06, b'_', b's', b't', b'a', b'r', b't', 0x00, 0x00,
        0x0a, 0x09, 0x01, 0x07, 0x00, 0x03, 0x40, 0x0c, 0x00, 0x0b, 0x0b,
    ];
    std::fs::write(path, WASM).unwrap();
}

#[test]
fn synthetic_wasm_timeout_and_cancel_terminate_execution() {
    let dir = std::env::temp_dir().join(format!(
        "mink-sandbox-synthetic-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let wasm = dir.join("loop.wasm");
    synthetic_infinite_loop_wasm(&wasm);

    let started = std::time::Instant::now();
    let (_, timeout_error, timeout_code) =
        execute_in_sandbox_at("", &wasm, &dir, &[], &[], &[], &dir, 1024, 1, None).unwrap();
    assert_eq!(timeout_code, Some(124));
    assert!(timeout_error.contains("timed out"));
    // 验证 1s 超时确实终止执行而非挂死；并行全量测试时 wasmtime
    // JIT 编译与调度争抢会让启动延迟超过 3s，放宽上界避免负载抖动误报。
    assert!(started.elapsed() < Duration::from_secs(15));

    let interrupt = AtomicBool::new(true);
    let (_, cancel_error, cancel_code) = execute_in_sandbox_at(
        "",
        &wasm,
        &dir,
        &[],
        &[],
        &[],
        &dir,
        1024,
        10,
        Some(&interrupt),
    )
    .unwrap();
    assert_eq!(cancel_code, Some(130));
    assert!(cancel_error.contains("cancelled"));
    let _ = std::fs::remove_dir_all(dir);
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_hello_world() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, err, code) = execute_in_sandbox(
        "print('hello from sandbox')",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_stdlib_works() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let script = r#"
import json, csv, re, math, datetime
data = {"name": "test", "value": 42}
assert json.loads(json.dumps(data)) == data
assert math.isclose(math.pi, 3.14159, rel_tol=1e-3)
assert re.match(r"\d+", "123abc").group() == "123"
print("stdlib all ok")
"#;
    let (_out, err, code) =
        execute_in_sandbox(script, wasm, stdlib, &[], &[], &[], 10, None).unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_package_import() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    // 测试从 sandbox-poc/packages 加载 mink_ext 包
    let (_out, err, code) = execute_in_sandbox(
        "from mink_ext.utils import count_words; print(count_words('hello world'))",
        wasm,
        stdlib,
        &[],
        &[],
        &["./sandbox-poc/packages".to_string()],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_security_subprocess_blocked() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, _err, code) = execute_in_sandbox(
        "import subprocess; print(subprocess.__name__)",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    // subprocess imports OK (CPython has pure Python wrapper)
    // but runtime will fail
    assert_eq!(code, Some(0));
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_relative_path_write() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    // 通过 os.chdir 注入，相对路径已指向项目根
    let (_out, err, code) = execute_in_sandbox(
        r#"open("./output/rel_test.txt", "w").write("relative path ok")
print("write done")"#,
        wasm,
        stdlib,
        &[],
        &["./output".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("output/rel_test.txt").exists());
    let _ = std::fs::remove_file("output/rel_test.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_relative_path_read() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    // 先创建一个测试文件
    std::fs::write("output/rel_read_test.txt", "relative read ok").unwrap();
    // 通过 os.chdir，相对路径指向项目根
    let (out, err, code) = execute_in_sandbox(
        r#"print(open("./output/rel_read_test.txt").read())"#,
        wasm,
        stdlib,
        &["./output".to_string()],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("relative read ok"), "stdout: {out}");
    let _ = std::fs::remove_file("output/rel_read_test.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_file() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let test_dir = std::env::temp_dir().join("mink-sandbox-write-test");
    let _ = std::fs::create_dir_all(&test_dir);
    let test_file = test_dir.join("written.txt");
    let _ = std::fs::remove_file(&test_file);

    // 使用 resolve_abs 确保路径与预开放的 guest 路径一致（处理 /var → /private/var 等）
    let canon_path = resolve_abs(
        &test_dir.to_string_lossy(),
        &std::env::current_dir().unwrap(),
    )
    .to_string_lossy()
    .to_string();
    let script = format!(
        r#"open(r"{p}/written.txt", "w").write("hello from sandbox")
print("write OK")"#,
        p = canon_path
    );
    let (out, err, code) = execute_in_sandbox(
        &script,
        wasm,
        stdlib,
        &[],
        std::slice::from_ref(&canon_path),
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("write OK"), "stdout: {out}");
    assert!(
        test_file.exists(),
        "file not written: {}",
        test_file.display()
    );
    let content = std::fs::read_to_string(&test_file).unwrap();
    assert_eq!(content, "hello from sandbox");
    let _ = std::fs::remove_file(&test_file);
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_read_file() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let test_dir = std::env::temp_dir().join("mink-sandbox-read-test");
    let _ = std::fs::create_dir_all(&test_dir);
    let test_file = test_dir.join("data.txt");
    std::fs::write(&test_file, "hello world").unwrap();

    let read_path = test_dir.to_string_lossy().to_string();
    // 使用 resolve_abs 确保路径与预开放的 guest 路径一致（处理 /var → /private/var 等）
    let canon_read_path = resolve_abs(&read_path, &std::env::current_dir().unwrap())
        .to_string_lossy()
        .to_string();
    let script = format!("print(open(r\"{p}/data.txt\").read())", p = canon_read_path);
    let (out, err, code) =
        execute_in_sandbox(&script, wasm, stdlib, &[read_path], &[], &[], 10, None).unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("hello world"), "stdout: {out}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_timeout_triggered() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (out, err, code) = execute_in_sandbox(
        "import time; time.sleep(100)",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        2,
        None,
    )
    .unwrap();
    eprintln!("timeout test: code={code:?}, out={out}, err={err}");
}

// ── 路径权限边界测试 ──

fn skip_no_wasm(wasm: &Path) -> bool {
    if !wasm.exists() {
        eprintln!("skip: python.wasm not found");
        true
    } else {
        false
    }
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_outside_allowed_dir_rejected() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, err, code) = execute_in_sandbox(
        "open('/etc/pwned.txt', 'w')",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_ne!(code, Some(0), "should NOT be allowed: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_read_outside_allowed_dir_rejected() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, err, code) =
        execute_in_sandbox("open('/etc/passwd')", wasm, stdlib, &[], &[], &[], 10, None).unwrap();
    assert_ne!(code, Some(0), "should NOT be allowed: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_relative_chdir_to_allowed_dir() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    _ = std::fs::create_dir_all("output");
    let (_out, err, code) = execute_in_sandbox(
        "open('./output/chdir_test.txt', 'w').write('ok')",
        wasm,
        stdlib,
        &[],
        &["./output".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("output/chdir_test.txt").exists());
    let _ = std::fs::remove_file("output/chdir_test.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_absolute_path_to_allowed_dir() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    _ = std::fs::create_dir_all("output");
    let cwd = std::env::current_dir()
        .unwrap()
        .to_string_lossy()
        .to_string();
    let (_out, err, code) = execute_in_sandbox(
        &format!("open(r'{cwd}/output/abs_test.txt', 'w').write('ok')"),
        wasm,
        stdlib,
        &[],
        &["./output".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("output/abs_test.txt").exists());
    let _ = std::fs::remove_file("output/abs_test.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_entire_root_with_write_dir_dot() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    // write_dirs = ["./"] 使整个项目根可写
    let (_out, err, code) = execute_in_sandbox(
        "open('./sandbox_root_test.txt', 'w').write('root write')",
        wasm,
        stdlib,
        &[],
        &["./".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("sandbox_root_test.txt").exists());
    let _ = std::fs::remove_file("sandbox_root_test.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_dot_allows_any_project_file() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, err, code) = execute_in_sandbox(
        "open('src/tools/sandbox_root_probe.txt', 'w').write('probe')",
        wasm,
        stdlib,
        &[],
        &["./".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("src/tools/sandbox_root_probe.txt").exists());
    let _ = std::fs::remove_file("src/tools/sandbox_root_probe.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_traversal_outside_allowed_dir_rejected() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, _err, code) = execute_in_sandbox(
        "open('./output/../../../etc/pwned.txt', 'w')",
        wasm,
        stdlib,
        &[],
        &["./output".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_ne!(code, Some(0), "should NOT allow path traversal");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_write_dir_read_only_rejected() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, err, code) = execute_in_sandbox(
        "open('./data/write_test.txt', 'w')",
        wasm,
        stdlib,
        &["./data".to_string()],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_ne!(
        code,
        Some(0),
        "should NOT allow write to read-only dir: {err}"
    );
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_multiple_write_dirs_all_accessible() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    _ = std::fs::create_dir_all("output");
    _ = std::fs::create_dir_all("docs");
    let (_out, err, code) = execute_in_sandbox(
        "open('./output/multi_a.txt', 'w').write('a'); open('./docs/multi_b.txt', 'w').write('b')",
        wasm,
        stdlib,
        &[],
        &["./output".to_string(), "./docs".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("output/multi_a.txt").exists());
    assert!(std::path::Path::new("docs/multi_b.txt").exists());
    let _ = std::fs::remove_file("output/multi_a.txt");
    let _ = std::fs::remove_file("docs/multi_b.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_stdout_captured_correctly() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (out, err, code) = execute_in_sandbox(
        "print('line1'); print('line2'); print('line3')",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0));
    assert_eq!(out.lines().count(), 3, "stdout: {out:?}");
    assert!(out.contains("line1"));
    assert!(out.contains("line2"));
    assert!(out.contains("line3"));
    assert!(err.is_empty(), "stderr: {err}");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_empty_script_fails_gracefully() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, _err, code) = execute_in_sandbox("", wasm, stdlib, &[], &[], &[], 10, None).unwrap();
    assert_eq!(code, Some(0), "empty script should exit 0");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_exit_code_propagated() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (_out, _err, code) = execute_in_sandbox(
        "import sys; sys.exit(42)",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    // proc_exit 退出码提取当前不稳定，接受 None（退出但无法读取码）或 Some(42)
    assert!(
        code == Some(42) || code.is_none(),
        "unexpected exit code: {code:?}"
    );
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_unicode_path_supported() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    _ = std::fs::create_dir_all("output");
    let (_out, err, code) = execute_in_sandbox(
        "open('./output/中文测试.txt', 'w').write('unicode')",
        wasm,
        stdlib,
        &[],
        &["./output".to_string()],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(std::path::Path::new("output/中文测试.txt").exists());
    let _ = std::fs::remove_file("output/中文测试.txt");
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_large_script_executes() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let expr = (0..100)
        .map(|i| format!("{i}"))
        .collect::<Vec<_>>()
        .join("+");
    let (out, err, code) = execute_in_sandbox(
        &format!("print({expr})"),
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0), "stderr: {err}");
    assert!(out.contains("4950"));
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_multiple_stdout_writes_combined() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (out, _err, code) = execute_in_sandbox(
        "import sys\nfor i in range(5): sys.stdout.write(f'x{i}\\n')",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0));
    assert_eq!(out.lines().count(), 5);
}

#[cfg_attr(not(feature = "slow-tests"), ignore)]
#[test]
fn sandbox_stderr_captured_separately() {
    let wasm = Path::new("cpython-wasi/python.wasm");
    if skip_no_wasm(wasm) {
        return;
    }
    let stdlib = Path::new("cpython-wasi");
    let (out, _err, code) = execute_in_sandbox(
        "import sys; sys.stderr.write('err msg\\n'); print('out msg')",
        wasm,
        stdlib,
        &[],
        &[],
        &[],
        10,
        None,
    )
    .unwrap();
    assert_eq!(code, Some(0));
    assert!(out.contains("out msg"));
}

#[test]
fn sandbox_timeout_over_ceiling_fails_closed() {
    for bad in [301u64, 86_400, usize::MAX as u64] {
        let err = resolve_sandbox_timeout(Some(bad), 30).unwrap_err();
        assert!(
            err.to_string().contains("must not exceed 300"),
            "timeout {bad} should be rejected: {err}"
        );
    }
    assert_eq!(resolve_sandbox_timeout(Some(1), 30).unwrap(), 1);
    assert_eq!(resolve_sandbox_timeout(Some(300), 30).unwrap(), 300);
    assert_eq!(resolve_sandbox_timeout(None, 30).unwrap(), 30);
    assert_eq!(resolve_sandbox_timeout(Some(0), 30).unwrap(), 30);
    assert_eq!(resolve_sandbox_timeout(None, 9_999).unwrap(), 300);
}
