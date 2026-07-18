.PHONY: check test build release fmt

check:
	cargo check --workspace

test:
	cargo test --workspace

build:
	cargo build --workspace

release:
	cargo build --release --workspace

fmt:
	cargo fmt --all
