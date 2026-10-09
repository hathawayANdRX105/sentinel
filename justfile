set dotenv-load := false

# Show active bd tasks.
bd:
    bd list --status open,in_progress --limit 20

# Build the Rust crate (all targets, all features).
build:
    cd rust && cargo build --all-targets --all-features

# Clippy on all targets.
clippy:
    cd rust && cargo clippy --all-targets --all-features

# Check formatting.
fmt:
    cd rust && cargo fmt --all -- --check

# Run the full test suite (unit + integration, incl. golden baselines).
test:
    cd rust && cargo test --all-targets --all-features

# Smoke the sentinel CLI. Usage: just smoke audit-draft <file> [args...]
smoke *args:
    cd rust && cargo run --bin sentinel --manifest-path Cargo.toml -- {{args}}
