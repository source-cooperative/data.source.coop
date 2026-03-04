.PHONY: check test run-server run-workers ci setup

check:
	cargo check

check-wasm:
	cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown

fmt:
	cargo fmt -- --check
fmt-fix:
	cargo fmt

clippy:
	cargo clippy -- -D warnings
clippy-fix:
	cargo clippy --fix --allow-dirty --allow-staged

test:
	cargo test

run-server:
	cargo run -p source-coop-server -- $(ARGS)

run-workers:
	npx wrangler dev --cwd crates/runtimes/cf-workers

ci-fast: fmt clippy check-wasm
ci: ci-fast test

setup:
	git config core.hooksPath .githooks
