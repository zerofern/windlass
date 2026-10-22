# Test runner for the wg integration suite: the Rust toolchain plus
# the system tools TunnelShell shells out to (`ip`, `wg`, `nft`).
# The repo is bind-mounted at /work by docker-compose.wg-integration.yml;
# cargo registry + target live in named volumes so re-runs are
# incremental.
FROM rust:bookworm
RUN apt-get update && apt-get install -y --no-install-recommends \
    iproute2 wireguard-tools nftables \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /work
