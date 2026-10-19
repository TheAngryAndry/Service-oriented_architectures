FROM rust:1.95-slim-bookworm AS builder
WORKDIR /build
COPY tools/Cargo.toml tools/Cargo.lock ./
COPY tools/src ./src
COPY tools/tests ./tests
RUN cargo test --release --locked --test e2e --no-run \
    && find target/release/deps -maxdepth 1 -type f -executable -name 'e2e-*' -exec cp '{}' /e2e \;

FROM debian:bookworm-slim
COPY --from=builder /e2e /usr/local/bin/e2e
USER 10001:10001
CMD ["e2e", "--ignored", "--nocapture"]
