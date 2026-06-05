FROM rust:latest AS builder
WORKDIR /app

ENV SQLX_OFFLINE=true

COPY Cargo.toml Cargo.lock ./
COPY .sqlx/            .sqlx/
COPY windlass-types/   windlass-types/
COPY windlass-machine/ windlass-machine/
COPY windlass-observability/ windlass-observability/
COPY windlass-db-core/ windlass-db-core/
COPY windlass-disk-core/ windlass-disk-core/
COPY windlass-docker-core/ windlass-docker-core/
COPY windlass-mam-core/ windlass-mam-core/
COPY windlass-qbit-core/ windlass-qbit-core/
COPY windlass-vpn-core/ windlass-vpn-core/
COPY windlass-domain-core/ windlass-domain-core/
COPY windlass-local/   windlass-local/
COPY windlass-clients/ windlass-clients/
COPY windlass-db/      windlass-db/
COPY windlass-web/     windlass-web/
COPY windlass/         windlass/
COPY windlass-testkit/ windlass-testkit/

RUN cargo build --release -p windlass-testkit

FROM debian:bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ca-certificates curl \
    && rm -rf /var/lib/apt/lists/*

COPY --from=builder /app/target/release/windlass-testkit /usr/local/bin/windlass-testkit

ENTRYPOINT ["/usr/local/bin/windlass-testkit"]
