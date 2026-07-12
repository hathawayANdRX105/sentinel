# Sentinel Agent Guide

## Scope

- This repository contains only the Sentinel review tools, not novel prose.
- Treat `novel-novel2` as external input data passed by CLI path.
- Keep the package under `src/`; do not reintroduce the old top-level `sentinel/` package.
- 跨 agent 使用说明：见根目录 [`GUIDE.md`](GUIDE.md)。

## Workflow

- Use `bd` for task state when `.beads/` is present.
- Keep review rules in `configs/rules/review.yaml`; avoid adding new hardcoded Chinese term banks in Python.
- Prefer conservative deletion: remove duplicate loaders or dead wrappers only when tests cover the behavior.

## Verification

Run focused checks from the repository root:

```bash
PYTHONPATH=src python3 -m unittest tests.test_rules_config -v
PYTHONPATH=src python3 -m unittest tests.test_outputs -v
PYTHONPATH=src python3 -m unittest tests.test_real_draft_smoke -v
```

CLI smoke examples:

```bash
PYTHONPATH=src python3 -m audit.plan --input path/to/plan.md --output /tmp/plan.md
PYTHONPATH=src python3 -m audit.draft --input path/to/ch01.md --format markdown --output /tmp/draft.md
PYTHONPATH=src python3 -m stats.plan --input path/to/plans --output-root /tmp/plan-stats-out
PYTHONPATH=src python3 -m stats.draft --input path/to/story-dir --output-root /tmp/draft-stats-out
just smoke-real-stats
just smoke-real-audit
```
