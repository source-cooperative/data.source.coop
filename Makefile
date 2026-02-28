.PHONY: check test run\:server run\:workers ci setup

check:
	cargo check
	cargo check -p source-coop-cf-workers --target wasm32-unknown-unknown

fmt:
	cargo fmt -- --check
fmt\:fix:
	cargo fmt

clippy:
	cargo clippy -- -D warnings
clippy\:fix:
	cargo clippy --fix --allow-dirty --allow-staged

test:
	cargo test

run\:server:
	cargo run -p source-coop-server -- $(ARGS)

run\:workers:
	npx wrangler dev --cwd crates/runtimes/cf-workers

build\:cli:
	cargo build -p source-coop-cli

build\:cli\:staging:
	cargo build -p source-coop-cli --no-default-features --features staging

ci: fmt clippy check test

setup:
	git config core.hooksPath .githooks
