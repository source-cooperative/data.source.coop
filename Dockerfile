FROM rust:1.82-slim AS builder

WORKDIR /app
COPY . .

RUN cargo build --release --package source-coop-server --bin source-coop-proxy

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/source-coop-proxy /usr/local/bin/source-coop-proxy

EXPOSE 8080

ENTRYPOINT ["source-coop-proxy"]
CMD ["--config", "/etc/source-coop-proxy/config.toml", "--listen", "0.0.0.0:8080"]
