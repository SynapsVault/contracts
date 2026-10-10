.PHONY: default all test build fmt clean fixture

default: build

all: test

test: build
	cargo test

build:
	stellar contract build
	@ls -l ../../target/wasm32v1-none/release/*.wasm

fmt:
	cargo fmt --all

clean:
	cargo clean

# Refresh the prebuilt WASM used as the upgrade target in unit tests.
fixture:
	stellar contract build --package vault-registry
	cp ../../target/wasm32v1-none/release/vault_registry.wasm fixtures/vault_registry_upgrade_target.wasm
