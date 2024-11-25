# Base stage
FROM rust:1.80.1 as builder
WORKDIR /app
COPY . .
RUN rustup target add aarch64-unknown-linux-gnu
RUN cargo build --release --locked --target=aarch64-unknown-linux-gnu

# Final stage
FROM debian:bullseye-slim
COPY --from=builder /app/target/release/source-data-proxy /usr/local/bin/
CMD ["source-data-proxy"]