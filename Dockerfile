# Final stage
FROM debian:bullseye-slim
COPY target/release/source-data-proxy /usr/local/bin/
CMD ["source-data-proxy"]