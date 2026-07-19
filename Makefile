NUNTIUS_SERVER_URL ?= http://47.97.154.221:8765/
NUNTIUS_INSTALL_ROOT ?= $(HOME)/.local
NUNTIUS_CLIENT_BIN := $(NUNTIUS_INSTALL_ROOT)/bin/nuntius-client
NUNTIUS_SECURITY_FLAG := $(if $(filter http://%,$(NUNTIUS_SERVER_URL)),--allow-insecure-http,)

.PHONY: check test build release fmt device-setup

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

device-setup:
	cargo install --path client --locked --force --root "$(NUNTIUS_INSTALL_ROOT)"
	@"$(NUNTIUS_CLIENT_BIN)" setup --server-url "$(NUNTIUS_SERVER_URL)" $(NUNTIUS_SECURITY_FLAG)
