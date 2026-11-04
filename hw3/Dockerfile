FROM rust:1.95-slim-bookworm AS build
WORKDIR /app
ENV CARGO_BUILD_JOBS=4
COPY Cargo.toml Cargo.lock build.rs ./
COPY proto ./proto
COPY migrations ./migrations
COPY src ./src
COPY tests ./tests
RUN cargo build --release --locked --bins
RUN cargo test --locked --no-run
FROM build AS test
CMD ["cargo", "test", "--locked", "--", "--include-ignored", "--test-threads=1", "--nocapture"]
FROM debian:bookworm-slim AS runtime
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl && rm -rf /var/lib/apt/lists/*
COPY --from=build /app/target/release/flight-service /app/target/release/booking-service /app/target/release/healthcheck /usr/local/bin/
USER 10001:10001
CMD ["booking-service"]
