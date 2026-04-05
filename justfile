default: test

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

check: fmt-check clippy test
