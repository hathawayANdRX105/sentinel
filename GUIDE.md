# Sentinel 使用指南（给 Agent）

给其他 coding agent 的最短调用说明。细节与英文约束见 [`docs/agent-usage.md`](docs/agent-usage.md)。

## 1. 这是什么

Sentinel 是**小说大纲 / 草稿的本地 CLI 审查与统计工具**，不含小说正文。

- 输入：外部路径（novel 仓库、任意章节 md）
- 输出：审查报告或统计镜像树
- 禁止：把 novel 正文 copy / commit 进本仓库

## 2. 运行前

```bash
cd ~/projects/sentinel
export PYTHONPATH=src
```

- 需要：本机 `python3` + `PyYAML`
- 无 console script，用 `python3 -m ...` 或 `just ...`

## 3. 先选命令

| 你要做的事 | 命令 |
|---|---|
| 查一份大纲是否结构漂移 | `python3 -m audit.plan --input <plan.md> --output /tmp/plan.md` |
| 查一章草稿重复/模板/疲劳 | `python3 -m audit.draft --input <ch.md> --format markdown --output /tmp/draft.md` |
| 给大纲目录出统计报告 | `python3 -m stats.plan --input <plans-dir> --output-root /tmp/plan-stats` |
| 给故事草稿目录出单章+滚动窗口统计 | `python3 -m stats.draft --input <story-dir> --output-root /tmp/draft-stats` |
| 真实草稿一键 smoke（统计 JSON） | `just smoke-real-stats` |
| 真实单章 audit smoke | `just smoke-real-audit` |

## 4. 输入 / 输出硬规则

- 位置参数与重复 `-i/--input` 可混用。
- **audit**（`audit.plan` / `audit.draft`）：`-o/--output` 是**文件或目录**。单输入写单文件；多输入且 `text`/`markdown` 时把 `--output` 当目录，每源一份报告。**`--format json` 始终把合并后的 JSON 数组写到该路径（不是目录）**。**没有** `--output-root`。警告默认 exit 0；自动化失败加 `--fail-on-warn`。
- **stats**（`stats.plan` / `stats.draft`）：`-o/--output` 仅当收集到**恰好 1 个**目标文件；多文件/目录用 `--output-root`（镜像 `*-stats` 树 + `SUMMARY.md`）。
- 草稿默认会做语料学习（扫同 novel 下 concept/plan/draft）。快速确定性跑加 `--no-corpus-learning`（只关学习过滤，YAML 规则仍生效）。
- `stats.draft --window-sizes` 默认 `2 3`（pairs + triples）。`just smoke-real-stats` 为加速固定为 window `2`。
- **生成物写 `/tmp` 或调用方指定目录；不要把报告提交进 git。**

## 5. 最小可复制示例

```bash
cd ~/projects/sentinel
export PYTHONPATH=src

# 大纲审查
python3 -m audit.plan --input /path/to/chapter-plan.md --output /tmp/sentinel-plan.md

# 草稿审查（推荐 markdown + 无语料学习做 smoke）
python3 -m audit.draft \
  --input /path/to/ch01.md \
  --format markdown \
  --output /tmp/sentinel-draft.md \
  --no-corpus-learning

# 草稿统计（单章报告 + SUMMARY + 滚动窗口）
python3 -m stats.draft \
  --input /path/to/story-dir \
  --output-root /tmp/sentinel-draft-stats \
  --no-corpus-learning

# 大纲统计
python3 -m stats.plan \
  --input /path/to/plans \
  --output-root /tmp/sentinel-plan-stats

# 本机真实草稿（若存在）
just smoke-real-stats
just smoke-real-audit
```

本机真实草稿默认目录（与 smoke 一致）：

```text
$HOME/projects/novel/novel1/drafts/story-3-foreign-whispers
```

- **just / scripts 覆盖路径**：位置参数  
  `just smoke-real-stats /abs/path/to/story`  
  `just smoke-real-audit /abs/path/to/ch01.md`
- **unittest 覆盖**：仅 `tests.test_real_draft_smoke` 读  
  `SENTINEL_SMOKE_DRAFT_DIR=/abs/path/to/story`  
  （`just smoke-real-*` **不**读该环境变量）
- 目录不存在时 smoke / 相关测试会 Skip，不算工具损坏

## 6. 规则改哪里

只改：

```text
configs/rules/review.yaml
```

| 段 | 用途 |
|---|---|
| `draft.regex_rules` | 高频词、句式、标点 |
| `draft.template_rules` | 可维护模板库 |
| `draft.tracked_terms` | 跟踪词库 |
| `draft.ending_labels` | 章末收束类型 |
| `plan.required_headings` | 大纲必备标题 |
| `plan.function_rules` | 章节 / Scene / 章末功能标签 |
| `plan.regex` / `plan.thresholds` | 大纲正则与阈值 |

改完跑：

```bash
PYTHONPATH=src python3 -m unittest tests.test_rules_config -v
```

不要在 Python 里新硬编码中文词库。

## 7. 看结果时看什么

### `audit.plan`

- markdown：`type` / `status` / `warnings` 列表
- 有警告时 status=`WARN`

### `audit.draft`（`--format markdown`）

- `## 概览`
- `总体状态`、`警告分区数`、字数/句数
- 审查提醒、句式疲劳、硬标志等分区

### `stats.plan` / `stats.draft`

- `--output-root` 下镜像 `*-stats` 树
- 每目录 `SUMMARY.md`，常见段：
  - `## Chapters`
  - `## Story-Wide Hard Flags`（draft）
  - `## Story-Wide Style Fatigue`（draft）
  - `## Story-Wide Ending Functions`（draft）
  - `## Priority`
- draft 默认还有 `pairs/`、`triples/` 滚动窗口报告

## 8. 自检

```bash
cd ~/projects/sentinel
export PYTHONPATH=src

# 全量聚焦测试
python3 -m unittest tests.test_rules_config tests.test_outputs tests.test_real_draft_smoke -v
# 或
just test

# 命令是否可用
python3 -m audit.draft --help
python3 -m stats.draft --help
```

- 缺外部 novel 时：`tests.test_outputs` / `tests.test_real_draft_smoke` 可能 Skip
- 真实一键：`just smoke-real-stats`、`just smoke-real-audit`

## 9. 禁止事项

- 不把小说正文、生成 `SUMMARY`、examples 报告提交进本仓库
- 不重新引入顶层 `sentinel/` 包，避免与 novel 内 `review/` 命名冲突
- 不在 Python 里堆中文词库；词库进 `configs/rules/review.yaml`
- 不要对 novel 树跑「无 `--output-root`」的 stats（会写镜像 `*-stats` 进 novel 侧）；示例一律 `/tmp`
