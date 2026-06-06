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
    DATABASE_URL=postgres://windlass:windlass@localhost:15432/windlass cargo test

test-all:
    cargo test -- --include-ignored

integration:
    # §34 PR 4 of 6 onwards: runs the contract-verification suite
    # (`integration_contracts`), the support-helper smoke tests
    # (`integration_support`), the windlass-local docker tests, and the
    # standalone qbit_integration suite.
    set -e; \
    cleanup() { docker compose -f docker-compose.dev.yml down -v --remove-orphans; docker compose -f docker-compose.qbit-integration.yml down -v --remove-orphans; }; \
    trap cleanup EXIT; \
    docker compose -f docker-compose.dev.yml up --build -d; \
    docker compose -f docker-compose.qbit-integration.yml up --build --wait -d; \
    cargo test -p windlass-local -- --include-ignored --test-threads=1 --nocapture; \
    cargo test --test integration_support -- --ignored --test-threads=1 --nocapture; \
    cargo test --test integration_contracts -- --ignored --test-threads=1 --nocapture; \
    cargo test --test qbit_integration -- --ignored --test-threads=1 --nocapture

# Run qBit-specific integration tests (requires docker-compose.qbit-integration.yml up)
integration-qbit:
    set -e; \
    cleanup() { docker compose -f docker-compose.qbit-integration.yml down -v --remove-orphans; }; \
    trap cleanup EXIT; \
    docker compose -f docker-compose.qbit-integration.yml up --build --wait -d; \
    cargo test --test qbit_integration -- --ignored --test-threads=1 --nocapture

# Bring up only Postgres for DB development.
db-up:
    docker compose -f docker-compose.dev.yml up --wait -d postgres

# Apply the Postgres schema used for SQLx compile-time checking.
db-migrate:
    docker compose -f docker-compose.dev.yml exec -T postgres psql -U windlass -d windlass -f /migrations/0001_initial.sql

# Tear down the dev stack and remove the Postgres volume.
db-down:
    docker compose -f docker-compose.dev.yml down -v --remove-orphans

# Print the local Postgres URL used by SQLx tooling.
db-url:
    @echo postgres://windlass:windlass@localhost:15432/windlass

# Refresh SQLx offline metadata after Postgres migrations are active.
sqlx-prepare:
    DATABASE_URL=postgres://windlass:windlass@localhost:15432/windlass cargo sqlx prepare --workspace

# Bring up the dev/test stack
# Bring up the dev/test stack (normal mode — safe for integration tests)
stack-up:
    docker compose -f docker-compose.dev.yml up --build -d

# Bring up the dev stack with all observability cores pre-paused (PAUSE_ON_START=true)
stack-up-paused:
    docker compose -f docker-compose.dev.yml -f docker-compose.paused.yml up --build -d

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
    DATABASE_URL=postgres://windlass:windlass@localhost:15432/windlass cargo clippy -- -W clippy::pedantic -W clippy::nursery

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

check: db-up db-migrate fmt-check clippy test
