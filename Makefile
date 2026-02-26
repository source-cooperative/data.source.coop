.PHONY: check test run\:server run\:workers

check:
	cargo check
	cargo check -p s3-proxy-cf-workers --target wasm32-unknown-unknown

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
	cargo run -p s3-proxy-server -- $(ARGS)

run\:workers:
	npx wrangler dev --cwd crates/runtimes/cf-workers

build\:cli:
	cargo build -p source-coop-cli

build\:cli\:staging:
	cargo build -p source-coop-cli --no-default-features --features staging
