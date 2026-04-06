default: test

# Build the React frontend (required before cargo build)
build-web:
    cd web && npm run build

# Run Vite dev server (proxies /api to localhost:5010)
dev-web:
    cd web && npm run dev

build:
    cargo build

test:
    cargo test

test-all:
    cargo test -- --include-ignored

integration:
    ./tests/integration/run.sh

fmt:
    cargo fmt

fmt-check:
    cargo fmt -- --check

clippy:
    cargo clippy -- -W clippy::pedantic -W clippy::nursery

coverage:
    cargo tarpaulin --exclude-files "mlm/*" "mousehole/*" --ignore-tests

audit:
    cargo audit

outdated:
    cargo outdated --workspace

check: fmt-check clippy test
