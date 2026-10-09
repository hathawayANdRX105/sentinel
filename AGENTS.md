# Sentinel Agent Guide

## Scope

- This repository contains only the Sentinel review tools (Rust, crate at `rust/`), not novel prose.
- External novel directories are passed by CLI path only.
- Rules live in `configs/rules/review.yaml`; do not add new hardcoded Chinese term banks in Rust.
- Golden baselines under `rust/tests/fixtures/expected/` are byte contracts; see
  `rust/PORTING_NOTES.md` before changing any output format.

## Workflow

- Use `bd` for task state when `.beads/` is present.
- Prefer conservative deletion: remove duplicate loaders or dead wrappers only when tests cover the behavior.

## Verification

From the repository root:

```bash
just test    # cargo test --all-targets --all-features (run in rust/)
just clippy  # cargo clippy --all-targets --all-features, zero warnings required
just fmt
```

CLI smoke examples (binary: `cargo run -q --bin sentinel -- ...` in `rust/`):

```bash
sentinel audit-plan path/to/plan.md --output /tmp/plan.md
sentinel audit-draft path/to/ch01.md --format markdown --output /tmp/draft.md
sentinel stats-plan path/to/plans --output-root /tmp/plan-stats-out
sentinel stats-draft path/to/story-dir --output-root /tmp/draft-stats-out
sentinel consistency build path/to/novel1
```
