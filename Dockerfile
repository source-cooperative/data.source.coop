FROM rust:1.80.1
ADD . /app
WORKDIR /app
RUN cargo build --release --locked
ENTRYPOINT ["cargo", "run", "--release"]
