.PHONY: build check test clippy regression-mock regression-client regression-api regression-all coverage coverage-core coverage-with-ignored clean

CORE_COVERAGE_IGNORE := (main\.rs|tui/|ui/|tools/(web|search|runner|bash|file)\.rs|llm/(client|transport)\.rs|sse/toolcall\.rs|session/compaction\.rs|config\.rs|prompt\.rs|assets\.rs|context\.rs|errors\.rs|events\.rs|session/(paths|init)\.rs|util\.rs|test_mock\.rs|regression\.rs|agent/(orchestrator|prefix|compactor|sub_coordinator|sub_executor)\.rs)

build:
	cargo build --release

check:
	cargo check

test: check
	cargo test

clippy:
	cargo clippy --all-targets --all-features -- -D warnings

regression-mock:
	cargo test regression:: -- --nocapture
	cargo test test_mock:: -- --nocapture

regression-client:
	cargo test llm::client::tests:: -- --ignored --nocapture
	cargo test session::compaction::tests::evaluate_and_compact_writes_clean_summary_and_keeps_valid_conversation -- --ignored --nocapture

regression-api:
	DSCODE_REAL_API=1 cargo test regression::real_deepseek_api_smoke_streams_response -- --ignored --nocapture

regression-all: regression-mock regression-client test clippy

coverage:
	@if command -v cargo-llvm-cov >/dev/null 2>&1; then \
		cargo llvm-cov --all-targets --all-features; \
	else \
		echo "cargo-llvm-cov is not installed. Install with: cargo install cargo-llvm-cov"; \
		exit 1; \
	fi

coverage-core:
	@if command -v cargo-llvm-cov >/dev/null 2>&1; then \
		cargo llvm-cov --all-targets --all-features --ignore-filename-regex '$(CORE_COVERAGE_IGNORE)' --fail-under-lines 90; \
	else \
		echo "cargo-llvm-cov is not installed. Install with: cargo install cargo-llvm-cov"; \
		exit 1; \
	fi

coverage-with-ignored:
	@if command -v cargo-llvm-cov >/dev/null 2>&1; then \
		cargo llvm-cov --all-targets --all-features -- --include-ignored; \
	else \
		echo "cargo-llvm-cov is not installed. Install with: cargo install cargo-llvm-cov"; \
		exit 1; \
	fi

clean:
	cargo clean
