# Base stage
FROM rust:1.80.1 as builder
WORKDIR /app
COPY . .
RUN cargo build --release

# Final stage
FROM debian:bullseye-slim
COPY --from=builder /app/target/release/source-data-proxy /usr/local/bin/
CMD ["source-data-proxy"]