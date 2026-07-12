# Sentinel Agent Usage

Sentinel is a CLI-only review package. Pass an external novel workspace as input; do not copy novel content into this repository.

## Select the tool

| Need | Command |
|---|---|
| Check one plan | `python3 -m audit.plan --input path/to/plan.md --output /tmp/plan.md` |
| Check one draft | `python3 -m audit.draft --input path/to/ch01.md --format markdown --output /tmp/draft.md` |
| Generate plan statistics | `python3 -m stats.plan --input path/to/plans --output-root /tmp/plan-stats` |
| Generate draft statistics and rolling windows | `python3 -m stats.draft --input path/to/story-dir --output-root /tmp/draft-stats` |

Run from the repository root with `PYTHONPATH=src`, or use the `just` recipes.

Use `python3 -m <module> --help` to inspect the module’s complete options.

## Input and output rules

- Positional paths and repeated `--input` paths can be combined.
- **audit** (`audit.plan` / `audit.draft`): `-o/--output` is a **file or directory**. One input → one file; multiple inputs with `text`/`markdown` → treat `--output` as a directory and write one report per source. **`--format json` always writes one combined JSON array to the given path** (not a directory of files). There is **no** `--output-root`. Warnings still exit 0 unless you pass `--fail-on-warn`.
- **stats** (`stats.plan` / `stats.draft`): `-o/--output` is allowed only when exactly one chapter/file is collected; use `--output-root` for directories or multiple files (mirrored `*-stats` tree + `SUMMARY.md`).
- Draft corpus learning scans nearby `concept/`, plan, and draft directories. Use `--no-corpus-learning` for a fast, deterministic smoke check; it disables only learned filters, not YAML rules.
- `stats.draft --window-sizes 2 3` is the default rolling pair/triple report set. Pass different sizes to change it; omit values after `--window-sizes` to generate only chapter reports and `SUMMARY.md`.

## Rule changes

Edit `configs/rules/review.yaml`, not Python term banks:

- `draft.regex_rules`, `draft.template_rules`, `draft.tracked_terms`, and `draft.ending_labels` control draft checks.
- `plan.required_headings`, `plan.function_rules`, `plan.regex`, and `plan.thresholds` control plan checks.

Run `PYTHONPATH=src python3 -m unittest tests.test_rules_config -v` after any rule change.

## Reproducible smoke output

The repository does not version novel prose or generated reports. Run the following commands against an external workspace and write to `/tmp`:

```bash
PYTHONPATH=src python3 -m audit.plan --input /path/to/chapter-plan.md --output /tmp/sentinel-plan-audit.md
PYTHONPATH=src python3 -m audit.draft --input /path/to/ch01.md --format markdown --output /tmp/sentinel-draft-audit.md --no-corpus-learning
PYTHONPATH=src python3 -m stats.plan --input /path/to/plans --output-root /tmp/sentinel-plan-stats
PYTHONPATH=src python3 -m stats.draft --input /path/to/story-dir --output-root /tmp/sentinel-draft-stats --no-corpus-learning
```

The audit files contain one report per input; `stats` writes mirrored chapter files plus a `SUMMARY.md` per source directory.

## 真实草稿 smoke

默认使用外部真实章节（不进仓库）：

- 故事目录：`~/projects/novel/novel1/drafts/story-3-foreign-whispers`
- **just / scripts 覆盖**：位置参数  
  `just smoke-real-stats /abs/path/to/story`  
  `just smoke-real-audit /abs/path/to/ch01.md`  
  或 `python3 scripts/smoke_real_stats.py /abs/path/to/story`
- **unittest 覆盖**：仅 `tests.test_real_draft_smoke` 读  
  `SENTINEL_SMOKE_DRAFT_DIR=/abs/path/to/story`  
  （`just smoke-real-*` **不**读该环境变量）
- 输出只写 `/tmp`，不提交报告文件

```bash
# 统计 smoke（SUMMARY + 章节/pairs 报告 + JSON 指标）
just smoke-real-stats
just smoke-real-stats $HOME/projects/novel/novel1/drafts/story-2-undercurrent

# 单章 audit smoke
just smoke-real-audit
just smoke-real-audit $HOME/projects/novel/novel1/drafts/story-3-foreign-whispers/ch01.md

# 可编程 smoke（缺外部数据时 Skip）
PYTHONPATH=src python3 -m unittest tests.test_real_draft_smoke -v
# 或：SENTINEL_SMOKE_DRAFT_DIR=/abs/path/to/story PYTHONPATH=src python3 -m unittest tests.test_real_draft_smoke -v

# 底层脚本（just 包装同命令）
python3 scripts/smoke_real_stats.py
python3 scripts/smoke_real_audit.py
```
