workflow_checker := env("WORKFLOW_DOC_CHECKER", env("HOME") + "/.agents/skills/workflow-document-check/scripts/check_workflow_docs.py")

default: check

build:
    cargo build --locked --all-targets --all-features

fmt:
    cargo fmt --all

fmt-check:
    cargo fmt --all -- --check

clippy:
    cargo clippy --locked --all-targets --all-features -- -D warnings

test:
    cargo test --locked --all-targets --all-features
    cargo test --locked --doc --all-features

integration:
    cargo test --locked --all-features --test runner --test shell --test cli --test web

run *args:
    cargo run --locked -- {{args}}

prose:
    prose-check README.md
    rg --files --hidden .workflow -g '*.md' -0 | xargs -0 prose-check

workflow-check:
    python "{{workflow_checker}}" .

check: build fmt-check clippy test prose workflow-check

coverage:
    cargo llvm-cov --locked --all-targets --all-features --summary-only
