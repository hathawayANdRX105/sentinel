# Sentinel

小说审查工具包（大纲 / 草稿 CLI）。从 novel 仓库的 review 工具拆出，独立演进。

## 目录

```text
sentinel/
├── configs/rules/review.yaml  # 统一规则（模板、词项、大纲/草稿检测）
├── src/
│   ├── audit/                 # 大纲/草稿审查
│   ├── stats/                 # 大纲/草稿统计
│   ├── reports/               # 审稿包、评分卡、学习日志、候选目录
│   ├── tools/apply.py         # 模板候选写回
│   ├── lib/                   # CLI、I/O、路径、规则加载
│   └── consistency.py         # 一致性索引
├── tests/
├── scripts/                   # 真实草稿 smoke 脚本
├── justfile
└── GUIDE.md                   # Agent 使用说明（唯一完整文档）
```

## 快速使用

```bash
cd ~/projects/sentinel
export PYTHONPATH=src

python3 -m audit.plan --input path/to/plan.md --output /tmp/plan.md
python3 -m audit.draft --input path/to/ch01.md --format markdown --output /tmp/draft.md
python3 -m stats.draft --input path/to/story-dir --output-root /tmp/stats-out
python3 -m stats.plan --input path/to/plans --output-root /tmp/plan-stats-out
```

完整输入/输出规则、smoke、禁止事项见根目录 [`GUIDE.md`](GUIDE.md)。

## 规则配置

所有检测规则集中在 `configs/rules/review.yaml`：

| 段 | 用途 |
|---|---|
| `draft.regex_rules` | 高频词、句式、标点 |
| `draft.template_rules` | 可维护模板库 |
| `draft.tracked_terms` | 跟踪词库 |
| `draft.ending_labels` | 章末收束类型 |
| `plan.required_headings` | 大纲必备标题 |
| `plan.function_rules` | 章节/Scene/章末功能标签 |

## 测试

```bash
cd ~/projects/sentinel
PYTHONPATH=src python3 -m unittest tests.test_rules_config tests.test_outputs tests.test_real_draft_smoke -v
# 或
just test
```

## 与 novel 项目的关系

- 本仓库**只**放审查工具，不放小说正文
- 调用时把小说目录当输入路径传入即可
- 不再使用 `review` 命名，避免与 novel 项目内部 `review/` 包冲突
