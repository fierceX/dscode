pub mod agent_config;
pub mod config;
pub mod registry;
pub mod runtime;

/// Serializes tests that write process-global environment variables
/// (`MODEL` / `HOME` / `MINK_HOME` / `DEEPSEEK_*`). Rust test threads run in
/// parallel inside one process, so an unguarded write races other tests on the
/// same keys — env-mutating tests in this module must hold this lock for their
/// whole body. Async tests await with `lock().await`; sync tests use
/// `blocking_lock()`.
#[cfg(test)]
pub(crate) static TEST_ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());
