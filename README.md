# Sentinel

小说审查工具包，纯 Rust 实现：全部 CLI 子命令由 `sentinel` 单二进制提供。

## 目录

```text
sentinel/
├── configs/
│   └── rules/review.yaml  # 统一规则配置（模板、词项、大纲/草稿检测）
├── src/                   # audit / stats / reports / consistency / tools / study
├── tests/                 # golden 逐字节对照 + 行为断言（fixtures/ 与 expected/）
├── Cargo.toml
└── justfile               # 构建与验证入口
```

## 快速使用（Rust）

```bash
cargo build --release                      # 编译（产物 target/release/sentinel）
cargo test  --all-targets --all-features   # 全量测试（含 golden 基线对照）

cargo run --bin sentinel -- rules                                     # 校验 review.yaml
cargo run --bin sentinel -- audit-draft path/to/ch01.md --format json # 草稿全量分析
cargo run --bin sentinel -- stats-draft path/to/story-dir --output-root /tmp/stats-out
```

仓库根可用 `just` 快捷配方（`build` / `clippy` / `fmt` / `test` / `smoke`）：

```bash
just test
just smoke audit-draft tests/fixtures/draft/standalone.md --format text
```

## 子命令全表

| 子命令 | 用途 |
|---|---|
| `rules` | 加载并校验 `review.yaml`，输出各节统计 |
| `audit-draft` | 草稿全量分析：规则指标、场面/对白/语料学习；`-o` 单文件/目录，`--learn-from`，`--fail-on-warn` |
| `audit-plan` | 大纲（arc/story/chapter）审查：结构漂移与字段误用；text/json/markdown |
| `audit-concept` | 概念卡审查：缺失字段与分类漂移 |
| `stats-draft` | 章节镜像统计树：章节报告 + 滚动窗口合并 + 逐目录 SUMMARY（`--output-root`） |
| `stats-plan` | 计划镜像统计：逐文件 `*-stats` 报告 + 逐目录 SUMMARY.md |
| `stats-concept` | 概念卡镜像统计：card-stats 树 + 逐目录 SUMMARY.md |
| `reports-scorecard` | 草稿章节评审记分卡：scorecards/*.md + 逐 story SUMMARY.md |
| `reports-catalog` | 跨 Story 模板/词项候选目录：template-catalog/{SUMMARY.md,CATALOG.json} |
| `reports-backlog` | 跨章模板积压：template-backlog/{SUMMARY.md,CANDIDATES.json} |
| `reports-kit` | 故事级评审套件：单章三类报告 + story 级 SUMMARY + review-kit/SUMMARY.md |
| `reports-learning` | 章节评审学习日志：learning/*.md + 逐 story SUMMARY.md |
| `reports-profiles` | 研究导向章节句子画像：profiles/*.md + 逐 story SUMMARY.md |
| `reports-workspace` | 整工作区看板：concept/plan/draft/consistency 四节 → 单份 AUDIT.md |
| `study-compare` | 两份 analysis JSON 的 summary 数值指标差值表 |
| `study-pov` | POV 漂移候选：确定性 JSON 输出（需人工复核，非结论） |
| `consistency` | SQLite/FTS5 一致性索引：构建、查询与 13 个子命令 |

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

## 测试与 golden 基线

- `tests/` 集成测试与 `tests/fixtures/expected/` 的 golden 基线逐字节对照
  （报告内嵌输入路径，测试侧先拷贝到临时根再按标记替换比对；`--format json`
  走零归一化深比较）；行为断言（如章末趋势合流、评审派单）直接打 Rust pub API。
- 基线在移植验收时由旧工具链字节定稿，此后以本 crate 实跑输出维护：
  行为有意变更时重跑对应子命令再生并随改动入库。

## 与 novel 项目的关系

- 本仓库**只**放审查工具，不放小说正文
- 调用时把小说目录当输入路径传入即可
- 不再使用 `review` 命名，避免与 novel 项目内部 `review/` 包冲突
