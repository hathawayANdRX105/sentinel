set dotenv-load := false

# Show active bd tasks.
bd:
    bd list --status open,in_progress --limit 20

# Build the Rust crate (all targets, all features).
build:
    cargo build --all-targets --all-features

# Clippy on all targets.
clippy:
    cargo clippy --all-targets --all-features

# Check formatting.
fmt:
    cargo fmt --all -- --check

# Run the full test suite (unit + integration, incl. golden baselines).
test:
    cargo test --all-targets --all-features

# Smoke the sentinel CLI. Usage: just smoke audit-draft <file> [args...]
smoke *args:
    cargo run --bin sentinel -- {{args}}
