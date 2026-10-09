# Porting Notes

移植过程中的裁决记录：参考实现（worktree `src/`）与 Rust 面之间的差异裁决与证据。

## audit-draft：`--diff` / JSONL / `--learning-source` 未实现（N/A）

历史任务书中"audit-draft CLI 面补全（--diff 变更检测 / JSONL 输出 / --learning-source）"一项
描述过时，Python 参考不存在这些语义，未实现：

- `src/audit/draft.py` `parse_args`（:4646-4668）的全部参数为
  `paths / -i / --sample-limit / --fail-on-warn / --format / -o / --learn-from / --no-corpus-learning`，
  无 `--diff`、无 `--learning-source`。
- 全仓 `grep -rniE '\bdiff' src/ --include='*.py'` 零命中（排除 `differ` 等词形后为空）。
- "JSONL 输出"为旧描述：参考实现输出 JSON 数组（`json.dumps(reports, indent=2)`，
  `-o` 时文件尾带 `\n`），Rust `serde_json::to_string_pretty` + 尾换行写盘已对齐
  （`tests/audit_draft_json.rs` 严格深度相等 + 类型位校验）。

## reports-scorecard：退出与打印归位 CLI 面

Python `reports/scorecard.py main()` 用 `raise SystemExit("No draft chapter files found.")`
（stderr + 退出码 1）并在每步 `print(path)`。Rust 面裁决：
`sentinel::reports::scorecard::run(&ScorecardOptions) -> Result<(i32, Vec<PathBuf>)>`
——库侧不直接 `process::exit`/打印；空输入 `eprintln!` 同文案 + 返回 `(1, 空)`；
`(0, 打印序列)` 由 `main.rs` 逐行 `println!` 后按 rc 退出。CLI 端到端
（stdout 字节 + 退出码 + 空输入 stderr 文案）由 `tests/reports_scorecard.rs`
`scorecard_cli_round` 守护。
## Counter JSON 形状

Python `dict(counter)` 序列化为 JSON **对象**（插入序）。Rust 用新类型
`audit::draft::CountMap`（保序 serialize_map）而非 pair 列表，
对照测试为**零归一化**的严格深度相等（`tests/audit_draft_json.rs`）。

## tools-apply：f-string 标量渲染差异（bool / 科学计数 / repr vs JSON）

Python `render_dry_run` 用 f-string 插值，非 `str` 标量按 `str(value)` 渲染；
Rust `src/tools/apply.rs` `json_scalar`（:108）按 `Display`/`serde_json` 文本渲染。
三类已知逐字偏差（仅当 catalog 的 `stories`/`count`/`reason` 等字段为非字符串值时出现）：

- bool：Python `str(True)` → `True`；Rust `bool::to_string()` → `true`。
- 浮点科学计数：Python `str(1e30)` → `1e+30`；Rust `f64 Display` → `1e30`。
- 容器：Python f-string 对 dict/list 用 **repr**（单引号、`None`）；
  Rust `Value::to_string()` 输出 **JSON 文本**（双引号、`null`）。

裁决：接受为已知偏差。对照轮（deviation rounds）确认三处偏差均只出现在
构造的非字符串标量输入；固化基线（`tests/fixtures/apply-inputs/{c1.yaml,c2.json}`）
字段全为字符串/整数，dry-run 与回写基线零 diff。不为此引入 Python repr 仿真。

## tools-apply：catalog 缺失键容错

Python `build_plan` 取 `item["action"]`/`item["state"]` 等键缺失时 KeyError
（未捕获 → traceback + 退出码 1）；Rust `json_field`/`json_top`（:119/:127）
对缺键给默认值（空串 / `"unknown"` / `0`）继续渲染。裁决：Rust 面更宽容，
行为面（rc/dry-run 文本）在合法 catalog 上逐字一致；缺键输入只作为
容错设计记录，不做对照基线。

## tools-apply：E2「Invalid catalog」stderr 引擎细节差异

yaml 解析失败的错误消息前缀（`Invalid catalog: <path>`）逐字一致；
失败详情一行是引擎文本（Python `yaml.YAMLError` 描述 vs Rust `serde_yaml`
解析错误描述），引擎细节不保证逐字。对照轮 E2 已人工 cmp：前缀逐字、
详情行已知差异。裁决：接受，不做 YAML 引擎错误文本仿真。

## tools-apply：yaml 回写序列化裁决

Python `_write_rules_yaml` 用 `yaml.safe_dump(allow_unicode=True, sort_keys=False)`
全量覆写；Rust `write_rules_yaml` 用 `serde_yaml::to_string(YamlValue)`（键序保留）。
两者对仓库真实 `configs/rules/review.yaml` 做「读改写 + 全量序列化」比对
字节级 IDENTICAL（对照轮 `/tmp/yamlcmp`）。固化基线
`tests/fixtures/expected/reports-workspace-apply/apply-full/post-apply.yaml`
（Python apply 后字节）与 Rust apply 后字节零 diff（`apply_round_writeback_full`）。
裁决：serde_yaml 直接等价，无需 PyYAML dump 仿真。

## stats concept/plan：`stats_path_for` 绝对路径双斜杠修复

初版移植把 `Path.components()` 逐段转字符串后 `join("/")` 重建路径；
Python 参考是 `Path(*parts)`（`Path.parts` 的根组件为单个 `"/"`）。
相对路径输入下两者字节相同（各对照轮均相对路径，未暴露），绝对路径输入
时 `join` 产生 `//tmp/...` 双斜杠（SUMMARY.md 内容路径、AUDIT.md 展示行）。
修复：`src/stats/concept.rs` `stats_path_for` 与 `src/stats/plan.rs`
`stats_path_for` 改用 `src/stats/draft.rs` `path_from_parts`（已 `pub`，
逐段 `PathBuf::push`，根组件还原单 `/`）。`tests/reports_workspace_apply.rs`
以 tempdir **绝对路径**输入运行，回归锁定此修复。

## 迁移收尾：指引命令文案切换（有意的参考偏离）

移植完成后，输出中给用户的指引命令从 Python 形态改为 Rust CLI 形态
（迁移前 Python 参考的字节对齐已完成使命，此处为**有意偏离**，不再回跟）：

- `consistency` 的 `CONSISTENCY_CLI` 常量与 review-queue/feedback-summary 指引：
  `python3 -m consistency ...` → `sentinel consistency ...`。
- `reports.workspace` Suggested Order 中 `consistency_index.py suspects`
  → `sentinel consistency suspects`。
- 受影响 golden 基线（consistency/learning/workspace 输出树）同步文本替换，
  全量测试锁定新文案。

## golden 基线再生工具

`rust/tests/*/` 头注释中的 `PYTHONPATH=src python3 -m ...` 生成命令为移植期历史事实；
Python 参考实现已从分支删除（commit `feat: ...` 之前的历史），如需再生基线，
从 git 历史检出旧版 `src/` 于同布局下运行。

## study-pov：py 导入损坏与 `schema_version` 意图值

`src/study/pov.py` 顶部 `from audit.draft import ANALYSIS_SCHEMA_VERSION, split_paragraph_infos, split_sentences`
三个符号：
- `ANALYSIS_SCHEMA_VERSION`：主仓 `src/audit/draft.py` grep 零命中——**常量不存在**，py 侧 ImportError 必崩；
- `split_paragraph_infos`：存在（`draft.py:347`），但被 ImportError 遮蔽，无法实际调用；
- `split_sentences`：存在（`draft.py:313`），同上。

py `test_study.py::test_pov_schema_version`（期望空文本 AssertionError）是红测试
（py 侧 ImportError 根本跑不到），未移植。

Rust 面裁决：`study::pov::SCHEMA_VERSION = 1`（意图值；py 侧无真值可对齐）。
`tests/study_tools.rs` 中 `pov_schema_version_is_one` 固化此值，并注明 py 参考损坏。

## study-pov：density 恒 0（py 侧 ponytail 注释承认）

py `pov.py:103-112`（ponytail 注释）：
```python
# ponytail: pronoun/name/attribution fields not populated by audit.draft ParagraphInfo;
# all densities will be zero. Add when audit pipeline enriches paragraph info.
"pronouns": [],
"personal_names": [],
"dialogue_attribution": 0,
```
`paragraph_info.get("pronouns", [])` 永远为空 → 三个密度函数均返回元组
`(0, 0.0)`。Rust 面 `PoVObservation` 三字段类型 `(i64, f64)`（JSON 渲染
`[0, 0.0]`，count 为 int、density 为 float，与 py 元组 JSON 形状逐位一致）。

## study-pov：段落切分 `markdown_noise` 仅在句级生效

Rust `TextSplitter::split_paragraph_infos` 与 py 同名函数一样
**不**过滤 `markdown_noise_line` 匹配行（`#` 标题行计为普通段），
`markdown_noise` 只在 `split_sentence_infos` 里起作用。
py/rust 在含 `#` 标题的章节上段落切分结果一致（标题计为第 1 段）。
