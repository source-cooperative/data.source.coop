FROM rust:1.82-slim AS builder

WORKDIR /app
COPY . .

RUN cargo build --release --package s3-proxy-server --bin s3-proxy

FROM debian:bookworm-slim

RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/s3-proxy /usr/local/bin/s3-proxy

EXPOSE 8080

ENTRYPOINT ["s3-proxy"]
CMD ["--config", "/etc/s3-proxy/config.toml", "--listen", "0.0.0.0:8080"]
