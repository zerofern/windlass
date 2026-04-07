FROM rust:latest AS builder
WORKDIR /app

COPY Cargo.toml Cargo.lock ./
COPY windlass-types/   windlass-types/
COPY windlass-core/    windlass-core/
COPY windlass-debug/   windlass-debug/
COPY windlass-local/   windlass-local/
COPY windlass-clients/ windlass-clients/
COPY windlass-web/     windlass-web/
COPY windlass/         windlass/
COPY windlass-testkit/ windlass-testkit/

RUN cargo build --release -p windlass-testkit

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/windlass-testkit /usr/local/bin/windlass-testkit

ENTRYPOINT ["/usr/local/bin/windlass-testkit"]
