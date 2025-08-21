# Build stage - target x86_64 for ECS Fargate compatibility
FROM --platform=linux/amd64 rust:1.82.0 AS builder

# Set environment variables for consistent builds
ENV CARGO_TARGET_DIR=/app/target
ENV RUSTFLAGS="-C target-cpu=x86-64"

# Copy source code
COPY . /app
WORKDIR /app

# Add x86_64 target and build
RUN rustup target add x86_64-unknown-linux-gnu
RUN cargo build --release --target x86_64-unknown-linux-gnu

# Runtime stage - minimal Debian image
FROM --platform=linux/amd64 debian:bookworm-slim AS runtime

# Install runtime dependencies (ca-certificates for HTTPS requests)
RUN apt-get update && \
    apt-get install -y --no-install-recommends \
        ca-certificates \
        curl \
        && rm -rf /var/lib/apt/lists/*

# Create app user for security
RUN groupadd -r appuser && useradd -r -g appuser appuser

# Copy the built binary from builder stage
COPY --from=builder /app/target/x86_64-unknown-linux-gnu/release/source-data-proxy /app/source-data-proxy

# Set proper permissions
RUN chown appuser:appuser /app/source-data-proxy && \
    chmod +x /app/source-data-proxy

# Switch to non-root user
USER appuser

# Set working directory and expose port
WORKDIR /app
EXPOSE 8080

# Health check endpoint
HEALTHCHECK --interval=30s --timeout=3s --start-period=5s --retries=3 \
    CMD curl -f http://localhost:8080/health || exit 1

# Run the binary directly
ENTRYPOINT ["/app/source-data-proxy"]
