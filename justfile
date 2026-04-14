default: test

# Build the React frontend (required before cargo build)
build-web:
    cd app && npm run build

# Run Vite dev server (proxies /api to localhost:5010)
dev-web:
    cd app && npm run dev

build:
    cargo build

test:
    cargo test

test-all:
    cargo test -- --include-ignored

integration:
    docker compose -f docker-compose.dev.yml up --build -d
    docker compose -f docker-compose.qbit-integration.yml up -d
    cargo test --test integration -- --ignored --test-threads=1 --nocapture; \
    cargo test -p windlass-local -- --include-ignored --test-threads=1 --nocapture; \
    cargo test --test qbit_integration -- --ignored --test-threads=1 --nocapture; \
    docker compose -f docker-compose.dev.yml down -v --remove-orphans; \
    docker compose -f docker-compose.qbit-integration.yml down -v --remove-orphans

# Run qBit-specific integration tests (requires docker-compose.qbit-integration.yml up)
integration-qbit:
    docker compose -f docker-compose.qbit-integration.yml up -d
    cargo test --test qbit_integration -- --ignored --test-threads=1 --nocapture; \
    docker compose -f docker-compose.qbit-integration.yml down -v --remove-orphans

# Bring up the dev/test stack
# Bring up the dev/test stack. Pass debug=true to start in debug mode.
# Usage: just stack-up          (normal mode)
#        just stack-up debug=true  (debug mode on from start)
stack-up debug="":
    #!/usr/bin/env bash
    set -euo pipefail
    export DEBUG_MODE_ON_START="{{debug}}"
    docker compose -f docker-compose.dev.yml up --build -d

# Tear down the dev/test stack
stack-down:
    docker compose -f docker-compose.dev.yml down -v --remove-orphans

# View live logs from the dev stack
stack-logs:
    docker compose -f docker-compose.dev.yml logs -f windlass

fmt:
    cargo fmt

fmt-check:
    cargo fmt -- --check

clippy:
    cargo clippy -- -W clippy::pedantic -W clippy::nursery

coverage:
    cargo tarpaulin \
        --exclude-files "mlm/*" "mousehole/*" \
        --exclude-files "windlass/src/main.rs" \
        --exclude-files "windlass/src/shell/*" \
        --exclude-files "windlass/tests/*" \
        --exclude-files "windlass-web/src/*" \
        --exclude-files "windlass-testkit/src/*" \
        --exclude-files "windlass-local/src/docker*.rs" \
        --ignore-tests

audit:
    cargo audit

outdated:
    cargo outdated --workspace

check: fmt-check clippy test
