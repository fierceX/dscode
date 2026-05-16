.PHONY: build check test clean

build:
	cargo build --release

check:
	cargo check

test: check
	cargo test

clean:
	cargo clean
