.PHONY: check test run\:server run\:workers

check:
	cargo check
	cargo check -p s3-proxy-cf-workers --target wasm32-unknown-unknown

test:
	cargo test

run\:server:
	cargo run -p s3-proxy-server -- $(ARGS)

run\:workers:
	npx wrangler dev --cwd crates/runtimes/cf-workers
